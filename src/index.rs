//! Build the hybrid search index from the local ServiceNowDocs clone.
//!
//! Walk `markdown/**` on the target branch (read straight from the object
//! store — no working tree), chunk + embed each file in batches, build SQLite
//! (FTS5 + sqlite-vec), then write the db + manifest into the data dir. Called
//! by `sndoc index` and by the auto-update lifecycle when the latest branch's
//! commit has changed.

use anyhow::{bail, Result};

use crate::core::chunk::{chunk_doc, chunk_embed_text};
use crate::core::constants::{
    embed_model, index_db_path, manifest_path, EMBED_DIM, INDEX_SCHEMA_VERSION,
};
use crate::core::embed::embed_passages;
use crate::core::index_store::IndexStore;
use crate::core::models::{IndexChunk, IndexManifest};
use crate::core::repo;

const BATCH: usize = 32;

fn log(msg: &str) {
    eprintln!("{msg}");
}

/// Build the index from the local clone and write db + manifest. Returns the
/// manifest. Assumes the repo is already cloned.
pub fn build_index(branch: Option<&str>) -> Result<IndexManifest> {
    let branch = match branch {
        Some(b) => b.to_lowercase(),
        None => repo::resolve_latest_branch().to_lowercase(),
    };
    log(&format!("[index] branch: {branch}"));
    let commit = repo::branch_tip_commit(&branch)?;

    let mut docs = repo::read_all_markdown(&branch)?;
    // Optional cap for testing / partial builds (mirrors the legacy `max_files`
    // param). Unset in normal use → index every file.
    if let Ok(cap) = std::env::var("SNDOC_INDEX_MAX_FILES") {
        if let Ok(n) = cap.parse::<usize>() {
            docs.truncate(n);
        }
    }
    log(&format!("[index] {} markdown files", docs.len()));
    if docs.is_empty() {
        bail!("No markdown files under branch '{branch}'");
    }

    // Chunk every file first (cheap), then embed + insert in batches (the cost).
    let mut all_chunks: Vec<IndexChunk> = Vec::new();
    for (repo_path, content) in &docs {
        all_chunks.extend(chunk_doc(content, repo_path, &branch));
    }
    log(&format!(
        "[index] {} chunks; embedding...",
        all_chunks.len()
    ));

    let db_path = index_db_path();
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let db_str = db_path.to_string_lossy().into_owned();
    let mut store = IndexStore::create(&db_str)?;
    let mut done = 0usize;
    for batch in all_chunks.chunks(BATCH) {
        let texts: Vec<String> = batch.iter().map(chunk_embed_text).collect();
        let vecs = embed_passages(&texts)?;
        store.insert_chunks(batch, &vecs)?;
        done += batch.len();
        if done % (BATCH * 20) == 0 || done == all_chunks.len() {
            log(&format!("[index]   embedded {done}/{}", all_chunks.len()));
        }
    }
    store.finalize()?;

    let manifest = IndexManifest {
        branch: branch.clone(),
        commit,
        chunk_count: all_chunks.len(),
        file_count: docs.len(),
        embed_model: embed_model(),
        embed_dim: EMBED_DIM,
        built_at: chrono::Utc::now().to_rfc3339(),
        schema_version: INDEX_SCHEMA_VERSION,
    };
    std::fs::write(manifest_path(), serde_json::to_string_pretty(&manifest)?)?;
    log(&format!("[index] wrote index -> {db_str}"));
    Ok(manifest)
}

/// Load the manifest for the built index, or `None` if not built yet.
pub fn read_manifest() -> Option<IndexManifest> {
    let path = manifest_path();
    let data = std::fs::read_to_string(path).ok()?;
    serde_json::from_str(&data).ok()
}

/// Whether a built index is current for `commit`: same docs commit and the
/// index layout this binary writes. Older layouts must be rebuilt.
pub fn is_current(manifest: &IndexManifest, commit: &str) -> bool {
    manifest.commit == commit && manifest.schema_version == INDEX_SCHEMA_VERSION
}

pub fn index_exists() -> bool {
    index_db_path().exists() && manifest_path().exists()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(commit: &str, schema_version: u32) -> IndexManifest {
        IndexManifest {
            branch: "brazil".into(),
            commit: commit.into(),
            chunk_count: 1,
            file_count: 1,
            embed_model: "m".into(),
            embed_dim: EMBED_DIM,
            built_at: String::new(),
            schema_version,
        }
    }

    #[test]
    fn index_is_current_only_for_same_commit_and_schema() {
        assert!(is_current(&manifest("abc", INDEX_SCHEMA_VERSION), "abc"));
        assert!(!is_current(&manifest("abc", INDEX_SCHEMA_VERSION), "def"));
        assert!(!is_current(
            &manifest("abc", INDEX_SCHEMA_VERSION - 1),
            "abc"
        ));
    }

    #[test]
    fn pre_v06_manifest_reads_as_schema_zero() {
        let json = r#"{"branch":"zurich","commit":"abc","chunk_count":1,"file_count":1,
                       "embed_model":"m","embed_dim":512,"built_at":""}"#;
        let m: IndexManifest = serde_json::from_str(json).unwrap();
        assert_eq!(m.schema_version, 0);
        assert!(!is_current(&m, "abc"));
    }
}
