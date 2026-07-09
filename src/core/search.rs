//! Query orchestration: open the local index, embed the query with the same
//! model used to build the index, and run the hybrid search. The index covers
//! the latest release only (other versions are fetched on demand via fetch).

use std::sync::Mutex;

use anyhow::Result;
use once_cell::sync::Lazy;

use crate::core::constants::index_db_path;
use crate::core::embed::embed_query;
use crate::core::index_store::IndexStore;
use crate::core::models::SearchHit;

// Cache one read-only store per db path (reopened if the path changes).
static STORE: Lazy<Mutex<Option<(String, IndexStore)>>> = Lazy::new(|| Mutex::new(None));

pub fn search(query: &str, limit: usize) -> Result<Vec<SearchHit>> {
    let q = query.trim();
    if q.is_empty() {
        return Ok(Vec::new());
    }
    let vec = embed_query(q)?;
    let file = index_db_path().to_string_lossy().into_owned();

    let mut guard = STORE.lock().expect("search store mutex poisoned");
    let need_open = match guard.as_ref() {
        Some((f, _)) => f != &file,
        None => true,
    };
    if need_open {
        let store = IndexStore::open(&file)?;
        *guard = Some((file, store));
    }
    let store = &guard.as_ref().unwrap().1;
    store.query(q, &vec, limit)
}
