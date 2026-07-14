//! Shared constants and data-dir paths for the ServiceNow docs core.
//!
//! The single source of truth is the official GitHub docs mirror,
//! github.com/ServiceNow/ServiceNowDocs: one branch per release, markdown under
//! `markdown/**` with YAML frontmatter. The CLI keeps a local clone of that
//! repo, builds a hybrid index (SQLite FTS5 + sqlite-vec) of the latest
//! release, and fetches any release's raw markdown on demand.
//!
//! Everything the CLI writes lives under a per-user data directory (see
//! [`data_dir`]), overridable with `SNDOC_DATA_DIR` so tests and power users can
//! relocate it. Paths are resolved lazily (functions, not statics) so the
//! override can be set after startup — e.g. by the CLI's `--data-dir` flag.

use std::env;
use std::path::PathBuf;

pub const GITHUB_REPO: &str = "ServiceNow/ServiceNowDocs";

pub fn github_raw_base() -> String {
    format!("https://raw.githubusercontent.com/{GITHUB_REPO}")
}

pub fn git_url() -> String {
    format!("https://github.com/{GITHUB_REPO}.git")
}

/// All doc files live under this prefix in every branch.
pub const MARKDOWN_PREFIX: &str = "markdown/";

/// Human-facing docs site, used to build citation URLs when a file has no
/// explicit `canonical_url` in its frontmatter.
pub const DOCS_BASE_URL: &str = "https://www.servicenow.com/docs";

/// Fallback branch when the clone reports no branches (should not happen).
pub const DEFAULT_BRANCH: &str = "main";

/// Embedding model (model2vec static embeddings — no transformer forward pass).
/// potion-retrieval-32M is the best-performing static retrieval model; 512-dim,
/// L2-normalized. Unlike bge, potion is symmetric: NO query prefix is applied.
/// The indexer and the query side must use the same model so vectors are
/// comparable. Downloaded from Hugging Face on first use and cached.
pub const DEFAULT_EMBED_MODEL: &str = "minishlab/potion-retrieval-32M";
pub const EMBED_DIM: usize = 512;

pub fn embed_model() -> String {
    env::var("SNDOC_EMBED_MODEL").unwrap_or_else(|_| DEFAULT_EMBED_MODEL.to_string())
}

/// Refresh the local clone at most this often on the auto-update path (the
/// `update` subcommand forces a refresh regardless).
pub const UPDATE_INTERVAL_S: f64 = 86_400.0; // 24 h

/// Self-heal re-clone throttle. A re-clone is a full download of the whole
/// docs repo, so after one automatic re-clone we won't attempt another for
/// this long even if the clone still looks incomplete — at most once a month.
pub const RECLONE_INTERVAL_S: f64 = 30.0 * 86_400.0; // ~30 days

pub const HTTP_TIMEOUT_S: u64 = 30;

/// Whether docs should be fetched live over HTTP by default instead of from the
/// local clone. Set `SNDOC_FETCH_SOURCE=live` to flip; default `local`. Read on
/// every call so the env var (and tests) can change it after startup.
pub fn fetch_live_default() -> bool {
    env::var("SNDOC_FETCH_SOURCE")
        .map(|v| v.trim().to_lowercase() == "live")
        .unwrap_or(false)
}

/// Per-user data directory for the clone, index, and state.
///
/// Overridable with `SNDOC_DATA_DIR` (read on every call so the CLI's
/// `--data-dir` flag and tests can relocate it after startup). Matches
/// Python `platformdirs.user_data_dir("sndoc")`: Linux `~/.local/share/sndoc`,
/// macOS `~/Library/Application Support/sndoc`, Windows `%LOCALAPPDATA%\sndoc`.
pub fn data_dir() -> PathBuf {
    if let Ok(override_dir) = env::var("SNDOC_DATA_DIR") {
        if !override_dir.is_empty() {
            return PathBuf::from(override_dir);
        }
    }
    let base = directories::BaseDirs::new()
        .map(|d| d.data_local_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    base.join("sndoc")
}

/// Local git clone of the ServiceNowDocs mirror.
pub fn repo_dir() -> PathBuf {
    data_dir().join("repo")
}

pub fn index_dir() -> PathBuf {
    data_dir().join("index")
}

/// The hybrid search index (SQLite).
pub fn index_db_path() -> PathBuf {
    index_dir().join("latest.db")
}

/// Manifest describing the built index (branch, commit, counts, model).
pub fn manifest_path() -> PathBuf {
    index_dir().join("manifest.json")
}

/// Small JSON file tracking `last_fetch` for the daily-refresh throttle.
pub fn state_path() -> PathBuf {
    data_dir().join("state.json")
}
