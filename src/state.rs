//! Lifecycle glue: keep the local clone present and fresh, and the index in
//! sync with the latest release branch.
//!
//! [`ensure_ready`] runs at the start of every read command:
//!   1. first run -> clone the docs repo,
//!   2. daily refresh -> fetch if the clone is stale (or forced),
//!   3. change detection -> rebuild the index when the latest branch's commit
//!      differs from the built index's manifest (unless `no_index`).

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};

use crate::core::constants::{state_path, RECLONE_INTERVAL_S, UPDATE_INTERVAL_S};
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
    if !repo::is_cloned() {
        log("[sndoc] first run: cloning ServiceNowDocs (this is a one-time setup)...");
        repo::clone()?;
        let mut state = read_state();
        state["last_fetch"] = serde_json::json!(now());
        write_state(&state)?;
    }

    // Refresh + sync against the clone. If any step fails because the clone is
    // incomplete/corrupt (missing objects it advertises — e.g. a thin-pack base
    // lookup during fetch, or peeling a ref during sync), self-heal by
    // re-cloning once and retrying. A fresh clone gets a self-contained pack,
    // so its object store is always complete.
    match refresh_and_sync(no_index, force_update, sync_worktree, need_index) {
        Ok(()) => Ok(()),
        Err(err) if repo::is_corrupt_clone_error(&err) => {
            let mut state = read_state();
            let last_reclone = state
                .get("last_reclone")
                .and_then(|v| v.as_f64())
                .unwrap_or(0.0);
            if (now() - last_reclone) > RECLONE_INTERVAL_S {
                log("[sndoc] local clone is incomplete; re-cloning ServiceNowDocs...");
                repo::reclone().context("re-cloning after detecting an incomplete clone")?;
                state["last_fetch"] = serde_json::json!(now());
                state["last_reclone"] = serde_json::json!(now());
                write_state(&state)?;
                refresh_and_sync(no_index, force_update, sync_worktree, need_index)
            } else {
                // Re-cloned within the monthly cooldown; a full re-clone is
                // expensive, so don't repeat it. Continue best-effort from the
                // existing clone — a real failure will surface at read time.
                log("[sndoc] local clone still looks incomplete, but it was re-cloned \
                     recently; continuing with the existing clone (the re-clone will be \
                     retried after the monthly cooldown).");
                Ok(())
            }
        }
        Err(err) => Err(err),
    }
}

/// One pass of the freshness + index logic against the existing clone. Returns
/// a corrupt-clone error (see [`repo::is_corrupt_clone_error`]) when the local
/// object store is missing objects, which [`ensure_ready`] recovers from.
fn refresh_and_sync(
    no_index: bool,
    force_update: bool,
    sync_worktree: bool,
    need_index: bool,
) -> Result<()> {
    let mut state = read_state();

    let last_fetch = state
        .get("last_fetch")
        .and_then(|v| v.as_f64())
        .unwrap_or(0.0);
    if force_update || (now() - last_fetch) > UPDATE_INTERVAL_S {
        log("[sndoc] refreshing docs clone...");
        match repo::fetch_updates() {
            Ok(()) => {
                state["last_fetch"] = serde_json::json!(now());
                write_state(&state)?;
            }
            // A corrupt/incomplete clone must propagate so ensure_ready can
            // self-heal (re-clone, subject to the monthly throttle).
            Err(err) if repo::is_corrupt_clone_error(&err) => return Err(err),
            // Transient/network failure: keep serving from the existing clone.
            Err(err) => log(&format!(
                "[sndoc] refresh failed ({err:#}); continuing with the existing clone."
            )),
        }
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
