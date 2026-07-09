"""ensure_ready lifecycle: first-run clone, daily-refresh throttle, reindex on
commit change, and --no-index behavior. All git/index calls are mocked."""

from __future__ import annotations

import time
import types

import pytest

from sndoc import state


@pytest.fixture
def mocked(mocker):
    """Patch the git + index calls ensure_ready makes; return the mocks."""
    m = types.SimpleNamespace()
    m.is_cloned = mocker.patch.object(state.repo, "is_cloned", return_value=True)
    m.clone = mocker.patch.object(state.repo, "clone")
    m.fetch_updates = mocker.patch.object(state.repo, "fetch_updates")
    m.checkout_latest = mocker.patch.object(state.repo, "checkout_latest")
    m.resolve_latest = mocker.patch.object(
        state.repo, "resolve_latest_branch", return_value="zurich"
    )
    m.branch_tip = mocker.patch.object(
        state.repo, "branch_tip_commit", return_value="newsha"
    )
    m.read_manifest = mocker.patch.object(state.index, "read_manifest")
    m.build_index = mocker.patch.object(state.index, "build_index")
    m.index_exists = mocker.patch.object(state.index, "index_exists", return_value=True)
    return m


def test_first_run_clones_and_skips_fetch(mocked):
    mocked.is_cloned.return_value = False
    state.ensure_ready(sync_worktree=False)
    mocked.clone.assert_called_once()
    mocked.fetch_updates.assert_not_called()


def test_stale_clone_triggers_fetch(mocked):
    state._write_state({"last_fetch": 0.0})  # very old
    state.ensure_ready(sync_worktree=False)
    mocked.clone.assert_not_called()
    mocked.fetch_updates.assert_called_once()


def test_fresh_clone_skips_fetch(mocked):
    state._write_state({"last_fetch": time.time()})
    state.ensure_ready(sync_worktree=False)
    mocked.fetch_updates.assert_not_called()


def test_force_update_fetches_even_when_fresh(mocked):
    state._write_state({"last_fetch": time.time()})
    state.ensure_ready(force_update=True, sync_worktree=True)
    mocked.fetch_updates.assert_called_once()


def test_reindex_when_commit_changed(mocked):
    state._write_state({"last_fetch": time.time()})
    mocked.read_manifest.return_value = types.SimpleNamespace(commit="oldsha")
    state.ensure_ready(sync_worktree=True)
    mocked.build_index.assert_called_once_with(branch="zurich")


def test_no_reindex_when_commit_matches(mocked):
    state._write_state({"last_fetch": time.time()})
    mocked.read_manifest.return_value = types.SimpleNamespace(commit="newsha")
    state.ensure_ready(sync_worktree=True, need_index=True)
    mocked.build_index.assert_not_called()


def test_no_index_skips_build(mocked):
    state._write_state({"last_fetch": time.time()})
    mocked.read_manifest.return_value = types.SimpleNamespace(commit="oldsha")
    state.ensure_ready(sync_worktree=True, no_index=True)
    mocked.build_index.assert_not_called()


def test_no_index_without_existing_index_raises(mocked):
    state._write_state({"last_fetch": time.time()})
    mocked.read_manifest.return_value = None
    mocked.index_exists.return_value = False
    with pytest.raises(RuntimeError):
        state.ensure_ready(sync_worktree=True, no_index=True, need_index=True)
