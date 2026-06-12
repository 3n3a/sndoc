# Bill of Materials — sndoc

`sndoc` is a self-contained local CLI: no cloud services, no hosted compute, no
embedding-API spend. The only external dependencies are the Python packages
below and two read-only network sources it pulls from on demand.

## Runtime dependencies

| Package | Purpose |
|---------|---------|
| `typer` | CLI framework (the `sndoc` command and its subcommands) |
| `platformdirs` | Resolve the per-user data directory (clone, index, state) |
| `model2vec` | Static embeddings (`minishlab/potion-retrieval-32M`, 512-dim) — token→vector lookup + mean pool, no transformer forward pass, no API |
| `sqlite-vec` | Vector KNN as a loadable SQLite extension |
| `pysqlite3-binary` (Linux only) | SQLite build with loadable-extension support + FTS5 (stdlib `sqlite3` on macOS/Windows already supports these) |
| `python-frontmatter` | Parse YAML frontmatter from each markdown doc |
| `httpx` | Fetch raw markdown from `raw.githubusercontent.com` |
| `numpy` | Embedding vector arrays |
| `mcp` | FastMCP SDK for the `sndoc serve` stdio server |

Dev-only: `pytest`, `pytest-mock`.

Exact resolved versions are pinned in `uv.lock`.

## External data sources (read-only, on demand)

| Source | Used for |
|--------|----------|
| `github.com/ServiceNow/ServiceNowDocs` (git) | Blobless clone of the docs; branch refs = release versions |
| `raw.githubusercontent.com/ServiceNow/ServiceNowDocs` | On-demand fetch of individual docs (any release) |
| Hugging Face (`minishlab/potion-retrieval-32M`) | One-time download of the embedding model on first run (cached) |

## Local footprint

- **Clone** (`<data>/repo`): blobless partial clone — refs + history, plus the
  checked-out latest branch's blobs.
- **Index** (`<data>/index/latest.db` + `manifest.json`): SQLite (FTS5 +
  sqlite-vec), a few hundred MB for a full release.
- **Model**: ~123 MB in the Hugging Face cache.

No always-on process; `sndoc update` (cron/systemd) is the only scheduled piece.
