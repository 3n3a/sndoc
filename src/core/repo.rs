//! Git access to the local ServiceNowDocs clone (via libgit2 / `git2` — no `git`
//! binary), plus raw HTTP fetch for on-demand docs. Branches = release versions.
//!
//! The CLI keeps a full clone under [`repo_dir`]: all branch refs, history, and
//! blobs are present, so every read is offline. Versions are derived from the
//! clone's remote refs and ordered by tip commit date (newest first) — no
//! hardcoded release list. Individual docs are read straight from the object
//! store (any release, no working tree), or fetched as raw markdown over HTTP
//! when `--live` is requested.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, Result};
use git2::{AutotagOption, ErrorClass, ErrorCode, FetchOptions, ObjectType, Remote, Repository};
use regex::Regex;

use crate::core::constants::{
    git_url, github_raw_base, repo_dir, DEFAULT_BRANCH, DOCS_BASE_URL, MARKDOWN_PREFIX,
};
use crate::core::http;
use crate::core::models::VersionInfo;

/// Branches that exist on the mirror but are not release versions. Excluding
/// these is not a maintained release allowlist (which is what we're avoiding) —
/// they're universal non-release names plus the symbolic HEAD alias.
const NON_RELEASE: &[&str] = &["main", "master", "HEAD", "gh-pages", "origin", "nofamily", "mobile", "other", "store"];

fn open() -> Result<Repository> {
    Repository::open(repo_dir()).context("opening local clone")
}

// --- clone lifecycle ------------------------------------------------------

pub fn is_cloned() -> bool {
    repo_dir().join(".git").is_dir()
}

/// Full clone (all refs + history + blobs) into the default clone dir. No
/// working tree is checked out; docs are read directly from the object store.
pub fn clone() -> Result<()> {
    clone_into(&repo_dir())
}

/// Full clone into `dir` (which must not already exist as a repo).
fn clone_into(dir: &Path) -> Result<()> {
    if let Some(parent) = dir.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let url = git_url();
    let repo = Repository::init(dir)
        .with_context(|| format!("initializing repo at {}", dir.display()))?;
    let mut remote = repo
        .remote("origin", &url)
        .with_context(|| format!("adding origin remote {url}"))?;
    fetch_all_branches(&mut remote).context("cloning ServiceNowDocs")?;
    update_origin_head(&repo, &remote);
    Ok(())
}

/// Fetch every release branch from `origin` into `refs/remotes/origin/*`. Used
/// for both the initial clone and later refreshes: unlike gitoxide's plain
/// incremental fetch, libgit2's index-pack resolves thin-pack delta bases
/// against the local object store, so a fetch never leaves an incomplete pack
/// behind — there is no separate "full clone" negotiation needed.
fn fetch_all_branches(remote: &mut Remote<'_>) -> Result<()> {
    let mut opts = FetchOptions::new();
    opts.download_tags(AutotagOption::None);
    remote
        .fetch(&["+refs/heads/*:refs/remotes/origin/*"], Some(&mut opts), None)
        .context("fetching from origin")?;
    Ok(())
}

/// Point `refs/remotes/origin/HEAD` at the remote's default branch (mirrors
/// what a real `git clone` sets up), so [`default_branch`] keeps working.
/// Best-effort: if the remote didn't advertise one, callers fall back to the
/// newest branch by commit date.
fn update_origin_head(repo: &Repository, remote: &Remote<'_>) {
    let Some(target) = remote
        .default_branch()
        .ok()
        .and_then(|buf| buf.as_str().ok().map(str::to_string))
        .and_then(|r| {
            r.strip_prefix("refs/heads/")
                .map(|b| format!("refs/remotes/origin/{b}"))
        })
    else {
        return;
    };
    let _ = repo.reference_symbolic(
        "refs/remotes/origin/HEAD",
        &target,
        true,
        "sndoc: set origin/HEAD",
    );
}

