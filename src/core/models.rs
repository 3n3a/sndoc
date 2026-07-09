//! Shared data structures for the ServiceNow docs core.

use serde::{Deserialize, Serialize};

/// One indexed markdown chunk (a heading-delimited slice of one file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexChunk {
    /// Repo path under markdown/, e.g. "api-reference/glide-record.md".
    pub path: String,
    /// Slugified heading, for deep links ("" for the lead section).
    pub anchor: String,
    /// Frontmatter title of the file.
    pub title: String,
    /// Frontmatter breadcrumb, " > "-joined.
    pub breadcrumb: String,
    /// This chunk's heading text.
    pub heading: String,
    /// Branch/release the chunk came from.
    pub release: String,
    /// The chunk body (markdown).
    pub content: String,
    /// Frontmatter canonical_url, or "" if absent.
    pub canonical_url: String,
    /// Frontmatter last_updated, or "".
    pub last_updated: String,
}

/// A search result: one topic (deduped to the best-matching chunk per file).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SearchHit {
    /// Repo path, pass to fetch.
    pub path: String,
    pub title: String,
    pub breadcrumb: String,
    pub anchor: String,
    pub release: String,
    /// Canonical docs URL to cite.
    pub url: String,
    /// Short excerpt of the matching chunk.
    pub snippet: String,
    /// Fused RRF score (higher is better).
    pub score: f64,
}

/// Fetched markdown for one topic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FetchResult {
    pub markdown: String,
    /// Human-facing docs URL to cite.
    pub source_url: String,
    /// Repo path.
    pub path: String,
    /// Branch it was read from.
    pub release: String,
}

/// A documentation version (release branch).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VersionInfo {
    /// Branch name.
    pub release: String,
    pub is_latest: bool,
}

/// Manifest stored alongside the index db.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IndexManifest {
    pub branch: String,
    pub commit: String,
    pub chunk_count: usize,
    pub file_count: usize,
    pub embed_model: String,
    pub embed_dim: usize,
    /// ISO timestamp (provided by the caller).
    pub built_at: String,
}
