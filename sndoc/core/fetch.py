"""Fetch a doc as markdown from the GitHub mirror, and list available versions.
Fetch reads the requested branch (default = latest release); search results
carry the repo `path` to pass here. Accepts a repo path, a reader path, or a
full docs.servicenow.com URL.
"""

from __future__ import annotations

from .chunk import parse_doc
from .constants import fetch_live_default
from .models import FetchResult, VersionInfo
from .repo import (
    branch_names,
    docs_url_for_path,
    fetch_raw_markdown,
    is_cloned,
    list_versions as _repo_list_versions,
    read_doc_from_clone,
    resolve_latest_branch,
    to_repo_path,
)


def fetch(
    path_or_url: str, version: str | None = None, live: bool = False
) -> FetchResult:
    """Fetch a topic as markdown. `version` overrides the branch; otherwise a
    release embedded in the input wins, else the latest release.

    Reads from the local clone by default (offline for the latest release; other
    releases lazily fetch just the needed blob). Pass `live=True` (or set
    SNDOC_FETCH_SOURCE=live) to read live over HTTP instead."""
    releases = branch_names()
    rp = to_repo_path(path_or_url, releases)
    if not rp.repo_path:
        raise ValueError(f"Could not derive a doc path from '{path_or_url}'.")

    if version and version.lower() in releases:
        branch = version.lower()
    elif rp.release_in_path:
        branch = rp.release_in_path
    else:
        branch = resolve_latest_branch()

    if live or fetch_live_default():
        raw = fetch_raw_markdown(rp.repo_path, branch)
    else:
        raw = read_doc_from_clone(rp.repo_path, branch)
        # Robust fallback only when there's no clone to read from at all; a genuine
        # path miss on a present clone falls through to the error below.
        if raw is None and not is_cloned():
            raw = fetch_raw_markdown(rp.repo_path, branch)
    if raw is None:
        raise ValueError(
            f"No doc found at '{rp.repo_path}' on branch '{branch}'. "
            "Check the path, try a different version (`sndoc list-versions`), "
            "or read live with `--live`."
        )

    # Strip frontmatter from the returned body; cite canonical_url if present.
    parsed = parse_doc(raw.markdown)
    source_url = parsed.meta.canonical_url or docs_url_for_path(rp.repo_path)
    trimmed = parsed.body.strip()
    # Add a title heading only if the body doesn't already lead with one.
    title = (
        f"# {parsed.meta.title}\n\n"
        if parsed.meta.title and not trimmed.startswith("#")
        else ""
    )
    return FetchResult(
        markdown=title + trimmed,
        source_url=source_url,
        path=rp.repo_path,
        release=branch,
    )


def list_versions() -> list[VersionInfo]:
    """List documentation versions (release branches), newest first."""
    return _repo_list_versions()
