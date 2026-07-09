"""MCP server startup: fetch defaults to live HTTP so tool calls never block on
a git subprocess (the Windows/Claude Desktop timeout). ensure_ready + the stdio
transport are stubbed so the test touches neither git nor the network."""

from __future__ import annotations

import os

import pytest

from sndoc import mcp_server


@pytest.fixture(autouse=True)
def _stub_runtime(monkeypatch):
    """Neuter the two side effects of serve(): clone/index setup and the transport."""
    monkeypatch.setattr("sndoc.state.ensure_ready", lambda **kwargs: None)
    monkeypatch.setattr(mcp_server.mcp, "run", lambda: None)


def test_serve_defaults_fetch_source_to_live(monkeypatch):
    monkeypatch.delenv("SNDOC_FETCH_SOURCE", raising=False)
    mcp_server.serve()
    assert os.environ["SNDOC_FETCH_SOURCE"] == "live"


def test_serve_respects_explicit_fetch_source(monkeypatch):
    monkeypatch.setenv("SNDOC_FETCH_SOURCE", "local")
    mcp_server.serve()
    assert os.environ["SNDOC_FETCH_SOURCE"] == "local"
