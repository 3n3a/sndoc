"""Shared constants and data-dir paths for the ServiceNow docs core.

The single source of truth is the official GitHub docs mirror,
github.com/ServiceNow/ServiceNowDocs: one branch per release, markdown under
`markdown/**` with YAML frontmatter. The CLI keeps a local clone of that repo,
builds a hybrid index (SQLite FTS5 + sqlite-vec) of the latest release, and
fetches any release's raw markdown on demand.

Everything the CLI writes lives under a per-user data directory (see
`data_dir()`), overridable with `SNDOC_DATA_DIR` so tests and power users can
relocate it. Paths are resolved lazily (functions, not module constants) so the
override can be set after import — e.g. by the CLI's `--data-dir` flag.
"""

from __future__ import annotations

import os
from pathlib import Path

import platformdirs

GITHUB_REPO = "ServiceNow/ServiceNowDocs"
GITHUB_RAW_BASE = f"https://raw.githubusercontent.com/{GITHUB_REPO}"
GIT_URL = f"https://github.com/{GITHUB_REPO}.git"

# All doc files live under this prefix in every branch.
MARKDOWN_PREFIX = "markdown/"

# Human-facing docs site, used to build citation URLs when a file has no
# explicit `canonical_url` in its frontmatter.
DOCS_BASE_URL = "https://www.servicenow.com/docs"

# Fallback branch when the clone reports no branches (should not happen).
DEFAULT_BRANCH = "main"

# Embedding model (model2vec static embeddings — no transformer forward pass, no
# onnxruntime). potion-retrieval-32M is the best-performing static retrieval
# model; 512-dim, L2-normalized. Unlike bge, potion is symmetric: NO query
# prefix is applied. The indexer and the query side must use the same model so
# vectors are comparable. Downloaded from Hugging Face on first use and cached.
EMBED_MODEL = os.environ.get("SNDOC_EMBED_MODEL", "minishlab/potion-retrieval-32M")
EMBED_DIM = 512

# Refresh the local clone at most this often on the auto-update path (the
# `update` subcommand forces a refresh regardless).
UPDATE_INTERVAL_S = 86_400  # 24 h

HTTP_TIMEOUT_S = 30.0


def fetch_live_default() -> bool:
    """Whether docs should be fetched live over HTTP by default instead of from the
    local clone. Set SNDOC_FETCH_SOURCE=live to flip; default 'local'. Read on every
    call so the env var (and tests) can change it after import."""
    return os.environ.get("SNDOC_FETCH_SOURCE", "local").strip().lower() == "live"


def data_dir() -> Path:
    """Per-user data directory for the clone, index, and state.

    Overridable with SNDOC_DATA_DIR (read on every call so the CLI's
    `--data-dir` flag and tests can relocate it after import)."""
    override = os.environ.get("SNDOC_DATA_DIR")
    if override:
        return Path(override)
    return Path(platformdirs.user_data_dir("sndoc"))


def repo_dir() -> Path:
    """Local git clone of the ServiceNowDocs mirror."""
    return data_dir() / "repo"


def index_dir() -> Path:
    return data_dir() / "index"


def index_db_path() -> Path:
    """The hybrid search index (SQLite)."""
    return index_dir() / "latest.db"


def manifest_path() -> Path:
    """Manifest describing the built index (branch, commit, counts, model)."""
    return index_dir() / "manifest.json"


def state_path() -> Path:
    """Small JSON file tracking last_fetch for the daily-refresh throttle."""
    return data_dir() / "state.json"
