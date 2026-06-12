"""fetch() source selection: local clone by default, live HTTP on --live /
SNDOC_FETCH_SOURCE=live, branch resolution, and the no-clone fallback. The two
readers and branch lookups are stubbed so nothing touches git or the network."""

from __future__ import annotations

import pytest

import sndoc.core.fetch as fetch_mod
from sndoc.core.repo import RawDoc


@pytest.fixture
def patched(monkeypatch):
    """Stub branch resolution + both readers; record which reader is used."""
    monkeypatch.delenv("SNDOC_FETCH_SOURCE", raising=False)
    monkeypatch.setattr(fetch_mod, "branch_names", lambda: {"zurich", "yokohama"})
    monkeypatch.setattr(fetch_mod, "resolve_latest_branch", lambda: "zurich")
    monkeypatch.setattr(fetch_mod, "is_cloned", lambda: True)
    calls = {"local": [], "live": []}

    def fake_local(repo_path, branch):
        calls["local"].append((repo_path, branch))
        return RawDoc(markdown="# Doc\n\nfrom clone\n", raw_url="local-url")

    def fake_live(repo_path, branch):
        calls["live"].append((repo_path, branch))
        return RawDoc(markdown="# Doc\n\nfrom http\n", raw_url="live-url")

    monkeypatch.setattr(fetch_mod, "read_doc_from_clone", fake_local)
    monkeypatch.setattr(fetch_mod, "fetch_raw_markdown", fake_live)
    return calls


def test_default_reads_local_clone(patched):
    res = fetch_mod.fetch("api/glide-record.md")
    assert "from clone" in res.markdown
    assert patched["local"] and not patched["live"]


def test_live_flag_reads_http(patched):
    res = fetch_mod.fetch("api/glide-record.md", live=True)
    assert "from http" in res.markdown
    assert patched["live"] and not patched["local"]


def test_env_source_live_flips_default(patched, monkeypatch):
    monkeypatch.setenv("SNDOC_FETCH_SOURCE", "live")
    res = fetch_mod.fetch("api/glide-record.md")
    assert "from http" in res.markdown
    assert patched["live"] and not patched["local"]


def test_version_resolves_branch_passed_to_reader(patched):
    fetch_mod.fetch("api/glide-record.md", version="yokohama")
    assert patched["local"][0] == ("api/glide-record.md", "yokohama")


def test_local_miss_falls_back_to_live_only_when_no_clone(monkeypatch):
    monkeypatch.delenv("SNDOC_FETCH_SOURCE", raising=False)
    monkeypatch.setattr(fetch_mod, "branch_names", lambda: {"zurich"})
    monkeypatch.setattr(fetch_mod, "resolve_latest_branch", lambda: "zurich")
    monkeypatch.setattr(fetch_mod, "read_doc_from_clone", lambda p, b: None)
    monkeypatch.setattr(fetch_mod, "is_cloned", lambda: False)
    live_hits = []
    monkeypatch.setattr(
        fetch_mod,
        "fetch_raw_markdown",
        lambda p, b: live_hits.append((p, b))
        or RawDoc(markdown="# Doc\n\nbody\n", raw_url="u"),
    )
    res = fetch_mod.fetch("api/glide-record.md")
    assert live_hits == [("api/glide-record.md", "zurich")]
    assert res.markdown


def test_local_miss_with_clone_raises_without_falling_back(monkeypatch):
    monkeypatch.delenv("SNDOC_FETCH_SOURCE", raising=False)
    monkeypatch.setattr(fetch_mod, "branch_names", lambda: {"zurich"})
    monkeypatch.setattr(fetch_mod, "resolve_latest_branch", lambda: "zurich")
    monkeypatch.setattr(fetch_mod, "read_doc_from_clone", lambda p, b: None)
    monkeypatch.setattr(fetch_mod, "is_cloned", lambda: True)

    def must_not_run(p, b):
        raise AssertionError("must not fall back to live when the clone is present")

    monkeypatch.setattr(fetch_mod, "fetch_raw_markdown", must_not_run)
    with pytest.raises(ValueError, match="No doc found"):
        fetch_mod.fetch("api/glide-record.md")
