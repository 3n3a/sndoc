//! End-to-end coverage of the `core::repo` git backend (libgit2 via `git2`):
//! initial clone, fetching local updates, and recovering from a stale local
//! ref. Runs against a `file://` fixture repo built on the fly, so it's
//! offline and doesn't touch the real ServiceNowDocs mirror.
//!
//! `SNDOC_DATA_DIR` / `SNDOC_GIT_URL` are process-global env vars read on
//! every call (see `core::constants`), so this file has exactly one `#[test]`
//! to avoid cross-test races — cargo runs each integration test file as its
//! own process, but functions *within* one file run concurrently by default.
//!
//! Caveat: a `file://` fixture exercises clone/fetch/update correctness, but
//! not the smart HTTP-protocol thin-pack path that originally motivated the
//! libgit2 rewrite — that's inherent to the transport and not reproducible
//! against a local repo.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use git2::{Oid, Repository, Signature};

use sndoc::core::repo;

/// Removes its directory (recursively, best-effort) on drop, so the fixture
/// and clone dirs are cleaned up even if an assertion panics partway through.
struct TempDir(PathBuf);

impl TempDir {
    fn new(label: &str) -> Self {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let dir = std::env::temp_dir().join(format!(
            "sndoc-test-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        TempDir(dir)
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Build a nested tree from flat `(path, content)` pairs (paths include the
/// `markdown/` prefix, e.g. `"markdown/a/x.md"`), writing blobs/trees into
/// `repo`'s object database. Returns the root tree's oid.
fn write_tree<'a>(repo: &Repository, entries: &[(&'a str, &'a str)]) -> Oid {
    let mut by_top: BTreeMap<&'a str, Vec<(&'a str, &'a str)>> = BTreeMap::new();
    let mut files: Vec<(&'a str, &'a str)> = Vec::new();
    for &(path, content) in entries {
        match path.split_once('/') {
            Some((first, rest)) => by_top.entry(first).or_default().push((rest, content)),
            None => files.push((path, content)),
        }
    }
    let mut builder = repo.treebuilder(None).expect("new treebuilder");
    for (name, content) in files {
        let blob = repo.blob(content.as_bytes()).expect("write blob");
        builder.insert(name, blob, 0o100644).expect("insert blob entry");
    }
    for (name, sub_entries) in by_top {
        let sub_id = write_tree(repo, &sub_entries);
        builder.insert(name, sub_id, 0o040000).expect("insert tree entry");
    }
    builder.write().expect("write tree")
}

/// Commit `entries` to `branch` in `repo` (creating the branch if it doesn't
/// exist yet, else committing on top of its current tip) and point HEAD at
/// it, so a client fetching this repo sees it as the default branch. Returns
/// the new commit's oid.
fn commit_files(repo: &Repository, branch: &str, entries: &[(&str, &str)]) -> Oid {
    let tree_id = write_tree(repo, entries);
    let tree = repo.find_tree(tree_id).expect("find written tree");
    let sig = Signature::now("sndoc test", "test@example.com").expect("signature");
    let refname = format!("refs/heads/{branch}");
    let parent = repo
        .find_reference(&refname)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    let commit_id = repo
        .commit(Some(&refname), &sig, &sig, "docs update", &tree, &parents)
        .expect("create commit");
    repo.set_head(&refname).expect("set HEAD");
    commit_id
}

#[test]
fn clone_fetch_and_update_lifecycle() {
    let source = TempDir::new("source");
    let clone_data = TempDir::new("clone-data");
    let branch = "washington";

    // --- Build the source fixture repo with an initial commit -------------
    let source_repo = Repository::init(source.path()).expect("init source repo");
    commit_files(
        &source_repo,
        branch,
        &[
            ("markdown/a/x.md", "# X\n\nHello."),
            ("markdown/b/y.md", "# Y\n\nWorld."),
        ],
    );
    let initial_tip = source_repo
        .find_reference(&format!("refs/heads/{branch}"))
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id();
    drop(source_repo);

    // Point sndoc at the fixture instead of a real per-user data dir / the
    // real GitHub mirror.
    std::env::set_var("SNDOC_DATA_DIR", clone_data.path());
    std::env::set_var(
        "SNDOC_GIT_URL",
        format!("file://{}", source.path().display()),
    );

    // --- 1. Initial load ----------------------------------------------------
    assert!(!repo::is_cloned(), "nothing cloned yet");
    repo::clone().expect("initial clone");
    assert!(repo::is_cloned());
    assert_eq!(repo::resolve_latest_branch(), branch);
    assert_eq!(
        repo::branch_tip_commit(branch).unwrap(),
        initial_tip.to_string()
    );
    let files = repo::read_all_markdown(branch).expect("read markdown");
    assert_eq!(
        files,
        vec![
            ("a/x.md".to_string(), "# X\n\nHello.".to_string()),
            ("b/y.md".to_string(), "# Y\n\nWorld.".to_string()),
        ]
    );

    // --- 2. Fetching local updates -------------------------------------------
    // Add a new commit upstream, then pull it down and confirm it's visible.
    let source_repo = Repository::open(source.path()).expect("reopen source repo");
    commit_files(
        &source_repo,
        branch,
        &[
            ("markdown/a/x.md", "# X\n\nHello."),
            ("markdown/b/y.md", "# Y\n\nWorld."),
            ("markdown/c/z.md", "# Z\n\nNew doc."),
        ],
    );
    let updated_tip = source_repo
        .find_reference(&format!("refs/heads/{branch}"))
        .unwrap()
        .peel_to_commit()
        .unwrap()
        .id();
    drop(source_repo);

    repo::fetch_updates().expect("fetch updates");
    assert_eq!(
        repo::branch_tip_commit(branch).unwrap(),
        updated_tip.to_string()
    );
    let files = repo::read_all_markdown(branch).expect("read markdown after fetch");
    assert_eq!(files.len(), 3);
    assert!(files
        .iter()
        .any(|(path, content)| path == "c/z.md" && content.contains("New doc")));

    // --- 3. Updating from an old commit --------------------------------------
    // Rewind the *clone's* tracking ref back to the original tip (as if the
    // local clone had gone stale), then fetch and confirm it advances forward
    // to the upstream tip again.
    let clone_repo =
        Repository::open(sndoc::core::constants::repo_dir()).expect("open local clone");
    clone_repo
        .reference(
            &format!("refs/remotes/origin/{branch}"),
            initial_tip,
            true,
            "test: rewind to simulate a stale clone",
        )
        .expect("rewind clone ref");
    drop(clone_repo);
    assert_eq!(
        repo::branch_tip_commit(branch).unwrap(),
        initial_tip.to_string(),
        "ref rewind didn't take"
    );

    repo::fetch_updates().expect("fetch updates after rewind");
    assert_eq!(
        repo::branch_tip_commit(branch).unwrap(),
        updated_tip.to_string(),
        "fetch did not advance the stale ref back to the upstream tip"
    );

    std::env::remove_var("SNDOC_DATA_DIR");
    std::env::remove_var("SNDOC_GIT_URL");
}
