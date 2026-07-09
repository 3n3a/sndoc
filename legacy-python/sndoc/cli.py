"""sndoc — local-first CLI for ServiceNow product documentation.

Hybrid search + fetch as Markdown over the official ServiceNow docs mirror,
usable by humans and AI agents. On first run it clones the docs repo; it
refreshes daily and reindexes when the docs change. The same capabilities are
available over MCP via `sndoc serve`.
"""

from __future__ import annotations

import dataclasses
import json as jsonlib
import os
from typing import Optional

import typer

app = typer.Typer(
    no_args_is_help=True,
    add_completion=False,
    help="ServiceNow documentation search & fetch (CLI + MCP).",
)


class _Settings:
    no_index: bool = False


settings = _Settings()


def _pkg_version() -> str:
    from importlib.metadata import PackageNotFoundError, version

    try:
        return version("sndoc-mcp")
    except PackageNotFoundError:
        return "unknown"


def _version_callback(value: bool) -> None:
    if value:
        typer.echo(f"sndoc {_pkg_version()}")
        raise typer.Exit()


@app.callback()
def _main(
    data_dir: Optional[str] = typer.Option(
        None,
        "--data-dir",
        envvar="SNDOC_DATA_DIR",
        help="Override the data directory (clone, index, state).",
    ),
    no_index: bool = typer.Option(
        False,
        "--no-index",
        help="Skip building/rebuilding the index on the auto-update path.",
    ),
    version: bool = typer.Option(
        False,
        "--version",
        "-V",
        help="Show the installed sndoc version and exit.",
        callback=_version_callback,
        is_eager=True,
    ),
) -> None:
    if data_dir:
        os.environ["SNDOC_DATA_DIR"] = data_dir
    settings.no_index = no_index


def _dump(obj) -> str:
    if dataclasses.is_dataclass(obj):
        return jsonlib.dumps(dataclasses.asdict(obj), indent=2)
    if isinstance(obj, list):
        return jsonlib.dumps([dataclasses.asdict(o) for o in obj], indent=2)
    return jsonlib.dumps(obj, indent=2)


@app.command()
def search(
    query: str = typer.Argument(..., help="Natural-language search query."),
    limit: int = typer.Option(8, "--limit", "-n", help="Max results."),
    json: bool = typer.Option(False, "--json", help="Emit JSON for agents."),
) -> None:
    """Hybrid search over the latest ServiceNow release."""
    from .core.format import format_search
    from .core.search import search as search_docs
    from .state import ensure_ready

    ensure_ready(no_index=settings.no_index, sync_worktree=True, need_index=True)
    hits = search_docs(query, limit)
    typer.echo(_dump(hits) if json else format_search(hits))


@app.command()
def fetch(
    path: str = typer.Argument(..., help="Repo path from a search result."),
    version: Optional[str] = typer.Option(
        None, "--version", "-v", help="Release name (default: latest)."
    ),
    live: bool = typer.Option(
        False, "--live", help="Fetch live from GitHub instead of the local clone."
    ),
    json: bool = typer.Option(False, "--json", help="Emit JSON for agents."),
) -> None:
    """Fetch a documentation topic as clean Markdown by its repo path."""
    from .core import fetch as docs
    from .core.format import format_fetch
    from .state import ensure_ready

    ensure_ready(no_index=settings.no_index, sync_worktree=False)
    res = docs.fetch(path, version, live=live)
    typer.echo(_dump(res) if json else format_fetch(res))


@app.command("fetch-url")
def fetch_url(
    url: str = typer.Argument(..., help="docs.servicenow.com URL or 'r/...' path."),
    live: bool = typer.Option(
        False, "--live", help="Fetch live from GitHub instead of the local clone."
    ),
    json: bool = typer.Option(False, "--json", help="Emit JSON for agents."),
) -> None:
    """Fetch a topic as clean Markdown given a docs URL or reader path."""
    from .core import fetch as docs
    from .core.format import format_fetch
    from .state import ensure_ready

    ensure_ready(no_index=settings.no_index, sync_worktree=False)
    res = docs.fetch(url, live=live)
    typer.echo(_dump(res) if json else format_fetch(res))


