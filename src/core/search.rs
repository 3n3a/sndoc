//! Query orchestration: open the local index, embed the query with the same
//! model used to build the index, and run the hybrid search. The index covers
//! the latest release only (other versions are fetched on demand via fetch).

use anyhow::Result;

use crate::core::constants::index_db_path;
use crate::core::embed::embed_query;
use crate::core::index_store::IndexStore;
use crate::core::models::SearchHit;

/// A fresh read-only connection per call rather than one cached/shared
/// connection: `rusqlite::Connection` isn't `Sync`, and under the HTTP MCP
/// transport concurrent tool calls run on different threads, so a shared
/// connection would need to be serialized behind a lock — collapsing
/// concurrent searches to one at a time. SQLite allows many concurrent
/// readers of a read-only db, so opening per call is cheap and lets searches
/// actually run in parallel; it also means a reindex's fresh db file is
/// picked up on the next call instead of a stale cached connection lingering.
pub fn search(query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let vec = embed_query(q)?;
    let file = index_db_path().to_string_lossy().into_owned();
    let store = IndexStore::open(&file)?;
    store.query(q, &vec, limit)
}
