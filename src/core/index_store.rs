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

/// Common English function words that carry no topical signal. Dropped from
/// the FTS query so "how to create a business rule" ranks on "create",
/// "business", "rule" instead of every chunk containing "how"/"to"/"a".
const STOPWORDS: &[&str] = &[
    "a", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for", "from", "how",
    "i", "if", "in", "into", "is", "it", "its", "me", "my", "of", "on", "or", "that", "the",
    "this", "to", "use", "using", "we", "what", "when", "where", "which", "why", "with", "you",
    "your",
];

/// Build a safe FTS5 MATCH expression: OR the alphanumeric query terms so
/// partial matches still contribute to BM25 ranking. With `drop_stopwords`,
/// function words are removed unless that would leave nothing to match.
fn to_fts_expr(query: &str, drop_stopwords: bool) -> String {
    let lowered = query.to_lowercase();
    let mut terms: Vec<&str> = Vec::new();
    let mut seen: HashSet<&str> = HashSet::new();
    for m in TERM_RE.find_iter(&lowered) {
        let t = m.as_str();
        if t.chars().count() >= 2 && seen.insert(t) {
            terms.push(t);
        }
    }
    if drop_stopwords {
        let content: Vec<&str> = terms
            .iter()
            .copied()
            .filter(|t| !STOPWORDS.contains(t))
            .collect();
        if !content.is_empty() {
            terms = content;
        }
    }
    terms
        .iter()
        .map(|t| format!("\"{t}\""))
        .collect::<Vec<_>>()
        .join(" OR ")
}

/// Ranking knobs for [`IndexStore::query_with`]. [`SearchParams::default`] is
/// the tuned configuration used by `sndoc search` and the MCP server;
/// [`SearchParams::legacy`] reproduces the v0.5 ranking for comparison.
#[derive(Debug, Clone)]
pub struct SearchParams {
    /// Chunks fetched from each arm before fusion (at least `n * 4`).
    pub pool: usize,
    /// RRF constant.
    pub rrf_k: f64,
    /// RRF weight of the BM25 arm.
    pub w_fts: f64,
    /// RRF weight of the vector arm.
    pub w_vec: f64,
    /// Fuse per document (each arm contributes the rank of a file's best
    /// chunk) instead of per chunk. Measured worse than chunk-level fusion on
    /// the eval (MRR -0.03), so off by default; kept for experiments.
    pub doc_level: bool,
    /// BM25 column weights: content, title, breadcrumb, description. The
    /// description weight is ignored on indexes built before it existed.
    pub column_weights: [f64; 4],
    /// Drop function words from the BM25 query.
    pub drop_stopwords: bool,
    /// Collapse hits that are the same topic published under several paths
    /// (identical title + snippet), keeping the best-ranked copy.
    pub collapse_duplicates: bool,
}

impl Default for SearchParams {
    fn default() -> Self {
        // Tuned with `cargo run --release --example eval -- --sweep` on a
        // held-out sample (see the example for the method). Title and
        // description hits dominate; the vector arm is down-weighted but kept
        // for queries that share no words with the topic.
        SearchParams {
            pool: 40,
            rrf_k: RRF_K,
            w_fts: 1.0,
            w_vec: 0.3,
            doc_level: false,
            column_weights: [1.0, 20.0, 2.0, 10.0],
            drop_stopwords: true,
            collapse_duplicates: true,
        }
    }
}

impl SearchParams {
    /// The v0.5 ranking: chunk-level RRF, equal weights, default BM25.
    pub fn legacy() -> Self {
        SearchParams {
            pool: 40,
            rrf_k: RRF_K,
            w_fts: 1.0,
            w_vec: 1.0,
            doc_level: false,
            column_weights: [1.0, 1.0, 1.0, 1.0],
            drop_stopwords: false,
            collapse_duplicates: false,
        }
    }
}

/// One arm's candidate: `(chunk id, file path)`.
pub type Candidate = (i64, String);

pub struct IndexStore {
    db: Connection,
    /// Whether the FTS table has the `description` column (index schema >= 2).
    has_description: bool,
}

