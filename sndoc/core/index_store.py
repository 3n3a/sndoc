"""The hybrid search index: SQLite with an FTS5 (BM25) table over chunk text and
a sqlite-vec vec0 virtual table over chunk embeddings. The indexer builds it;
the query side fuses the two arms with Reciprocal Rank Fusion so exact-term
queries (FTS) and conceptual queries (vector) both rank well.

This module is the native-deps choke point: `_connect()` is the single place
that decides which sqlite3 build to use. The stdlib `sqlite3` on some platforms
is compiled without loadable-extension support (so `sqlite_vec.load()` can't
run); `pysqlite3-binary` ships a SQLite with extensions + FTS5 enabled, so we
prefer it and fall back to the stdlib only if it's absent.
"""

from __future__ import annotations

import os
import re

import sqlite_vec

from .constants import EMBED_DIM
from .embed import to_vec_blob
from .models import IndexChunk, SearchHit

RRF_K = 60


def _import_sqlite():
    """Prefer pysqlite3 (loadable extensions + FTS5 guaranteed); fall back to
    the stdlib."""
    try:
        import pysqlite3.dbapi2 as sqlite3  # type: ignore

        return sqlite3
    except ImportError:
        import sqlite3  # type: ignore

        return sqlite3


sqlite3 = _import_sqlite()


def _connect(file: str, *, readonly: bool) -> "sqlite3.Connection":
    if readonly:
        conn = sqlite3.connect(f"file:{file}?mode=ro", uri=True)
    else:
        conn = sqlite3.connect(file)
    conn.enable_load_extension(True)
    sqlite_vec.load(conn)
    conn.enable_load_extension(False)
    return conn


def _to_fts_expr(query: str) -> str:
    """Build a safe FTS5 MATCH expression: OR the alphanumeric query terms so
    partial matches still contribute to BM25 ranking."""
    terms: list[str] = []
    seen: set[str] = set()
    for t in re.findall(r"[a-z0-9]+", query.lower()):
        if len(t) >= 2 and t not in seen:
            seen.add(t)
            terms.append(t)
    if not terms:
        return ""
    return " OR ".join(f'"{t}"' for t in terms)


