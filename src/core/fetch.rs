//! Fetch a doc as markdown from the GitHub mirror, and list available versions.
//! Fetch reads the requested branch (default = latest release); search results
//! carry the repo `path` to pass here. Accepts a repo path, a reader path, or a
//! full docs.servicenow.com URL.

use anyhow::{bail, Result};

use crate::core::chunk::parse_doc;
use crate::core::constants::fetch_live_default;
use crate::core::models::{FetchResult, VersionInfo};
use crate::core::repo;

/// Fetch a topic as markdown. `version` overrides the branch; otherwise a
/// release embedded in the input wins, else the latest release.
///
/// Reads from the local clone by default; pass `live=true` (or set
/// `SNDOC_FETCH_SOURCE=live`) to read live over HTTP instead.
pub fn fetch(path_or_url: &str, version: Option<&str>, live: bool) -> Result<FetchResult> {
    let releases = repo::branch_names();
    let rp = repo::to_repo_path(path_or_url, &releases);
    if rp.repo_path.is_empty() {
        bail!("Could not derive a doc path from '{path_or_url}'.");
    }

    let branch = match version.map(|v| v.to_lowercase()) {
        Some(vl) if releases.contains(&vl) => vl,
        _ => match &rp.release_in_path {
            Some(r) => r.clone(),
            None => repo::resolve_latest_branch(),
        },
    };

    let raw = if live || fetch_live_default() {
        repo::fetch_raw_markdown(&rp.repo_path, &branch)
    } else {
        let mut r = repo::read_doc_from_clone(&rp.repo_path, &branch);
        // Robust fallback only when there's no clone to read from at all; a
        // genuine path miss on a present clone falls through to the error below.
        if r.is_none() && !repo::is_cloned() {
            r = repo::fetch_raw_markdown(&rp.repo_path, &branch);
        }
        r
    };
    let raw = match raw {
        Some(r) => r,
        None => bail!(
            "No doc found at '{}' on branch '{}'. Check the path, try a different \
             version (`sndoc list-versions`), or read live with `--live`.",
            rp.repo_path,
            branch
        ),
    };

    // Strip frontmatter from the returned body; cite canonical_url if present.
    let parsed = parse_doc(&raw.markdown);
    let source_url = if parsed.meta.canonical_url.is_empty() {
        repo::docs_url_for_path(&rp.repo_path)
    } else {
        parsed.meta.canonical_url.clone()
    };
    let trimmed = parsed.body.trim();
    // Add a title heading only if the body doesn't already lead with one.
    let title = if !parsed.meta.title.is_empty() && !trimmed.starts_with('#') {
        format!("# {}\n\n", parsed.meta.title)
    } else {
        String::new()
    };
    Ok(FetchResult {
        markdown: format!("{title}{trimmed}"),
        source_url,
        path: rp.repo_path,
        release: branch,
    })
}

/// List documentation versions (release branches), newest first.
pub fn list_versions() -> Vec<VersionInfo> {
    repo::list_versions()
}
