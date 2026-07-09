//! The hybrid search index: SQLite with an FTS5 (BM25) table over chunk text
//! and a sqlite-vec vec0 virtual table over chunk embeddings. The indexer
//! builds it; the query side fuses the two arms with Reciprocal Rank Fusion so
//! exact-term queries (FTS) and conceptual queries (vector) both rank well.
//!
//! sqlite-vec is registered in-process via `sqlite3_auto_extension` (no
//! loadable extension file), and FTS5 is compiled into the bundled SQLite.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Once;

use anyhow::Result;
use once_cell::sync::Lazy;
use regex::Regex;
use rusqlite::{params, Connection, OpenFlags};

use crate::core::constants::EMBED_DIM;
use crate::core::embed::to_vec_blob;
use crate::core::models::{IndexChunk, SearchHit};
use crate::core::repo::docs_url_for_path;

const RRF_K: f64 = 60.0;

static WS_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").unwrap());
static TERM_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[a-z0-9]+").unwrap());

/// Register the sqlite-vec `vec0` extension for all future connections. Safe to
/// call repeatedly; the registration happens exactly once.
fn register_vec0() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| unsafe {
        rusqlite::ffi::sqlite3_auto_extension(Some(std::mem::transmute(
            sqlite_vec::sqlite3_vec_init as *const (),
        )));
    });
}

/// Environment probe for `doctor`: register vec0, open an in-memory db, read
/// the sqlite-vec version, and create temp vec0 + fts5 tables to prove both
/// load. Returns the sqlite-vec version string on success.
pub fn probe() -> Result<String> {
    register_vec0();
    let db = Connection::open_in_memory()?;
    let vec_version: String = db.query_row("SELECT vec_version()", [], |r| r.get(0))?;
    db.execute_batch(
        "CREATE VIRTUAL TABLE v USING vec0(embedding float[4]);
         CREATE VIRTUAL TABLE f USING fts5(body);",
    )?;
    Ok(vec_version)
}

/// Build a safe FTS5 MATCH expression: OR the alphanumeric query terms so
/// partial matches still contribute to BM25 ranking.
fn to_fts_expr(query: &str) -> String {
    let lowered = query.to_lowercase();
    let mut terms: Vec<String> = Vec::new();
    let mut seen: HashSet<String> = HashSet::new();
    for m in TERM_RE.find_iter(&lowered) {
        let t = m.as_str();
        if t.chars().count() >= 2 && !seen.contains(t) {
            seen.insert(t.to_string());
            terms.push(format!("\"{t}\""));
        }
    }
    terms.join(" OR ")
}

pub struct IndexStore {
    db: Connection,
}

