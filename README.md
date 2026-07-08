# sndoc

A local-first **CLI** for the official **ServiceNow product documentation** —
**hybrid semantic + keyword search** and fetch as clean Markdown — usable by both
humans and AI agents. The single source of truth is the
[`ServiceNow/ServiceNowDocs`](https://github.com/ServiceNow/ServiceNowDocs)
GitHub mirror, which the CLI clones on first run, refreshes daily, and reindexes
whenever the docs change. The same capabilities are also exposed over **MCP**
(`sndoc serve`) for use in Claude Code / Desktop.

## Install

### Pre-built binary (recommended)

Download the latest release from [GitHub Releases](../../releases):

- **Windows**: `sndoc-setup.exe` (installer) or `sndoc-windows-amd64.exe` (portable)
- **Linux**: `sndoc-linux-amd64`
- **macOS**: `sndoc-macos-amd64`

The Windows installer adds `sndoc` to your PATH and can install the Claude skill for you.

### From source

Uses [uv](https://docs.astral.sh/uv/). Python 3.12+.

```bash
uv tool install .        # installs a global `sndoc` command
# or, in a checkout:
uv sync && uv run sndoc --help
```

On first run `sndoc` clones the docs repo (a blobless partial clone — cheap) and
downloads the embedding model (`minishlab/potion-retrieval-32M`, ~123 MB, cached).
Everything it writes lives under the per-user data dir (`platformdirs`), e.g.
`~/.local/share/sndoc/` on Linux — override with `--data-dir` or `SNDOC_DATA_DIR`.

### Build the native binary locally

The release binaries are built with [Nuitka](https://nuitka.net/). To reproduce a
build locally:

```bash
uv sync --group dev
uv run python -m nuitka --onefile --follow-imports \
  --include-package=sndoc --include-package-data=sqlite_vec \
  --include-package=model2vec --include-package=tokenizers --include-package=numpy \
  --output-dir=dist --output-filename=sndoc --assume-yes-for-downloads sndoc_main.py
# Windows installer (requires Inno Setup):
iscc installer\installer.iss
```

`--include-package-data=sqlite_vec` is required: it bundles the `vec0` loadable
extension the search index opens at runtime. The embedding model itself is **not**
bundled — it is still downloaded from Hugging Face on first run. Produces
`dist/sndoc[.exe]` and (on Windows) `dist/sndoc-setup.exe`.
[`.github/workflows/release.yml`](.github/workflows/release.yml) builds all three
platforms on `v*` tags.

## Commands

| Command | What it does |
| --- | --- |
| `sndoc search "<query>" [--limit N] [--json]` | Hybrid search over the latest release; returns topics with a repo `path` to fetch |
| `sndoc fetch <path> [--version <release>] [--live] [--json]` | Fetch a topic as Markdown by its repo `path` |
| `sndoc fetch-url <url> [--live] [--json]` | Fetch a topic from a `docs.servicenow.com` URL or an `r/...` reader path |
| `sndoc list-versions [--json]` | Available release versions, newest first (latest flagged) |
| `sndoc index [--branch <release>] [--force]` | Build/rebuild the search index from the local clone |
| `sndoc update [--no-index]` | Refresh the clone and reindex on change — the cron/daemon entry point |
| `sndoc serve` | Run the MCP stdio server (same capabilities, for AI agents) |
| `sndoc doctor` | Check sqlite-vec + FTS5, the index, and clone status |

Pass `--json` to any read command for structured output (agent-friendly). Global
flags `--data-dir` and `--no-index` go before the subcommand
(`sndoc --no-index update`); `update` also accepts `--no-index` directly.

`fetch`/`fetch-url` read from the **local clone** by default — offline for the latest
release, and for any other release the blobless clone lazily fetches just the one blob
it needs (no checkout). Pass `--live` to read live from `raw.githubusercontent.com`
instead, or set `SNDOC_FETCH_SOURCE=live` to make live the default.

```bash
sndoc search "how to query a GlideRecord"
sndoc fetch administer/reference-pages/concept/c_GlideRecordQueries.md
sndoc fetch-url https://www.servicenow.com/docs/r/administer/.../c_Foo
sndoc list-versions --json
```

## How it works

```
 first run / daily / sndoc update
   ├─ git clone --filter=blob:none ServiceNowDocs   (blobless: refs + history, lazy blobs)
   ├─ git fetch (throttled to once/24h on the auto path; forced by `update`)
   └─ if latest-branch commit != indexed commit → reindex
         clone(latest) → chunk by heading → embed (model2vec, local) → SQLite (FTS5 + sqlite-vec)
                                                            │  data dir: index/latest.db + manifest.json
 search ─┐  search.py ── index_store.py (pysqlite3 + sqlite-vec, hybrid RRF)
 fetch  ─┤            └─ embed.py (model2vec query embedding)
 list   ─┘  repo.py ── git refs (versions, newest by commit date) · raw.githubusercontent.com (fetch)
```

- **Index.** On change, the CLI walks every `markdown/**` file on the latest
  release branch, chunks by heading, embeds each chunk with a **local**
  [model2vec](https://github.com/MinishLab/model2vec) static model
  (`minishlab/potion-retrieval-32M`, 512-dim — a token→vector lookup + mean pool,
  no transformer forward pass, no API), and builds a SQLite file with an **FTS5**
  (BM25) table and a **`sqlite-vec`** vector table.
- **Search (hybrid).** Embeds the query, runs BM25 + vector KNN, and fuses them
  with **Reciprocal Rank Fusion**. Exact-term queries (`gliderecord`) lean on
  BM25; conceptual ones lean on the vector arm. Results are deduped to the best
  chunk per file.
- **Fetch & versions.** Markdown is read from `raw.githubusercontent.com` on the
  requested branch (latest by default) — no working-tree churn. Versions are the
  clone's release branches, ordered **newest-first by tip commit date** (no
  hardcoded release list); the most recent is the latest.

## Daily refresh (daemon)

Any command auto-refreshes the clone at most once every 24 h. For an unattended
refresh, run `sndoc update` from cron or a systemd timer:

```cron
# refresh ServiceNow docs + reindex every day at 03:00
0 3 * * *  sndoc update
```

Use `sndoc update --no-index` to refresh the clone only (skip the re-embed).

## Use from Claude (MCP)

`sndoc serve` runs an MCP stdio server exposing the same four capabilities as
tools (`search_servicenow_docs`, `fetch_servicenow_doc`,
`fetch_servicenow_doc_by_url`, `list_servicenow_versions`).

> **Note:** the MCP server fetches doc bodies **live over HTTP** from GitHub
> (`SNDOC_FETCH_SOURCE=live` by default under `serve`), rather than shelling out
> to `git show` per request. This keeps fetch reliable on GUI hosts like Claude
> Desktop — especially on Windows, where a per-request git subprocess can hang on
> a credential prompt or an inherited stdio pipe. Search is unaffected (it reads
> the local index). Set `SNDOC_FETCH_SOURCE=local` to force clone-backed reads.

> **Heads up:** MCP hosts spawn the server with their own stripped environment,
> not your interactive shell's `PATH`. A bare `sndoc` command therefore often
> fails with `spawn sndoc ENOENT` — the host can't find the executable that
> `uv tool install` put in `~/.local/bin` (or the Windows installer put in
> `C:\Program Files (x86)\sndoc`). The fix is to give the host a command it can
> resolve: an absolute path, or `uv run`.

**Claude Code** — launched from a terminal, so it usually inherits your `PATH`:

```bash
claude mcp add sndoc -- sndoc serve
# If the host can't find it (spawn sndoc ENOENT), pass the absolute path:
claude mcp add sndoc -- "$(which sndoc)" serve       # macOS/Linux
```

**Claude Desktop** — a GUI app; it does **not** inherit your shell `PATH`, so use
the absolute path (find it with `which sndoc` / `where sndoc`) in
`claude_desktop_config.json`:

```jsonc
// macOS/Linux (uv tool install → ~/.local/bin)
{ "mcpServers": { "sndoc": { "command": "/Users/you/.local/bin/sndoc", "args": ["serve"] } } }
```

```jsonc
// Windows (installer → C:\Program Files (x86)\sndoc)
{ "mcpServers": { "sndoc": { "command": "C:\\Program Files (x86)\\sndoc\\sndoc.exe", "args": ["serve"] } } }
```

`.vscode/mcp.json` in this repo is already templated for VS Code (it uses
`uv run sndoc serve`, which resolves the tool from the project's synced venv).

## Claude skill

`.claude/skills/sndoc/SKILL.md` is an auto-invoked skill that tells Claude to reach
for the `sndoc` CLI whenever a `docs.servicenow.com` URL appears or ServiceNow docs
are requested — so it works even without the MCP server configured. Install it
globally (available in every project):

```bash
cp -r .claude/skills/sndoc ~/.claude/skills/sndoc
```

On Windows:

```powershell
Copy-Item -Recurse .claude\skills\sndoc "$env:USERPROFILE\.claude\skills\sndoc"
```

The Windows installer (`sndoc-setup.exe`) can do this for you. Create
`~/.claude/skills/` first if it does not exist.

## Configuration (env vars)

| Var | Default | Notes |
| --- | --- | --- |
| `SNDOC_DATA_DIR` | platform data dir | Where the clone, index, and state live (also `--data-dir`) |
| `SNDOC_EMBED_MODEL` | `minishlab/potion-retrieval-32M` | model2vec model (512-dim); must match between index build and query |
| `SNDOC_FETCH_SOURCE` | `local` | `local` reads docs from the clone (offline-capable); `live` reads from `raw.githubusercontent.com`. Per-command override: `--live` |

## Development

```bash
uv sync
uv run pytest          # offline test suite (git/network mocked)
uv run sndoc doctor    # verify sqlite-vec + FTS5 load
```

## Layout

```
sndoc/cli.py                   — Typer CLI (entry point: `sndoc`)
sndoc/state.py                 — lifecycle: clone, daily refresh, reindex-on-change
sndoc/index.py                 — build the search index (chunk → embed → SQLite)
sndoc/mcp_server.py            — MCP stdio server (FastMCP), shares the core
sndoc/core/      search, fetch, repo, embed, chunk, index_store, models, format,
                 constants — git + raw-fetch + hybrid index, no CLI/MCP deps
tests/                         — pytest suite (offline; git/httpx mocked)
sndoc_main.py                  — entry point for the Nuitka onefile binary
installer/installer.iss        — Windows installer (Inno Setup): PATH + skill + cache cleanup
.claude/skills/sndoc/SKILL.md  — auto-invoked Claude skill driving the CLI
.github/workflows/test.yml     — CI: uv sync + pytest
.github/workflows/release.yml  — CI: 3-platform Nuitka build + Windows installer on v* tags
```