class IndexStore:
    def __init__(self, conn: "sqlite3.Connection") -> None:
        self._db = conn

    @classmethod
    def open(cls, file: str) -> "IndexStore":
        """Open an existing index for queries (read-only)."""
        return cls(_connect(file, readonly=True))

    @classmethod
    def create(cls, file: str) -> "IndexStore":
        """Create a fresh index file with the schema (overwrites any existing)."""
        if os.path.exists(file):
            os.remove(file)
        conn = _connect(file, readonly=False)
        conn.execute("PRAGMA journal_mode = WAL")
        conn.executescript(
            f"""
            CREATE TABLE chunks (
                id           INTEGER PRIMARY KEY,
                path         TEXT NOT NULL,
                anchor       TEXT NOT NULL,
                title        TEXT NOT NULL,
                breadcrumb   TEXT NOT NULL,
                heading      TEXT NOT NULL,
                release      TEXT NOT NULL,
                content      TEXT NOT NULL,
                canonical_url TEXT NOT NULL,
                last_updated TEXT NOT NULL
            );
            CREATE VIRTUAL TABLE chunks_fts USING fts5(
                content, title, breadcrumb,
                content='chunks', content_rowid='id', tokenize='porter unicode61'
            );
            CREATE VIRTUAL TABLE vec_chunks USING vec0(embedding float[{EMBED_DIM}]);
            """
        )
        return cls(conn)

    def insert_chunks(
        self, chunks: list[IndexChunk], embeddings: list
    ) -> None:
        """Insert a batch of chunks with their embeddings (build phase)."""
        cur = self._db.cursor()
        for c, vec in zip(chunks, embeddings):
            cur.execute(
                """
                INSERT INTO chunks
                  (path, anchor, title, breadcrumb, heading, release, content,
                   canonical_url, last_updated)
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                """,
                (
                    c.path, c.anchor, c.title, c.breadcrumb, c.heading,
                    c.release, c.content, c.canonical_url, c.last_updated,
                ),
            )
            cur.execute(
                "INSERT INTO vec_chunks (rowid, embedding) VALUES (?, ?)",
                (cur.lastrowid, to_vec_blob(vec)),
            )
        self._db.commit()

    def finalize(self) -> None:
        """Populate the FTS index from chunks and compact the file. Call once
        after all inserts."""
        self._db.executescript(
            """
            INSERT INTO chunks_fts (rowid, content, title, breadcrumb)
              SELECT id, content, title, breadcrumb FROM chunks;
            INSERT INTO chunks_fts (chunks_fts) VALUES ('optimize');
            """
        )
        self._db.commit()
        self._db.execute("VACUUM")
        self._db.commit()
        # Fold the WAL back into the main db and drop the -wal/-shm sidecars so
        # the single latest.db file is self-contained for blob upload (the
        # indexer uploads only latest.db).
        self._db.execute("PRAGMA wal_checkpoint(TRUNCATE)")
        self._db.execute("PRAGMA journal_mode=DELETE")
        self._db.commit()

    def query(self, query_text: str, query_vec, n: int) -> list[SearchHit]:
        """Hybrid search: BM25 + vector KNN fused by RRF, deduped to the best
        chunk per file. Returns up to `n` hits."""
        pool = max(n * 4, 40)

        # --- BM25 arm ---
        fts_expr = _to_fts_expr(query_text)
        fts_ids: list[int] = []
        if fts_expr:
            rows = self._db.execute(
                "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ? "
                "ORDER BY rank LIMIT ?",
                (fts_expr, pool),
            ).fetchall()
            fts_ids = [r[0] for r in rows]

        # --- vector arm ---
        vec_rows = self._db.execute(
            "SELECT rowid FROM vec_chunks WHERE embedding MATCH ? "
            "ORDER BY distance LIMIT ?",
            (to_vec_blob(query_vec), pool),
        ).fetchall()
        vec_ids = [r[0] for r in vec_rows]

        # --- RRF fusion ---
        scores: dict[int, float] = {}

        def add_ranks(ids: list[int]) -> None:
            for rank, rid in enumerate(ids):
                scores[rid] = scores.get(rid, 0.0) + 1.0 / (RRF_K + rank + 1)

        add_ranks(fts_ids)
        add_ranks(vec_ids)
        if not scores:
            return []

        ranked = sorted(scores.items(), key=lambda kv: kv[1], reverse=True)

        seen_paths: set[str] = set()
        hits: list[SearchHit] = []
        for rid, score in ranked:
            row = self._db.execute(
                "SELECT * FROM chunks WHERE id = ?", (rid,)
            ).fetchone()
            if row is None:
                continue
            path = row[1]
            if path in seen_paths:  # best chunk per file only
                continue
            seen_paths.add(path)
            hits.append(_row_to_hit(row, score))
            if len(hits) >= n:
                break
        return hits

    def manifest_dim_ok(self, embed_dim: int) -> bool:
        return embed_dim == EMBED_DIM

    def close(self) -> None:
        self._db.close()


def _row_to_hit(row, score: float) -> SearchHit:
    # Column order matches the chunks table.
    (_id, path, anchor, title, breadcrumb, _heading, release,
     content, canonical_url, _last_updated) = row
    # Local import to avoid a circular import at module load.
    from .repo import docs_url_for_path

    base = canonical_url or docs_url_for_path(path)
    url = f"{base}#{anchor}" if anchor else base
    return SearchHit(
        path=path,
        title=title or _heading or path,
        breadcrumb=breadcrumb,
        anchor=anchor,
        release=release,
        url=url,
        snippet=re.sub(r"\s+", " ", content)[:240].strip(),
        score=score,
    )
