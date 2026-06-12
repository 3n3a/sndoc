"""Shared dataclasses for the ServiceNow docs core."""

from dataclasses import dataclass


@dataclass(slots=True)
class IndexChunk:
    """One indexed markdown chunk (a heading-delimited slice of one file)."""

    path: str  # repo path under markdown/, e.g. "api-reference/glide-record.md"
    anchor: str  # slugified heading, for deep links ("" for the lead section)
    title: str  # frontmatter title of the file
    breadcrumb: str  # frontmatter breadcrumb, " > "-joined
    heading: str  # this chunk's heading text
    release: str  # branch/release the chunk came from
    content: str  # the chunk body (markdown)
    canonical_url: str  # frontmatter canonical_url, or "" if absent
    last_updated: str  # frontmatter last_updated, or ""


@dataclass(slots=True)
class SearchHit:
    """A search result: one topic (deduped to the best-matching chunk per file)."""

    path: str  # repo path, pass to fetch_servicenow_doc
    title: str
    breadcrumb: str
    anchor: str
    release: str
    url: str  # canonical docs URL to cite
    snippet: str  # short excerpt of the matching chunk
    score: float  # fused RRF score (higher is better)


@dataclass(slots=True)
class FetchResult:
    """Fetched markdown for one topic."""

    markdown: str
    source_url: str  # human-facing docs URL to cite
    path: str  # repo path
    release: str  # branch it was read from


@dataclass(slots=True)
class VersionInfo:
    """A documentation version (release branch)."""

    release: str  # branch name
    is_latest: bool


@dataclass(slots=True)
class IndexManifest:
    """Manifest stored alongside the index blob."""

    branch: str
    commit: str
    chunk_count: int
    file_count: int
    embed_model: str
    embed_dim: int
    built_at: str  # ISO timestamp (provided by the caller)
