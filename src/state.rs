//! Lifecycle glue: keep the local clone present and fresh, and the index in
//! sync with the latest release branch.
//!
//! [`ensure_ready`] runs at the start of every read command:
//!   1. first run -> clone the docs repo,
//!   2. daily refresh -> fetch if the clone is stale (or forced),
//!   3. change detection -> rebuild the index when the latest branch's commit
//!      differs from the built index's manifest (unless `no_index`).

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Result};

use crate::core::constants::{state_path, UPDATE_INTERVAL_S};
use crate::core::repo;
use crate::index;

fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

fn read_state() -> serde_json::Value {
    match std::fs::read_to_string(state_path()) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|_| serde_json::json!({})),
        Err(_) => serde_json::json!({}),
    }
}

fn write_state(state: &serde_json::Value) -> Result<()> {
    let path = state_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, serde_json::to_string_pretty(state)?)?;
    Ok(())
}

fn log(msg: &str) {
    eprintln!("{msg}");
}

/// Make the local clone (and optionally the index) ready to serve.
///
/// `sync_worktree`: resolve the latest branch and reindex on change. Set for
/// commands that read the index (search, index, update, serve). Unset for
/// ref-only commands (list-versions, fetch).
///
/// `need_index`: error out if no index exists and `no_index` suppressed
/// building.
pub fn ensure_ready(
    no_index: bool,
    force_update: bool,
    sync_worktree: bool,
    need_index: bool,
) -> Result<()> {
    let mut state = read_state();

    if !repo::is_cloned() {
        log("[sndoc] first run: cloning ServiceNowDocs (this is a one-time setup)...");
        repo::clone()?;
        state["last_fetch"] = serde_json::json!(now());
        write_state(&state)?;
    }

    let last_fetch = state
        .get("last_fetch")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if force_update || (now() - last_fetch) > UPDATE_INTERVAL_S {
        log("[sndoc] refreshing docs clone...");
        repo::fetch_updates()?;
        state["last_fetch"] = serde_json::json!(now());
        write_state(&state)?;
    }

    if !sync_worktree {
        return Ok(());
    }

    let latest = repo::resolve_latest_branch();
    let target_commit = repo::branch_tip_commit(&latest)?;
    let manifest = index::read_manifest();

    let up_to_date = manifest
        .as_ref()
        .map(|m| m.commit == target_commit)
        .unwrap_or(false);

    if !up_to_date {
        if no_index {
            if need_index && !index::index_exists() {
                bail!(
                    "No search index found and --no-index was given. Run `sndoc index` \
                     (or drop --no-index) to build it."
                );
            }
            return Ok(());
        }
        log(&format!(
            "[sndoc] docs changed ({latest} @ {}); reindexing...",
            &target_commit[..target_commit.len().min(8)]
        ));
        index::build_index(Some(&latest))?;
    } else if need_index && !index::index_exists() {
        bail!("Search index is missing. Run `sndoc index`.");
    }

    Ok(())
}