/// Whether an error indicates the local clone is missing objects it should
/// have (an incomplete/corrupt object store), as opposed to a network/
/// transport failure. libgit2 reports this as a `NotFound` error in the
/// ODB/object/tree/indexer class when peeling a ref or reading a blob whose
/// target object isn't present locally.
pub fn is_corrupt_clone_error(err: &anyhow::Error) -> bool {
    err.chain().any(|cause| {
        cause.downcast_ref::<git2::Error>().is_some_and(|e| {
            e.code() == ErrorCode::NotFound
                && matches!(
                    e.class(),
                    ErrorClass::Odb | ErrorClass::Object | ErrorClass::Tree | ErrorClass::Indexer
                )
        })
    })
}

/// Replace the clone with a fresh one, atomically. Clones into a sibling temp
/// dir first and swaps it in only on success, so a failed re-clone (e.g. the
/// network is down) leaves the existing clone untouched.
pub fn reclone() -> Result<()> {
    let dir = repo_dir();
    let tmp = sibling_with_suffix(&dir, ".reclone-tmp");
    let old = sibling_with_suffix(&dir, ".old");

    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).context("removing stale re-clone temp dir")?;
    }
    if let Err(err) = clone_into(&tmp) {
        let _ = std::fs::remove_dir_all(&tmp);
        return Err(err);
    }

    // Swap: move the (corrupt) clone aside, move the fresh one in, then delete
    // the old one best-effort (a Windows lock must not fail the whole refresh).
    let _ = std::fs::remove_dir_all(&old);
    if dir.exists() {
        std::fs::rename(&dir, &old).context("moving old clone aside")?;
    }
    std::fs::rename(&tmp, &dir).context("swapping in the fresh clone")?;
    let _ = std::fs::remove_dir_all(&old);
    Ok(())
}

/// A path next to `dir` with an extra suffix on the file name (a sibling, so
/// the rename swap stays on the same filesystem).
fn sibling_with_suffix(dir: &Path, suffix: &str) -> std::path::PathBuf {
    let name = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".to_string());
    let sibling = format!("{name}{suffix}");
    match dir.parent() {
        Some(parent) => parent.join(sibling),
        None => std::path::PathBuf::from(sibling),
    }
}

/// Update all remote-tracking refs from origin.
pub fn fetch_updates() -> Result<()> {
    let repo = open()?;
    let mut remote = repo.find_remote("origin").context("finding origin remote")?;
    fetch_all_branches(&mut remote).context("fetching updates")?;
    update_origin_head(&repo, &remote);
    Ok(())
}

// --- branches / versions --------------------------------------------------

/// Release branch names with their tip committer time (epoch seconds).
fn release_branches_with_time(repo: &Repository) -> Result<Vec<(String, i64)>> {
    let refs = repo
        .references_glob("refs/remotes/origin/*")
        .context("iterating remote refs")?;
    let mut out: Vec<(String, i64)> = Vec::new();
    for reference in refs {
        let reference = match reference {
            Ok(r) => r,
            Err(_) => continue,
        };
        // short name like "origin/zurich"; strip the remote prefix.
        let short = match reference.shorthand() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let name = short.strip_prefix("origin/").unwrap_or(short).to_string();
        if NON_RELEASE.contains(&name.as_str()) {
            continue;
        }
        if name.contains("feature/") {
            continue;
        }
        let secs = match reference.peel_to_commit() {
            Ok(commit) => commit.time().seconds(),
            Err(_) => continue,
        };
        out.push((name, secs));
    }
    Ok(out)
}

/// Release branch names, newest tip-commit first. No hardcoded release list.
fn branches_by_date(repo: &Repository) -> Vec<String> {
    let mut with_time = release_branches_with_time(repo).unwrap_or_default();
    // Newest first; ties broken by name for determinism.
    with_time.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
    with_time.into_iter().map(|(n, _)| n).collect()
}

/// The release the repo's default branch (origin/HEAD) points at. ServiceNow
/// keeps this at the current GA release, so it's the authoritative "latest".
fn default_branch(repo: &Repository) -> Option<String> {
    let reference = repo.find_reference("refs/remotes/origin/HEAD").ok()?;
    let target = reference.symbolic_target().ok().flatten()?;
    let branch = target.strip_prefix("refs/remotes/origin/")?;
    if NON_RELEASE.contains(&branch) {
        return None;
    }
    Some(branch.to_string())
}

