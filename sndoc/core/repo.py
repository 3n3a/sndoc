"""Git access to the local ServiceNowDocs clone, plus raw fetch for on-demand
docs. Branches = release versions.

The CLI keeps a blobless partial clone under `repo_dir()`: all branch refs and
history are present, file blobs are fetched lazily on checkout. Versions are
derived from the clone's remote refs and ordered by tip commit date (newest
first) — no hardcoded release list. Individual docs (any release) are fetched as
raw markdown over HTTP so reading a doc never churns the working tree.
"""

from __future__ import annotations

import os
import re
import subprocess
from dataclasses import dataclass

import httpx

from .constants import (
    DEFAULT_BRANCH,
    DOCS_BASE_URL,
    GIT_URL,
    GITHUB_RAW_BASE,
    HTTP_TIMEOUT_S,
    MARKDOWN_PREFIX,
    repo_dir,
)
from .models import VersionInfo


# Bound every git call. A hung git raises TimeoutExpired instead of blocking the
# process forever — the failure mode behind MCP fetch timeouts on Windows.
GIT_TIMEOUT_S = 30.0
# Clone pulls a lot; give it plenty of headroom (runs once, at startup).
GIT_CLONE_TIMEOUT_S = 600.0

# Config overrides prepended to every git invocation. Disable fsmonitor so git
# never spawns a background daemon that inherits (and holds open) the captured
# stdout/stderr pipes — on Windows that lingering handle makes subprocess.run
# block on an EOF that never arrives. `useBuiltinFSMonitor` covers older git.
_GIT_HARDENING = ("-c", "core.fsmonitor=false", "-c", "core.useBuiltinFSMonitor=false")


def _git_env() -> dict[str, str]:
    """Environment for git that guarantees it never blocks on an interactive
    prompt (credentials/terminal) — there is no console under an MCP stdio server."""
    env = dict(os.environ)
    env["GIT_TERMINAL_PROMPT"] = "0"  # never prompt on the terminal
    env["GCM_INTERACTIVE"] = "Never"  # Git Credential Manager: no GUI/console prompt
    env["GIT_OPTIONAL_LOCKS"] = "0"  # skip background lock/refresh work
    return env


def _no_window_flags() -> dict[str, int]:
    """On Windows, don't pop a console window for the GUI-launched server."""
    if os.name == "nt":
        return {"creationflags": subprocess.CREATE_NO_WINDOW}
    return {}


def _git(
    *args: str, check: bool = True, timeout: float = GIT_TIMEOUT_S
) -> subprocess.CompletedProcess[str]:
    """Run a git command inside the local clone, capturing output. Hardened so it
    can never hang on a prompt, an inherited pipe, or an unbounded op."""
    return subprocess.run(
        ["git", *_GIT_HARDENING, "-C", str(repo_dir()), *args],
        check=check,
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        timeout=timeout,
        env=_git_env(),
        **_no_window_flags(),
    )


# --- clone lifecycle ------------------------------------------------------

def is_cloned() -> bool:
    return (repo_dir() / ".git").is_dir()


def clone() -> None:
    """Blobless partial clone: cheap (all refs + history, no file blobs).
    Blobs for the checked-out branch are fetched lazily."""
    repo_dir().parent.mkdir(parents=True, exist_ok=True)
    subprocess.run(
        ["git", *_GIT_HARDENING, "clone", "--filter=blob:none", GIT_URL, str(repo_dir())],
        check=True,
        capture_output=True,
        text=True,
        stdin=subprocess.DEVNULL,
        timeout=GIT_CLONE_TIMEOUT_S,
        env=_git_env(),
        **_no_window_flags(),
    )


def fetch_updates() -> None:
    """Update all remote-tracking refs from origin."""
    _git("fetch", "--all", "--prune")


def checkout_latest(branch: str) -> None:
    """Point the working tree at the tip of `origin/<branch>` (creates/resets the
    local branch; lazily fetches that branch's blobs)."""
    _git("checkout", "-B", branch, f"origin/{branch}")


# --- branches / versions --------------------------------------------------

# Branches that exist on the mirror but are not release versions. Excluding
# these is not a maintained release allowlist (which is what we're avoiding) —
# they're universal non-release names plus the symbolic origin/HEAD alias, which
# `%(refname:short)` renders as the bare "origin".
NON_RELEASE = {"main", "master", "HEAD", "gh-pages", "origin"}


def _branches_by_date() -> list[str]:
    """Release branch names, newest tip-commit first. No hardcoded release list."""
    out = _git(
        "for-each-ref",
        "--sort=-committerdate",
        "--format=%(refname:short)",
        "refs/remotes/origin",
    )
    names: list[str] = []
    for line in out.stdout.splitlines():
        short = line.strip()
        if not short:
            continue
        name = re.sub(r"^origin/", "", short)
        if name in NON_RELEASE:
            continue
        names.append(name)
    return names