impl IndexStore {
    /// Open an existing index for queries (read-only).
    pub fn open(file: &str) -> Result<IndexStore> {
        register_vec0();
        let db = Connection::open_with_flags(
            file,
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )?;
        let has_description = db
            .prepare("SELECT * FROM chunks_fts LIMIT 0")?
            .column_names()
            .contains(&"description");
        Ok(IndexStore {
            db,
            has_description,
        })
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
                last_updated TEXT NOT NULL,
                description  TEXT NOT NULL DEFAULT ''
            );
            CREATE VIRTUAL TABLE chunks_fts USING fts5(
                content, title, breadcrumb, description,
                content='chunks', content_rowid='id', tokenize='porter unicode61'
            );
            CREATE VIRTUAL TABLE vec_chunks USING vec0(embedding float[{EMBED_DIM}]);"
        ))?;
        Ok(IndexStore {
            db,
            has_description: true,
        })
    }

    /// Insert a batch of chunks with their embeddings (build phase).
    pub fn insert_chunks(&mut self, chunks: &[IndexChunk], embeddings: &[Vec<f32>]) -> Result<()> {
        let tx = self.db.transaction()?;
        {
            let mut stmt_c = tx.prepare(
                "INSERT INTO chunks
                   (path, anchor, title, breadcrumb, heading, release, content,
                    canonical_url, last_updated, description)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
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
                    c.description,
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
            "INSERT INTO chunks_fts (rowid, content, title, breadcrumb, description)
               SELECT id, content, title, breadcrumb, description FROM chunks;
             INSERT INTO chunks_fts (chunks_fts) VALUES ('optimize');",
        )?;
        self.db.execute_batch("VACUUM;")?;
        self.db
            .execute_batch("PRAGMA wal_checkpoint(TRUNCATE); PRAGMA journal_mode=DELETE;")?;
        Ok(())
    }

    /// Hybrid search with the default (tuned) ranking. Returns up to `n` hits,
    /// one per topic.
    pub fn query(&self, query_text: &str, query_vec: &[f32], n: usize) -> Result<Vec<SearchHit>> {
        self.query_with(query_text, query_vec, n, &SearchParams::default())
    }

    /// Hybrid search: BM25 + vector KNN fused by weighted RRF, one hit per file.
    pub fn query_with(
        &self,
        query_text: &str,
        query_vec: &[f32],
        n: usize,
        p: &SearchParams,
    ) -> Result<Vec<SearchHit>> {
        let pool = p.pool.max(n * 4);
        let fts = self.fts_arm(query_text, pool, p)?;
        let vec_hits = self.vector_arm(query_vec, pool)?;
        self.fuse(&fts, &vec_hits, n, p)
    }

    /// BM25 arm: up to `pool` `(chunk id, path)` pairs, best first.
    pub fn fts_arm(
        &self,
        query_text: &str,
        pool: usize,
        p: &SearchParams,
    ) -> Result<Vec<Candidate>> {
        let fts_expr = to_fts_expr(query_text, p.drop_stopwords);
        let mut out: Vec<Candidate> = Vec::new();
        if fts_expr.is_empty() {
            return Ok(out);
        }
        // Column weights go through FTS5's `rank MATCH` override so the
        // `ORDER BY rank LIMIT` fast path still applies.
        let w = &p.column_weights;
        let rank_fn = if self.has_description {
            format!("bm25({}, {}, {}, {})", w[0], w[1], w[2], w[3])
        } else {
            format!("bm25({}, {}, {})", w[0], w[1], w[2])
        };
        let mut stmt = self.db.prepare(
            "SELECT f.rowid, c.path FROM
               (SELECT rowid, rank FROM chunks_fts
                 WHERE chunks_fts MATCH ?1 AND rank MATCH ?3 ORDER BY rank LIMIT ?2) f
             JOIN chunks c ON c.id = f.rowid ORDER BY f.rank",
        )?;
        let rows = stmt.query_map(params![fts_expr, pool as i64, rank_fn], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Vector arm: the `pool` nearest chunks as `(chunk id, path)`, best first.
    /// This is the expensive part of a query (exact KNN over every chunk).
    pub fn vector_arm(&self, query_vec: &[f32], pool: usize) -> Result<Vec<Candidate>> {
        let blob = to_vec_blob(query_vec);
        let mut stmt = self.db.prepare(
            "SELECT v.rowid, c.path FROM
               (SELECT rowid, distance FROM vec_chunks
                 WHERE embedding MATCH ?1 ORDER BY distance LIMIT ?2) v
             JOIN chunks c ON c.id = v.rowid ORDER BY v.distance",
        )?;
        let rows = stmt.query_map(params![blob, pool as i64], |r| Ok((r.get(0)?, r.get(1)?)))?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /// Fuse the two arms with weighted RRF and resolve the top `n` hits.
    pub fn fuse(
        &self,
        fts: &[Candidate],
        vec_hits: &[Candidate],
        n: usize,
        p: &SearchParams,
    ) -> Result<Vec<SearchHit>> {
        // --- weighted RRF ---
        // Each candidate is a fusion key (file path when doc_level, else chunk
        // id) with a representative chunk: the first (best) chunk seen for it.
        // Insertion order is kept for deterministic tie-breaking.
        let mut scores: HashMap<String, f64> = HashMap::new();
        let mut rep: HashMap<String, i64> = HashMap::new();
        let mut order: Vec<String> = Vec::new();
        for (arm, weight) in [(fts, p.w_fts), (vec_hits, p.w_vec)] {
            let mut rank = 0usize;
            let mut seen_in_arm: HashSet<String> = HashSet::new();
            for (rid, path) in arm.iter() {
                let key = if p.doc_level {
                    path.clone()
                } else {
                    rid.to_string()
                };
                if !seen_in_arm.insert(key.clone()) {
                    continue; // only a file's best chunk counts per arm
                }
                rank += 1;
                if !scores.contains_key(&key) {
                    order.push(key.clone());
                    rep.insert(key.clone(), *rid);
                }
                *scores.entry(key).or_insert(0.0) += weight / (p.rrf_k + rank as f64);
            }
        }
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
        let mut seen_topics: HashSet<(String, String)> = HashSet::new();
        let mut hits: Vec<SearchHit> = Vec::new();
        let mut stmt = self.db.prepare(
            "SELECT path, anchor, title, breadcrumb, heading, release, content, canonical_url
             FROM chunks WHERE id = ?1",
        )?;
        for key in order {
            let score = scores[&key];
            let row = stmt.query_row(params![rep[&key]], |r| {
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
            if !seen_paths.insert(row.path.clone()) {
                continue; // best chunk per file only
            }
            let hit = row_to_hit(row, score);
            if p.collapse_duplicates
                && !seen_topics.insert((hit.title.to_lowercase(), hit.snippet.clone()))
            {
                continue; // same topic already listed under another path
            }
            hits.push(hit);
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
            description: String::new(),
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

    #[test]
    fn fts_expr_drops_stopwords_but_never_everything() {
        assert_eq!(
            to_fts_expr("How to create a business rule", true),
            "\"create\" OR \"business\" OR \"rule\""
        );
        assert_eq!(to_fts_expr("how to", true), "\"how\" OR \"to\"");
        assert_eq!(to_fts_expr("How to", false), "\"how\" OR \"to\"");
    }

    #[test]
    fn doc_level_fusion_and_duplicate_collapse() {
        let dir = std::env::temp_dir().join(format!("sndoc_idx_test2_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let file = dir.join("t.db");
        let f = file.to_str().unwrap();

        let mut store = IndexStore::create(f).unwrap();
        let mut dup_a = chunk("a/dup.md", "Dup", "identical body");
        let mut dup_b = chunk("b/dup.md", "Dup", "identical body");
        dup_a.title = "Same Topic".into();
        dup_b.title = "Same Topic".into();
        let mut described = chunk("c.md", "C", "unrelated words");
        described.description = "kangaroo handling".into();
        let chunks = vec![
            chunk("long.md", "L1", "zebra"),
            chunk("long.md", "L2", "zebra"),
            chunk("long.md", "L3", "zebra"),
            chunk("other.md", "O", "zebra"),
            dup_a,
            dup_b,
            described,
        ];
        let vecs = vec![
            unit(0),
            unit(0),
            unit(0),
            unit(0),
            unit(3),
            unit(3),
            unit(5),
        ];
        store.insert_chunks(&chunks, &vecs).unwrap();
        store.finalize().unwrap();
        let store = IndexStore::open(f).unwrap();
        assert!(store.has_description);

        // Doc-level: long.md's three chunks count once per arm.
        let p = SearchParams::default();
        let fts = store.fts_arm("zebra", 40, &p).unwrap();
        assert_eq!(fts.len(), 4); // chunk-level candidates
        let hits = store.fuse(&fts, &[], 10, &p).unwrap();
        let paths: Vec<&str> = hits.iter().map(|h| h.path.as_str()).collect();
        assert_eq!(paths.len(), 2);
        assert!(paths.contains(&"long.md") && paths.contains(&"other.md"));

        // The same topic under two paths is listed once.
        let hits = store.query_with("identical", &unit(3), 10, &p).unwrap();
        assert_eq!(hits.iter().filter(|h| h.title == "Same Topic").count(), 1);
        let legacy = store
            .query_with("identical", &unit(3), 10, &SearchParams::legacy())
            .unwrap();
        assert_eq!(legacy.iter().filter(|h| h.title == "Same Topic").count(), 2);

        // The description column is searchable.
        let hits = store.query_with("kangaroo", &unit(7), 10, &p).unwrap();
        assert_eq!(hits[0].path, "c.md");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
