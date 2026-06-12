"""Shared fixtures. Every test runs against an isolated, empty data dir and the
network is never touched (git/httpx are mocked where needed)."""

from __future__ import annotations

import pytest


@pytest.fixture(autouse=True)
def isolated_data_dir(tmp_path, monkeypatch):
    """Point SNDOC_DATA_DIR at a fresh tmp dir and reset cached module globals."""
    monkeypatch.setenv("SNDOC_DATA_DIR", str(tmp_path / "data"))

    # Drop any cached index store / model so a previous test can't leak a handle
    # to a now-deleted db.
    import sndoc.core.search as search_mod
    import sndoc.core.embed as embed_mod

    search_mod._store = None
    embed_mod._model = None
    yield
    search_mod._store = None
    embed_mod._model = None