/// Set of release branch names (for version validation).
pub fn branch_names() -> HashSet<String> {
    match open() {
        Ok(repo) => branches_by_date(&repo).into_iter().collect(),
        Err(_) => HashSet::new(),
    }
}

/// The current release: the repo's default branch, else newest by commit date,
/// else [`DEFAULT_BRANCH`].
pub fn resolve_latest_branch() -> String {
    let repo = match open() {
        Ok(r) => r,
        Err(_) => return DEFAULT_BRANCH.to_string(),
    };
    if let Some(latest) = default_branch(&repo) {
        return latest;
    }
    branches_by_date(&repo)
        .into_iter()
        .next()
        .unwrap_or_else(|| DEFAULT_BRANCH.to_string())
}

/// Release branches with the latest flagged and pinned first; the rest follow
/// newest-first by tip commit date.
pub fn list_versions() -> Vec<VersionInfo> {
    let repo = match open() {
        Ok(r) => r,
        Err(_) => return Vec::new(),
    };
    let by_date = branches_by_date(&repo);
    let latest = default_branch(&repo).or_else(|| by_date.first().cloned());
    let mut ordered: Vec<String> = Vec::new();
    if let Some(l) = &latest {
        if by_date.contains(l) {
            ordered.push(l.clone());
        }
    }
    for b in &by_date {
        if Some(b) != latest.as_ref() {
            ordered.push(b.clone());
        }
    }
    ordered
        .into_iter()
        .map(|b| {
            let is_latest = Some(&b) == latest.as_ref();
            VersionInfo {
                release: b,
                is_latest,
            }
        })
        .collect()
}

fn collect_markdown(
    repo: &Repository,
    tree: &git2::Tree<'_>,
    prefix: &str,
    out: &mut Vec<(String, String)>,
) -> Result<()> {
    for entry in tree.iter() {
        let name = entry.name().context("reading tree entry name")?.to_string();
        let full = if prefix.is_empty() {
            name.clone()
        } else {
            format!("{prefix}/{name}")
        };
        match entry.kind() {
            Some(ObjectType::Tree) => {
                let object = entry.to_object(repo).context("loading subtree")?;
                let subtree = object
                    .into_tree()
                    .map_err(|_| anyhow::anyhow!("tree entry {full} is not a tree"))?;
                collect_markdown(repo, &subtree, &full, out)?;
            }
            Some(ObjectType::Blob) if name.ends_with(".md") => {
                let object = entry.to_object(repo).context("loading blob")?;
                let blob = object
                    .into_blob()
                    .map_err(|_| anyhow::anyhow!("tree entry {full} is not a blob"))?;
                let content = String::from_utf8_lossy(blob.content()).into_owned();
                out.push((full, content));
            }
            _ => {}
        }
    }
    Ok(())
}

/// Read every `markdown/**/*.md` file on origin/<branch> straight from the
/// object store (no working tree). Returns `(repo_path, content)` pairs where
/// `repo_path` is relative to `markdown/`, sorted by path.
pub fn read_all_markdown(branch: &str) -> Result<Vec<(String, String)>> {
    let repo = open()?;
    let refname = format!("refs/remotes/origin/{branch}");
    let commit = repo
        .find_reference(&refname)
        .with_context(|| format!("finding {refname}"))?
        .peel_to_commit()
        .with_context(|| format!("peeling {refname}"))?;
    let tree = commit.tree().context("reading commit tree")?;
    let md_entry = match tree.get_path(Path::new(MARKDOWN_PREFIX.trim_end_matches('/'))) {
        Ok(entry) => entry,
        Err(e) if e.code() == ErrorCode::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).context("looking up markdown/ tree entry"),
    };
    let md_tree = md_entry
        .to_object(&repo)
        .context("loading markdown/ tree")?
        .into_tree()
        .map_err(|_| anyhow::anyhow!("markdown/ is not a tree"))?;
    let mut out: Vec<(String, String)> = Vec::new();
    collect_markdown(&repo, &md_tree, "", &mut out)?;
    out.sort_by(|a, b| a.0.cmp(&b.0));
    Ok(out)
}