@app.command("list-versions")
def list_versions(
    json: bool = typer.Option(False, "--json", help="Emit JSON for agents."),
) -> None:
    """List available ServiceNow release versions (newest first)."""
    from .core import fetch as docs
    from .core.format import format_versions
    from .state import ensure_ready

    ensure_ready(no_index=settings.no_index, sync_worktree=False)
    versions = docs.list_versions()
    typer.echo(_dump(versions) if json else format_versions(versions))


@app.command()
def index(
    branch: Optional[str] = typer.Option(
        None, "--branch", "-b", help="Release branch to index (default: latest)."
    ),
    force: bool = typer.Option(
        False, "--force", "-f", help="Rebuild even if already up to date."
    ),
) -> None:
    """Build or rebuild the search index from the local clone."""
    from .core import repo
    from . import index as indexer
    from .state import ensure_ready

    ensure_ready(no_index=True, sync_worktree=False)
    target = (branch or repo.resolve_latest_branch()).lower()
    if not force and indexer.index_exists():
        manifest = indexer.read_manifest()
        if manifest and manifest.commit == repo.branch_tip_commit(target):
            typer.echo(f"Index already up to date for '{target}'. Use --force to rebuild.")
            return
    manifest = indexer.build_index(branch=target)
    typer.echo(
        f"Indexed {manifest.chunk_count} chunks from {manifest.file_count} files "
        f"({manifest.branch} @ {manifest.commit[:8]})."
    )


@app.command()
def update(
    no_index: bool = typer.Option(
        False, "--no-index", help="Refresh the clone but skip reindexing."
    ),
) -> None:
    """Refresh the docs clone and reindex on change (cron/daemon entry point)."""
    from .state import ensure_ready

    ensure_ready(
        no_index=no_index or settings.no_index,
        force_update=True,
        sync_worktree=True,
    )
    typer.echo("Update complete.")


@app.command()
def serve() -> None:
    """Run the MCP stdio server (for Claude Code / Desktop / inspector)."""
    from .mcp_server import serve as run_server

    run_server()


@app.command()
def doctor() -> None:
    """Check the environment: sqlite-vec + FTS5, index, and clone status."""
    from .core import repo
    from . import index as indexer

    typer.echo(f"[ok] sndoc version: {_pkg_version()}")

    ok = True

    try:
        from .core.index_store import sqlite3
        import sqlite_vec

        conn = sqlite3.connect(":memory:")
        conn.enable_load_extension(True)
        sqlite_vec.load(conn)
        conn.enable_load_extension(False)
        (vec_version,) = conn.execute("SELECT vec_version()").fetchone()
        conn.execute("CREATE VIRTUAL TABLE v USING vec0(embedding float[4])")
        conn.execute("CREATE VIRTUAL TABLE f USING fts5(body)")
        conn.close()
        typer.echo(f"[ok] sqlite ({sqlite3.__name__}) + sqlite-vec {vec_version} + fts5")
    except Exception as err:  # noqa: BLE001
        ok = False
        typer.echo(f"[FAIL] sqlite-vec/fts5: {err}")

    typer.echo(f"[..] clone: {'present' if repo.is_cloned() else 'absent (run any command to clone)'}")

    manifest = indexer.read_manifest()
    if manifest:
        typer.echo(
            f"[ok] index: {manifest.branch} @ {manifest.commit[:8]} "
            f"({manifest.chunk_count} chunks, built {manifest.built_at})"
        )
    else:
        typer.echo("[..] index: not built yet (run `sndoc index`)")

    if not ok:
        raise typer.Exit(1)


if __name__ == "__main__":
    app()
