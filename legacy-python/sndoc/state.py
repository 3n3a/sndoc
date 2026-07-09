"""Lifecycle glue: keep the local clone present and fresh, and the index in
sync with the latest release branch.

`ensure_ready()` runs at the start of every read command:
  1. first run -> clone the docs repo (blobless),
  2. daily refresh -> `git fetch` if the clone is stale (or forced),
  3. change detection -> rebuild the index when the latest branch's commit
     differs from the built index's manifest (unless --no-index).
"""

from __future__ import annotations

import json
import time

from .core.constants import UPDATE_INTERVAL_S, state_path
from .core import repo
from . import index


def _read_state() -> dict:
    path = state_path()
    if not path.exists():
        return {}
    try:
        return json.loads(path.read_text(encoding="utf-8"))
    except (json.JSONDecodeError, OSError):
        return {}


def _write_state(state: dict) -> None:
    path = state_path()
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(state, indent=2), encoding="utf-8")


def _log(msg: str) -> None:
    import sys

    print(msg, file=sys.stderr)


def ensure_ready(
    *,
    no_index: bool = False,
    force_update: bool = False,
    sync_worktree: bool = False,
    need_index: bool = False,
) -> None:
    """Make the local clone (and optionally the index) ready to serve.

    sync_worktree: check out the latest branch and reindex on change. Set for
        commands that read the index/worktree (search, index, update, serve).
        Unset for ref-only commands (list-versions, fetch via raw HTTP).
    need_index: error out if no index exists and --no-index suppressed building.
    """
    state = _read_state()

    if not repo.is_cloned():
        _log("[sndoc] first run: cloning ServiceNowDocs (this is a one-time setup)...")
        repo.clone()
        state["last_fetch"] = time.time()
        _write_state(state)

    last_fetch = float(state.get("last_fetch", 0.0))
    if force_update or (time.time() - last_fetch) > UPDATE_INTERVAL_S:
        _log("[sndoc] refreshing docs clone...")
        repo.fetch_updates()
        state["last_fetch"] = time.time()
        _write_state(state)

    if not sync_worktree:
        return

    latest = repo.resolve_latest_branch()
    repo.checkout_latest(latest)
    target_commit = repo.branch_tip_commit(latest)
    manifest = index.read_manifest()

    if manifest is None or manifest.commit != target_commit:
        if no_index:
            if need_index and not index.index_exists():
                raise RuntimeError(
                    "No search index found and --no-index was given. "
                    "Run `sndoc index` (or drop --no-index) to build it."
                )
            return
        _log(f"[sndoc] docs changed ({latest} @ {target_commit[:8]}); reindexing...")
        index.build_index(branch=latest)
    elif need_index and not index.index_exists():
        raise RuntimeError("Search index is missing. Run `sndoc index`.")