impl IndexStore {
    /// Open an existing index for queries (read-only).
    pub fn open(file: &str) -> Result<IndexStore> {
        register_vec0();
        let db = Connection::open_with_flags(
            file,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        Ok(IndexStore { db })
    }

    /// Create a fresh index file with the schema (overwrites any existing).
    pub fn create(file: &str) -> Result<IndexStore> {
        register_vec0();
        if Path::new(file).exists() {
            std::fs::remove_file(file)?;
        }
        let db = Connection::open(file)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(&format!(
            "CREATE TABLE chunks (
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
            CREATE VIRTUAL TABLE vec_chunks USING vec0(embedding float[{EMBED_DIM}]);"
        ))?;
        Ok(IndexStore { db })
    }

    /// Insert a batch of chunks with their embeddings (build phase).
    pub fn insert_chunks(&mut self, chunks: &[IndexChunk], embeddings: &[Vec<f32>]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut stmt_c = tx.prepare(
                "INSERT INTO chunks
                   (path, anchor, title, breadcrumb, heading, release, content,
                    canonical_url, last_updated)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            )?;
            let mut stmt_v =
                tx.prepare("INSERT INTO vec_chunks (rowid, embedding) VALUES (?1, ?2)")?;
            for (c, vec) in chunks.iter().zip(embeddings.iter()) {
                stmt_c.execute(params![
                    c.path,
                    c.anchor,
                    c.title,
                    c.breadcrumb,
                    c.heading,
                    c.release,
                    c.content,
                    c.canonical_url,
                    c.last_updated,
                ])?;
                let rowid = tx.last_insert_rowid();
                stmt_v.execute(params![rowid, to_vec_blob(vec)])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Populate the FTS index from chunks and compact the file. Call once after
    /// all inserts. Leaves a single self-contained db file (no -wal/-shm).
    pub fn finalize(&mut self) -> Result<()> {
        self.db.execute_batch(
            "INSERT INTO chunks_fts (rowid, content, title, breadcrumb)
               SELECT id, content, title, breadcrumb FROM chunks;
             INSERT INTO chunks_fts (chunks_fts) VALUES ('optimize');",
        )?;
        self.db.execute_batch("VACUUM;")?;
        self.db
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        Ok(())
    }

    /// Hybrid search: BM25 + vector KNN fused by RRF, deduped to the best chunk
    /// per file. Returns up to `n` hits.
    pub fn query(&self, query_text: &str, query_vec: &[f32], n: usize) -> Result<Vec<SearchHit>> {
        let pool = std::cmp::max(n * 4, 40) as i64;

        // --- BM25 arm ---
        let fts_expr = to_fts_expr(query_text);
        let mut fts_ids: Vec<i64> = Vec::new();
        if !fts_expr.is_empty() {
            let mut stmt = self.db.prepare(
                "SELECT rowid FROM chunks_fts WHERE chunks_fts MATCH ?1 ORDER BY rank LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![fts_expr, pool], |r| r.get::<_, i64>(0))?;
            for id in rows {
                fts_ids.push(id?);
            }
        }

        // --- vector arm ---
        let blob = to_vec_blob(query_vec);
        let mut vec_ids: Vec<i64> = Vec::new();
        {
            let mut stmt = self.db.prepare(
                "SELECT rowid FROM vec_chunks WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2",
            )?;
            let rows = stmt.query_map(params![blob, pool], |r| r.get::<_, i64>(0))?;
            for id in rows {
                vec_ids.push(id?);
            }
        }

        // --- RRF fusion (preserve first-seen order for deterministic ties) ---
        let mut scores: HashMap<i64, f64> = HashMap::new();
        let mut order: Vec<i64> = Vec::new();
        let add_ranks = |ids: &[i64], scores: &mut HashMap<i64, f64>, order: &mut Vec<i64>| {
            for (rank, &rid) in ids.iter().enumerate() {
                let contrib = 1.0 / (RRF_K + rank as f64 + 1.0);
                if !scores.contains_key(&rid) {
                    order.push(rid);
                }
                *scores.entry(rid).or_insert(0.0) += contrib;
            }
        };
        add_ranks(&fts_ids, &mut scores, &mut order);
        add_ranks(&vec_ids, &mut scores, &mut order);
        if scores.is_empty() {
            return Ok(Vec::new());
        }

        // Stable sort by score desc; equal scores keep first-seen order.
        order.sort_by(|a, b| {
            scores[b]
                .partial_cmp(&scores[a])
                .unwrap_or(std::cmp::Ordering::Equal)
        });

        let mut seen_paths: HashSet<String> = HashSet::new();
        let mut hits: Vec<SearchHit> = Vec::new();
        let mut stmt = self.db.prepare(
            "SELECT path, anchor, title, breadcrumb, heading, release, content, canonical_url
             FROM chunks WHERE id = ?1",
        )?;
        for rid in order {
            let score = scores[&rid];
            let row = stmt.query_row(params![rid], |r| {
                Ok(Row {
                    path: r.get(0)?,
                    anchor: r.get(1)?,
                    title: r.get(2)?,
                    breadcrumb: r.get(3)?,
                    heading: r.get(4)?,
                    release: r.get(5)?,
                    content: r.get(6)?,
                    canonical_url: r.get(7)?,
                })
            });
            let row = match row {
                Ok(r) => r,
                Err(_) => continue,
            };
            if seen_paths.contains(&row.path) {
                continue; // best chunk per file only
            }
            seen_paths.insert(row.path.clone());
            hits.push(row_to_hit(row, score));
            if hits.len() >= n {
                break;
            }
        }
        Ok(hits)
    }
}

struct Row {
    path: String,
    anchor: String,
    title: String,
    breadcrumb: String,
    heading: String,
    release: String,
    content: String,
    canonical_url: String,
}

fn row_to_hit(row: Row, score: f64) -> SearchHit {
    let base = if row.canonical_url.is_empty() {
        docs_url_for_path(&row.path)
    } else {
        row.canonical_url.clone()
    };
    let url = if row.anchor.is_empty() {
        base
    } else {
        format!("{base}#{}", row.anchor)
    };
    let collapsed = WS_RE.replace_all(&row.content, " ");
    let snippet: String = collapsed.chars().take(240).collect();
    let title = if !row.title.is_empty() {
        row.title
    } else if !row.heading.is_empty() {
        row.heading
    } else {
        row.path.clone()
    };
    SearchHit {
        path: row.path,
        title,
        breadcrumb: row.breadcrumb,
        anchor: row.anchor,
        release: row.release,
        url,
        snippet: snippet.trim().to_string(),
        score,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::models::IndexChunk;

    fn unit(dim: usize) -> Vec<f32> {
        let mut v = vec![0f32; EMBED_DIM];
        v[dim] = 1.0;
        v
    }

    fn chunk(path: &str, heading: &str, content: &str) -> IndexChunk {
        IndexChunk {
            path: path.to_string(),
            anchor: heading.to_lowercase(),
            title: path.to_string(),
            breadcrumb: "Docs".to_string(),
            heading: heading.to_string(),
            release: "zurich".to_string(),
            content: content.to_string(),
            canonical_url: String::new(),
            last_updated: String::new(),
        }
    }

    #[test]
    fn build_and_query_hybrid() {
        let dir = std::env::temp_dir().join(format!("sndoc_idx_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.db");
        let f = file.to_str().unwrap();

        let mut store = IndexStore::create(f).unwrap();
        let chunks = vec![
            chunk("a.md", "Alpha", "alpha beta gamma"),
            chunk("a.md", "Alpha2", "second chunk of a"),
            chunk("b.md", "Beta", "delta epsilon zeta"),
        ];
        let vecs = vec![unit(0), unit(2), unit(1)];
        store.insert_chunks(&chunks, &vecs).unwrap();
        store.finalize().unwrap();

        let store = IndexStore::open(f).unwrap();
        // BM25 arm should surface a.md for "alpha"; vector arm agrees (unit(0)).
        let hits = store.query("alpha", &unit(0), 8).unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].path, "a.md");
        // Best-chunk-per-file dedup: only one hit per path.
        let paths: HashSet<_> = hits.iter().map(|h| h.path.clone()).collect();
        assert_eq!(paths.len(), hits.len());

        // Vector-only query (nonsense text, vector points at b.md).
        let hits = store.query("zzzz", &unit(1), 8).unwrap();
        assert_eq!(hits[0].path, "b.md");

        let _ = std::fs::remove_dir_all(&dir);
    }
}

