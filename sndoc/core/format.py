"""Shared text formatting for results, used by both the CLI and the MCP
stdio server so human-facing output is identical everywhere.
"""

from __future__ import annotations

from .models import FetchResult, SearchHit, VersionInfo


def format_search(hits: list[SearchHit]) -> str:
    if not hits:
        return "No ServiceNow documentation results found."

    lines = ["ServiceNow documentation search results:\n"]
    for i, h in enumerate(hits):
        where = f" — {h.breadcrumb}" if h.breadcrumb else ""
        lines.append(f"{i + 1}. {h.title}{where} ({h.release})")
        if h.snippet:
            lines.append(f"   {h.snippet}")
        lines.append(f"   url: {h.url}")
        lines.append(f"   path: {h.path}")
        lines.append("")
    lines.append(
        "To read a result, fetch it with `fetch_servicenow_doc` (pass its `path`; "
        "add `version` for a specific release)."
    )
    return "\n".join(lines)


def format_fetch(res: FetchResult) -> str:
    return f"> Source: {res.source_url} (release: {res.release})\n\n{res.markdown}"


def format_versions(versions: list[VersionInfo]) -> str:
    if not versions:
        return "No ServiceNow documentation versions found."
    lines = ["ServiceNow documentation versions (newest first):\n"]
    for v in versions:
        lines.append(f"- {v.release}{'  (latest)' if v.is_latest else ''}")
    lines.append(
        "\nSearch covers the latest release. Fetch any version with "
        "`fetch_servicenow_doc` and a `version`."
    )
    return "\n".join(lines)