/// Commit sha at the tip of origin/<branch> (the indexable target).
pub fn branch_tip_commit(branch: &str) -> Result<String> {
    let repo = open()?;
    let refname = format!("refs/remotes/origin/{branch}");
    let commit = repo
        .find_reference(&refname)
        .with_context(|| format!("finding {refname}"))?
        .peel_to_commit()
        .with_context(|| format!("peeling {refname}"))?;
    Ok(commit.id().to_string())
}

// --- path / url helpers ---------------------------------------------------

#[derive(Debug, Clone)]
pub struct RepoPath {
    pub repo_path: String,
    pub release_in_path: Option<String>,
}

/// Normalize a docs URL / reader path / repo path to a repo path under
/// markdown/ (without the prefix), and any release embedded in it.
/// `known_releases` (the dynamic branch set) decides whether a leading segment
/// is a release.
pub fn to_repo_path(input: &str, known_releases: &HashSet<String>) -> RepoPath {
    let mut p = input.trim().to_string();
    p = Regex::new(r"^https?://[^/]+/docs/")
        .unwrap()
        .replace(&p, "")
        .into_owned();
    p = Regex::new(r"^https?://[^/]+/")
        .unwrap()
        .replace(&p, "")
        .into_owned();
    p = Regex::new(r"^/+").unwrap().replace(&p, "").into_owned();
    p = Regex::new(r"^r/").unwrap().replace(&p, "").into_owned();
    p = Regex::new(&format!("^{MARKDOWN_PREFIX}"))
        .unwrap()
        .replace(&p, "")
        .into_owned();
    p = Regex::new(r"\.html$").unwrap().replace(&p, "").into_owned();

    let segments: Vec<&str> = p.split('/').collect();
    let mut release_in_path: Option<String> = None;
    let mut rest = segments.clone();
    if segments.len() > 1 && known_releases.contains(segments[0]) {
        release_in_path = Some(segments[0].to_string());
        rest = segments[1..].to_vec();
    }
    let mut repo_path = rest.join("/");
    if !repo_path.is_empty() && !repo_path.ends_with(".md") {
        repo_path.push_str(".md");
    }
    RepoPath {
        repo_path,
        release_in_path,
    }
}

/// Build a human-facing docs URL for a repo path (citation fallback).
pub fn docs_url_for_path(repo_path: &str) -> String {
    let no_ext = Regex::new(r"\.md$").unwrap().replace(repo_path, "");
    format!("{DOCS_BASE_URL}/r/{no_ext}")
}

#[derive(Debug, Clone)]
pub struct RawDoc {
    pub markdown: String,
    pub raw_url: String,
}

/// Fetch raw markdown for a repo path on a branch over HTTP, or `None` on a miss.
pub fn fetch_raw_markdown(repo_path: &str, branch: &str) -> Option<RawDoc> {
    let clean = Regex::new(&format!("^{MARKDOWN_PREFIX}"))
        .unwrap()
        .replace(repo_path, "")
        .into_owned();
    let raw_url = format!("{}/{}/{}{}", github_raw_base(), branch, MARKDOWN_PREFIX, clean);
    let markdown = http::get_text(&raw_url)?;
    if markdown.trim().is_empty() {
        return None;
    }
    Some(RawDoc { markdown, raw_url })
}

