"""CLI surface: help, list-versions (text + JSON), and search JSON output.
ensure_ready and the core lookups are mocked so nothing touches git or network."""

from __future__ import annotations

import json

import pytest
from typer.testing import CliRunner

import sndoc.core.fetch as fetch_mod
import sndoc.core.search as search_mod
import sndoc.state as state_mod
from sndoc.cli import app
from sndoc.core.models import FetchResult, SearchHit, VersionInfo

runner = CliRunner()


@pytest.fixture(autouse=True)
def no_side_effects(mocker):
    mocker.patch.object(state_mod, "ensure_ready")


def test_help_lists_commands():
    result = runner.invoke(app, ["--help"])
    assert result.exit_code == 0
    for cmd in ("search", "fetch", "fetch-url", "list-versions", "index", "update", "serve"):
        assert cmd in result.output


def test_list_versions_text(mocker):
    mocker.patch.object(
        fetch_mod,
        "list_versions",
        return_value=[
            VersionInfo(release="zurich", is_latest=True),
            VersionInfo(release="yokohama", is_latest=False),
        ],
    )
    result = runner.invoke(app, ["list-versions"])
    assert result.exit_code == 0
    assert "zurich" in result.output and "(latest)" in result.output
    assert "yokohama" in result.output


def test_list_versions_json(mocker):
    mocker.patch.object(
        fetch_mod,
        "list_versions",
        return_value=[VersionInfo(release="zurich", is_latest=True)],
    )
    result = runner.invoke(app, ["list-versions", "--json"])
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert data == [{"release": "zurich", "is_latest": True}]


def test_search_json(mocker):
    hit = SearchHit(
        path="api/glide-record.md",
        title="GlideRecord",
        breadcrumb="API",
        anchor="",
        release="zurich",
        url="https://example.com/r/api/glide-record",
        snippet="GlideRecord is...",
        score=0.5,
    )
    mocker.patch.object(search_mod, "search", return_value=[hit])
    result = runner.invoke(app, ["search", "glide record", "--json"])
    assert result.exit_code == 0
    data = json.loads(result.output)
    assert data[0]["path"] == "api/glide-record.md"


def _fetch_result() -> FetchResult:
    return FetchResult(
        markdown="# Doc\n\nbody",
        source_url="https://example.com/r/api/glide-record",
        path="api/glide-record.md",
        release="zurich",
    )


def test_fetch_defaults_to_local(mocker):
    m = mocker.patch.object(fetch_mod, "fetch", return_value=_fetch_result())
    result = runner.invoke(app, ["fetch", "api/glide-record.md"])
    assert result.exit_code == 0
    assert m.call_args.kwargs.get("live") is False


def test_fetch_live_flag_passes_through(mocker):
    m = mocker.patch.object(fetch_mod, "fetch", return_value=_fetch_result())
    result = runner.invoke(app, ["fetch", "api/glide-record.md", "--live"])
    assert result.exit_code == 0
    assert m.call_args.kwargs.get("live") is True


def test_fetch_url_live_flag_passes_through(mocker):
    m = mocker.patch.object(fetch_mod, "fetch", return_value=_fetch_result())
    result = runner.invoke(app, ["fetch-url", "r/api/glide-record", "--live"])
    assert result.exit_code == 0
    assert m.call_args.kwargs.get("live") is True