def default_branch() -> str | None:
    """The release the repo's default branch (origin/HEAD) points at. ServiceNow
    keeps this at the current GA release, so it's the authoritative "latest" —
    tip commit dates don't track release recency (an old release can get a late
    docs patch, and a fresh mirror push stamps every branch the same day)."""
    out = _git("symbolic-ref", "refs/remotes/origin/HEAD", check=False)
    ref = out.stdout.strip()
    prefix = "refs/remotes/origin/"
    if ref.startswith(prefix):
        name = ref[len(prefix):]
        if name not in NON_RELEASE:
            return name
    return None


def branch_names() -> set[str]:
    """Set of release branch names (for version validation)."""
    return set(_branches_by_date())


def resolve_latest_branch() -> str:
    """The current release: the repo's default branch, else newest by commit
    date, else DEFAULT_BRANCH."""
    latest = default_branch()
    if latest:
        return latest
    branches = _branches_by_date()
    return branches[0] if branches else DEFAULT_BRANCH


def list_versions() -> list[VersionInfo]:
    """Release branches with the latest flagged and pinned first; the rest follow
    newest-first by tip commit date."""
    by_date = _branches_by_date()
    latest = default_branch() or (by_date[0] if by_date else None)
    ordered = ([latest] if latest in by_date else []) + [
        b for b in by_date if b != latest
    ]
    return [VersionInfo(release=b, is_latest=(b == latest)) for b in ordered]


def branch_tip_commit(branch: str) -> str:
    """Commit sha at the tip of origin/<branch> (the indexable target)."""
    return _git("rev-parse", f"origin/{branch}").stdout.strip()


def head_commit() -> str:
    """HEAD commit sha of the checked-out working tree."""
    return _git("rev-parse", "HEAD").stdout.strip()


# --- path / url helpers ---------------------------------------------------

@dataclass(slots=True)
class RepoPath:
    repo_path: str
    release_in_path: str | None


def to_repo_path(input_str: str, known_releases: set[str] | None = None) -> RepoPath:
    """Normalize a docs URL / reader path / repo path to a repo path under
    markdown/ (without the prefix), and any release embedded in it. `known_releases`
    (the dynamic branch set) decides whether a leading segment is a release."""
    releases = known_releases if known_releases is not None else branch_names()
    p = input_str.strip()
    p = re.sub(r"^https?://[^/]+/docs/", "", p)  # full docs URL
    p = re.sub(r"^https?://[^/]+/", "", p)  # any other host
    p = re.sub(r"^/+", "", p)
    p = re.sub(r"^r/", "", p)  # reader path prefix
    p = re.sub(rf"^{MARKDOWN_PREFIX}", "", p)  # already a repo path
    p = re.sub(r"\.html$", "", p)

    segments = p.split("/")
    release_in_path: str | None = None
    if len(segments) > 1 and segments[0] in releases:
        release_in_path = segments[0]
        segments = segments[1:]
    repo_path = "/".join(segments)
    if repo_path and not repo_path.endswith(".md"):
        repo_path += ".md"
    return RepoPath(repo_path=repo_path, release_in_path=release_in_path)


def docs_url_for_path(repo_path: str) -> str:
    """Build a human-facing docs URL for a repo path (citation fallback)."""
    no_ext = re.sub(r"\.md$", "", repo_path)
    return f"{DOCS_BASE_URL}/r/{no_ext}"


@dataclass(slots=True)
class RawDoc:
    markdown: str
    raw_url: str


def fetch_raw_markdown(repo_path: str, branch: str) -> RawDoc | None:
    """Fetch raw markdown for a repo path on a branch over HTTP, or None on a miss."""
    clean = re.sub(rf"^{MARKDOWN_PREFIX}", "", repo_path)
    raw_url = f"{GITHUB_RAW_BASE}/{branch}/{MARKDOWN_PREFIX}{clean}"
    with httpx.Client(timeout=HTTP_TIMEOUT_S, follow_redirects=True) as client:
        resp = client.get(raw_url)
    if resp.status_code != 200:
        return None
    markdown = resp.text
    if not markdown.strip():
        return None
    return RawDoc(markdown=markdown, raw_url=raw_url)


def read_doc_from_clone(repo_path: str, branch: str) -> RawDoc | None:
    """Read a doc's committed markdown from the local clone at origin/<branch>.

    `git show origin/<branch>:<path>` reads the blob from the object store without
    touching the working tree. In the blobless partial clone the latest release's
    blobs are already local (checked out by the initial clone), so this is offline;
    for any other branch the missing blob is lazily fetched from the promisor remote
    — only that one object, no checkout. Returns None on a miss (unknown path/branch)
    or an empty file."""
    clean = re.sub(rf"^{MARKDOWN_PREFIX}", "", repo_path)
    ref_path = f"{MARKDOWN_PREFIX}{clean}"
    try:
        out = _git("show", f"origin/{branch}:{ref_path}", check=False)
    except subprocess.TimeoutExpired:
        # A wedged git read is treated as a miss so the caller can fall back
        # (live HTTP) or surface a clean error, rather than hang.
        return None
    if out.returncode != 0 or not out.stdout.strip():
        return None
    return RawDoc(
        markdown=out.stdout,
        raw_url=f"{GITHUB_RAW_BASE}/{branch}/{ref_path}",
    )