/// Read a doc's committed markdown from the local clone at origin/<branch>.
///
/// Navigates the commit's tree to the blob without touching a working tree. In
/// the full clone every branch's blobs are already local, so this is offline.
/// Returns `None` on a miss (unknown path/branch) or an empty file.
pub fn read_doc_from_clone(repo_path: &str, branch: &str) -> Option<RawDoc> {
    let clean = Regex::new(&format!("^{MARKDOWN_PREFIX}"))
        .unwrap()
        .replace(repo_path, "")
        .into_owned();
    let ref_path = format!("{MARKDOWN_PREFIX}{clean}");

    let repo = open().ok()?;
    let refname = format!("refs/remotes/origin/{branch}");
    let commit = repo.find_reference(&refname).ok()?.peel_to_commit().ok()?;
    let tree = commit.tree().ok()?;
    let entry = tree.get_path(Path::new(&ref_path)).ok()?;
    let object = entry.to_object(&repo).ok()?;
    let blob = object.into_blob().ok()?;
    let markdown = String::from_utf8_lossy(blob.content()).into_owned();
    if markdown.trim().is_empty() {
        return None;
    }
    Some(RawDoc {
        markdown,
        raw_url: format!("{}/{}/{}", github_raw_base(), branch, ref_path),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn releases() -> HashSet<String> {
        ["zurich", "yokohama"].iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn plain_repo_path_gets_md_suffix() {
        let rp = to_repo_path("api/glide-record", &releases());
        assert_eq!(rp.repo_path, "api/glide-record.md");
        assert_eq!(rp.release_in_path, None);
    }

    #[test]
    fn already_md_path_unchanged() {
        let rp = to_repo_path("api/glide-record.md", &releases());
        assert_eq!(rp.repo_path, "api/glide-record.md");
    }

    #[test]
    fn full_docs_url_with_html_stripped() {
        let rp = to_repo_path(
            "https://www.servicenow.com/docs/api/glide-record.html",
            &releases(),
        );
        assert_eq!(rp.repo_path, "api/glide-record.md");
    }

    #[test]
    fn reader_path_with_release() {
        let rp = to_repo_path("r/zurich/api/glide-record", &releases());
        assert_eq!(rp.release_in_path.as_deref(), Some("zurich"));
        assert_eq!(rp.repo_path, "api/glide-record.md");
    }

    #[test]
    fn markdown_prefix_stripped() {
        let rp = to_repo_path("markdown/api/glide-record.md", &releases());
        assert_eq!(rp.repo_path, "api/glide-record.md");
    }

    #[test]
    fn unknown_leading_segment_not_a_release() {
        let rp = to_repo_path("notarelease/api/glide-record", &releases());
        assert_eq!(rp.release_in_path, None);
        assert_eq!(rp.repo_path, "notarelease/api/glide-record.md");
    }

    #[test]
    fn docs_url_drops_md_and_prefixes_reader() {
        assert_eq!(
            docs_url_for_path("api/glide-record.md"),
            "https://www.servicenow.com/docs/r/api/glide-record"
        );
    }

    #[test]
    fn corrupt_clone_error_matches_missing_object_signatures() {
        // A missing blob discovered while reading a doc from an incomplete clone.
        let odb_miss = git2::Error::new(
            ErrorCode::NotFound,
            ErrorClass::Odb,
            "the object could not be found",
        );
        let wrapped = anyhow::Error::new(odb_miss).context("reading doc from clone");
        assert!(is_corrupt_clone_error(&wrapped));

        // The same missing-objects state surfacing while peeling a ref's tree.
        let tree_miss = git2::Error::new(
            ErrorCode::NotFound,
            ErrorClass::Tree,
            "the tree object could not be found",
        );
        let wrapped = anyhow::Error::new(tree_miss).context("peeling refs/remotes/origin/x");
        assert!(is_corrupt_clone_error(&wrapped));
    }

    #[test]
    fn corrupt_clone_error_ignores_network_failures() {
        let net_err = git2::Error::new(
            ErrorCode::GenericError,
            ErrorClass::Net,
            "failed to connect to github.com: connection refused",
        );
        let wrapped = anyhow::Error::new(net_err).context("connecting to origin");
        assert!(!is_corrupt_clone_error(&wrapped));

        // A non-git2 error (e.g. filesystem) must never match.
        let io_err = std::io::Error::new(std::io::ErrorKind::PermissionDenied, "denied");
        let wrapped = anyhow::Error::new(io_err).context("creating repo dir");
        assert!(!is_corrupt_clone_error(&wrapped));
    }

    #[test]
    fn sibling_suffix_stays_next_to_target() {
        let s = sibling_with_suffix(Path::new("/data/sndoc/repo"), ".reclone-tmp");
        assert_eq!(s, Path::new("/data/sndoc/repo.reclone-tmp"));
    }
}
