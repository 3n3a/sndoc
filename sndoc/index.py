"""Build the hybrid search index from the local ServiceNowDocs clone.

Walk `markdown/**` on the checked-out branch, chunk + embed each file in
batches, build SQLite (FTS5 + sqlite-vec), then write the db + manifest into the
data dir. Called by `sndoc index` and by the auto-update lifecycle when the
latest branch's commit has changed.
"""

from __future__ import annotations

import dataclasses
import json
import sys
from datetime import datetime, timezone
from pathlib import Path

from .core.chunk import chunk_doc, chunk_embed_text
from .core.constants import (
    EMBED_DIM,
    EMBED_MODEL,
    MARKDOWN_PREFIX,
    index_db_path,
    manifest_path,
    repo_dir,
)
from .core.embed import embed_passages
from .core.index_store import IndexStore
from .core.models import IndexChunk, IndexManifest
from .core import repo

BATCH = 32


def _log(msg: str) -> None:
    print(msg, file=sys.stderr)


def _list_markdown_files(root: Path) -> list[str]:
    if not root.exists():
        return []
    return sorted(
        str(p.relative_to(root)) for p in root.rglob("*.md") if p.is_file()
    )


def build_index(
    branch: str | None = None,
    *,
    subdir: str | None = None,
    max_files: int | None = None,
) -> IndexManifest:
    """Build the index from the local clone and write db + manifest. Returns the
    manifest. Assumes the repo is already cloned."""
    branch = (branch or repo.resolve_latest_branch()).lower()
    _log(f"[index] branch: {branch}")
    repo.checkout_latest(branch)
    commit = repo.head_commit()

    md_prefix_root = repo_dir() / MARKDOWN_PREFIX
    md_root = md_prefix_root / (subdir or "")
    files = _list_markdown_files(md_root)
    if max_files:
        files = files[:max_files]
    _log(f"[index] {len(files)} markdown files")
    if not files:
        raise RuntimeError(f"No markdown files under {md_root}")

    # Chunk every file first (cheap), then embed + insert in batches (the cost).
    all_chunks: list[IndexChunk] = []
    for rel in files:
        full = md_root / rel
        repo_path = str(full.relative_to(md_prefix_root)).replace("\\", "/")
        content = full.read_text(encoding="utf-8")
        all_chunks.extend(chunk_doc(content, repo_path, branch))
    _log(f"[index] {len(all_chunks)} chunks; embedding...")

    db_path = index_db_path()
    db_path.parent.mkdir(parents=True, exist_ok=True)
    store = IndexStore.create(str(db_path))
    done = 0
    for i in range(0, len(all_chunks), BATCH):
        batch = all_chunks[i : i + BATCH]
        vecs = embed_passages([chunk_embed_text(c) for c in batch])
        store.insert_chunks(batch, vecs)
        done += len(batch)
        if done % (BATCH * 20) == 0 or done == len(all_chunks):
            _log(f"[index]   embedded {done}/{len(all_chunks)}")
    store.finalize()
    store.close()

    manifest = IndexManifest(
        branch=branch,
        commit=commit,
        chunk_count=len(all_chunks),
        file_count=len(files),
        embed_model=EMBED_MODEL,
        embed_dim=EMBED_DIM,
        built_at=datetime.now(timezone.utc).isoformat(),
    )
    manifest_path().write_text(
        json.dumps(dataclasses.asdict(manifest), indent=2), encoding="utf-8"
    )
    _log(f"[index] wrote index -> {db_path}")
    return manifest


def read_manifest() -> IndexManifest | None:
    """Load the manifest for the built index, or None if not built yet."""
    path = manifest_path()
    if not path.exists():
        return None
    data = json.loads(path.read_text(encoding="utf-8"))
    return IndexManifest(**data)


def index_exists() -> bool:
    return index_db_path().exists() and manifest_path().exists()
