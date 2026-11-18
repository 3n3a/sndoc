//! The fetch fallback the MCP tool descriptions and the skill tell the model
//! to follow: retry the same path on the next older release from
//! `list_versions` (newest first), and treat "fails on every release" as a
//! wrong path to re-resolve with search. Runs offline against a `file://`
//! fixture repo, like `git_backend.rs`.
//!
//! One `#[test]` only: `SNDOC_DATA_DIR` / `SNDOC_GIT_URL` are process-global.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use git2::{Oid, Repository, Signature, Time};

use sndoc::core::{fetch, repo};

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
        builder
            .insert(name, blob, 0o100644)
            .expect("insert blob entry");
    }
    for (name, sub_entries) in by_top {
        let sub_id = write_tree(repo, &sub_entries);
        builder
            .insert(name, sub_id, 0o040000)
            .expect("insert tree entry");
    }
    builder.write().expect("write tree")
}

/// Commit `entries` to `branch` at an explicit commit time, and point HEAD at
/// the branch (the last branch committed becomes the clone's origin/HEAD).
fn commit_at(repo: &Repository, branch: &str, secs: i64, entries: &[(&str, &str)]) {
    let tree_id = write_tree(repo, entries);
    let tree = repo.find_tree(tree_id).expect("find written tree");
    let sig =
        Signature::new("sndoc test", "test@example.com", &Time::new(secs, 0)).expect("signature");
    let refname = format!("refs/heads/{branch}");
    let parent = repo
        .find_reference(&refname)
        .ok()
        .and_then(|r| r.peel_to_commit().ok());
    let parents: Vec<&git2::Commit<'_>> = parent.iter().collect();
    repo.commit(Some(&refname), &sig, &sig, "docs", &tree, &parents)
        .expect("create commit");
    repo.set_head(&refname).expect("set HEAD");
}

const NEW_PAGE: &str = "markdown/itsm/now-assist/voice.md";
const OLD_PAGE: &str = "markdown/itsm/legacy.md";

#[test]
fn fetch_falls_back_through_older_releases() {
    let source = TempDir::new("fallback-source");
    let data = TempDir::new("fallback-data");

    let src = Repository::init(source.path()).expect("init source repo");
    let with_old: &[(&str, &str)] = &[
        (NEW_PAGE, "# Voice\n\nVoice agents."),
        (OLD_PAGE, "# Legacy\n\nOnly on older releases."),
    ];
    let without_old: &[(&str, &str)] = &[(NEW_PAGE, "# Voice\n\nVoice agents.")];
    commit_at(&src, "xanadu", 1_000, with_old);
    commit_at(&src, "yokohama", 2_000, with_old);
    commit_at(&src, "zurich", 3_000, with_old);
    commit_at(&src, "australia", 4_000, with_old);
    // A late backport on zurich: its tip is now the newest of all branches,
    // which must not move it ahead of australia.
    commit_at(&src, "zurich", 9_000, with_old);
    // brazil (latest, the default branch) dropped the legacy page.
    commit_at(&src, "brazil", 5_000, without_old);
    drop(src);

    std::env::set_var("SNDOC_DATA_DIR", data.path());
    std::env::set_var(
        "SNDOC_GIT_URL",
        format!("file://{}", source.path().display()),
    );
    std::env::remove_var("SNDOC_FETCH_SOURCE");
    repo::clone().expect("clone fixture");

    // 1. Fallback order is true release order, not tip-commit order.
    let order: Vec<String> = fetch::list_versions()
        .into_iter()
        .map(|v| v.release)
        .collect();
    assert_eq!(
        order,
        ["brazil", "australia", "zurich", "yokohama", "xanadu"]
    );
    assert!(fetch::list_versions()[0].is_latest);

    // 2. Default (latest) fetch fails, and the error points at the procedure.
    let path = "itsm/legacy.md";
    let err = fetch::fetch(path, None, false).unwrap_err().to_string();
    assert!(err.contains("'brazil'"), "{err}");
    assert!(err.contains("next older release"), "{err}");
    assert!(err.contains("search for the topic title"), "{err}");

    // 3. Retrying on the next older release succeeds and names that release.
    let res = fetch::fetch(path, Some(&order[1]), false).expect("australia has it");
    assert_eq!(res.release, "australia");
    assert!(res.markdown.contains("Only on older releases."));

    // 4. An unknown release is an error, never a silent switch to latest.
    let err = fetch::fetch("itsm/now-assist/voice.md", Some("bogus"), false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("Unknown version 'bogus'"), "{err}");
    assert!(
        err.contains("brazil, australia, zurich, yokohama, xanadu"),
        "{err}"
    );
    // Empty means "not given" (some MCP clients send "").
    assert_eq!(
        fetch::fetch("itsm/now-assist/voice.md", Some(""), false)
            .unwrap()
            .release,
        "brazil"
    );

    // 5. A link target missing a subfolder fails on every release; the
    //    re-resolved path (what search returns) works on the latest.
    let broken = "itsm/voice.md";
    for release in &order {
        assert!(
            fetch::fetch(broken, Some(release), false).is_err(),
            "{broken} unexpectedly found on {release}"
        );
    }
    let fixed = fetch::fetch("itsm/now-assist/voice.md", Some(&order[0]), false).unwrap();
    assert_eq!(fixed.release, "brazil");
    assert_eq!(fixed.path, "itsm/now-assist/voice.md");
}
