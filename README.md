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

Uses [Rust](https://www.rust-lang.org/) (stable) and a C compiler (for the
bundled SQLite + sqlite-vec).

```bash
cargo install --path .   # installs a global `sndoc` command
# or, in a checkout:
cargo build --release && ./target/release/sndoc --help
```

On first run `sndoc` clones the docs repo (a full clone via
[gitoxide](https://github.com/GitoxideLabs/gitoxide) — no `git` binary needed)
and downloads the embedding model (`minishlab/potion-retrieval-32M`, cached).
Everything it writes lives under the per-user data dir, e.g.
`~/.local/share/sndoc/` on Linux — override with `--data-dir` or `SNDOC_DATA_DIR`.

### Build the native binary locally

The release binaries are a single self-contained `cargo` build — the bundled
SQLite (with FTS5), the `sqlite-vec` `vec0` extension, and the git client
(gitoxide) are all compiled in, so there is no loadable extension and no `git`
binary at runtime. The embedding model is **not** bundled — it is downloaded from
Hugging Face on first run.

```bash
cargo build --release        # produces target/release/sndoc[.exe]
# Windows installer (requires Inno Setup):
iscc installer\installer.iss
```

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

`fetch`/`fetch-url` read from the **local clone** by default — fully offline for
every release, since the full clone has all branches' blobs (docs are read
straight from the git object store, no working-tree checkout). Pass `--live` to
read live from `raw.githubusercontent.com` instead, or set
`SNDOC_FETCH_SOURCE=live` to make live the default.

```bash
sndoc search "how to query a GlideRecord"
sndoc fetch administer/reference-pages/concept/c_GlideRecordQueries.md
sndoc fetch-url https://www.servicenow.com/docs/r/administer/.../c_Foo
sndoc list-versions --json
```

## How it works

```
 first run / daily / sndoc update
   ├─ clone ServiceNowDocs via gitoxide (full clone: all refs + history + blobs, no git binary)
   ├─ fetch (throttled to once/24h on the auto path; forced by `update`)
   └─ if latest-branch commit != indexed commit → reindex
         read markdown/** from the object store → chunk by heading → embed (model2vec, local)
                                                → SQLite (FTS5 + sqlite-vec)
                                                  │  data dir: index/latest.db + manifest.json
 search ─┐  core/search.rs ── core/index_store.rs (rusqlite + sqlite-vec, hybrid RRF)
 fetch  ─┤                 └─ core/embed.rs (model2vec-rs query embedding)
 list   ─┘  core/repo.rs ── gix refs (versions, newest by commit date) · raw.githubusercontent.com (--live)
```

- **Index.** On change, the CLI reads every `markdown/**` file on the latest
  release branch straight from the git object store, chunks by heading, embeds
  each chunk with a **local**
  [model2vec](https://github.com/MinishLab/model2vec) static model
  (`minishlab/potion-retrieval-32M`, 512-dim — a token→vector lookup + mean pool,
  no transformer forward pass, no API) via
  [model2vec-rs](https://github.com/MinishLab/model2vec-rs), and builds a SQLite
  file with an **FTS5** (BM25) table and a **`sqlite-vec`** vector table.
- **Search (hybrid).** Embeds the query, runs BM25 + vector KNN, and fuses them
  with **Reciprocal Rank Fusion**. Exact-term queries (`gliderecord`) lean on
  BM25; conceptual ones lean on the vector arm. Results are deduped to the best
  chunk per file.
- **Fetch & versions.** Markdown is read from the local clone's object store on
  the requested branch (latest by default), or from `raw.githubusercontent.com`
  with `--live`. Versions are the clone's release branches, ordered
  **newest-first by tip commit date** (no hardcoded release list); the repo's
  default branch (origin/HEAD) is the latest.

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

> **Note:** the MCP server defaults to fetching doc bodies **live over HTTP** from
> GitHub (`SNDOC_FETCH_SOURCE=live` under `serve`) so a `fetch` tool call never
> waits on a large clone that hasn't finished yet. Search is unaffected (it reads
> the local index). Set `SNDOC_FETCH_SOURCE=local` to force clone-backed reads
> (fully offline once the clone exists).

> **Heads up:** MCP hosts spawn the server with their own stripped environment,
> not your interactive shell's `PATH`. A bare `sndoc` command therefore often
> fails with `spawn sndoc ENOENT` — the host can't find the executable that
> `cargo install` put in `~/.cargo/bin` (or the Windows installer put in
> `C:\Program Files (x86)\sndoc`). The fix is to give the host a command it can
> resolve: an absolute path to the binary.

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
// macOS/Linux (cargo install → ~/.cargo/bin)
{ "mcpServers": { "sndoc": { "command": "/Users/you/.cargo/bin/sndoc", "args": ["serve"] } } }
```

```jsonc
// Windows (installer → C:\Program Files (x86)\sndoc)
{ "mcpServers": { "sndoc": { "command": "C:\\Program Files (x86)\\sndoc\\sndoc.exe", "args": ["serve"] } } }
```

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
cargo build
cargo test                     # offline test suite (no git/network)
cargo run -- doctor            # verify sqlite-vec + FTS5 load
```

## Layout

```
src/main.rs                    — clap CLI (entry point: `sndoc`)
src/state.rs                   — lifecycle: clone, daily refresh, reindex-on-change
src/index.rs                   — build the search index (chunk → embed → SQLite)
src/mcp.rs                     — MCP stdio server (rmcp), shares the core
src/core/      search, fetch, repo, embed, chunk, index_store, models, format,
               http, constants — git (gitoxide) + raw-fetch + hybrid index, no CLI/MCP deps
installer/installer.iss        — Windows installer (Inno Setup): PATH + skill + cache cleanup
.claude/skills/sndoc/SKILL.md  — auto-invoked Claude skill driving the CLI
.github/workflows/test.yml     — CI: cargo build + test
.github/workflows/release.yml  — CI: 3-platform cargo build + Windows installer on v* tags
legacy-python/                 — the previous Python implementation, archived
```
