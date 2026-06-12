"""MCP stdio server: exposes the same capabilities as the CLI subcommands over
stdio via the official MCP SDK (FastMCP), reusing the shared core. For use in
Claude Code, Claude Desktop, or the MCP inspector. Run with `sndoc serve`.
"""

from __future__ import annotations

import sys

from mcp.server.fastmcp import FastMCP

from .core import fetch as docs
from .core.format import format_fetch, format_search, format_versions
from .core.search import search

mcp = FastMCP("sndoc-mcp")


@mcp.tool(
    name="search_servicenow_docs",
    description=(
        "Semantic + keyword search over the official ServiceNow product "
        "documentation (the latest release). Returns top matching topics with a "
        "repo `path` to fetch. Use for any question about how a ServiceNow "
        "feature, API, table, or behavior works."
    ),
)
def search_servicenow_docs(query: str) -> str:
    return format_search(search(query))


@mcp.tool(
    name="fetch_servicenow_doc",
    description=(
        "Fetch a ServiceNow documentation topic as clean Markdown by its `path` "
        "(from a search result). Optionally pass `version` (a release name from "
        "list_servicenow_versions) to read a specific release; defaults to latest. "
        "Reads from the local docs clone by default; pass `live=true` to read live "
        "from GitHub instead."
    ),
)
def fetch_servicenow_doc(
    path: str, version: str | None = None, live: bool = False
) -> str:
    return format_fetch(docs.fetch(path, version, live=live))


@mcp.tool(
    name="fetch_servicenow_doc_by_url",
    description=(
        "Fetch a ServiceNow documentation topic as clean Markdown, given a "
        "docs.servicenow.com URL or an 'r/...' reader path. Reads from the local "
        "docs clone by default; pass `live=true` to read live from GitHub instead."
    ),
)
def fetch_servicenow_doc_by_url(url: str, live: bool = False) -> str:
    return format_fetch(docs.fetch(url, live=live))


@mcp.tool(
    name="list_servicenow_versions",
    description=(
        "List the available ServiceNow documentation versions (release branches), "
        "newest first. Use the names with `fetch_servicenow_doc`'s `version` arg."
    ),
)
def list_servicenow_versions() -> str:
    return format_versions(docs.list_versions())


def serve() -> None:
    """Ensure the clone + index are ready, then run the stdio transport."""
    from .state import ensure_ready

    ensure_ready(sync_worktree=True, need_index=True)
    # stdio transport owns stdout; keep diagnostics on stderr.
    print("sndoc-mcp server ready (stdio).", file=sys.stderr)
    mcp.run()
