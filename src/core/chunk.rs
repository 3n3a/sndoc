//! Parse a doc file's frontmatter and split its body into heading-delimited
//! chunks small enough to embed. Each chunk carries the file title + breadcrumb
//! + its heading so the embedded text has enough standalone context to match
//! conceptual queries.

use once_cell::sync::Lazy;
use regex::Regex;

use crate::core::models::IndexChunk;

/// Target chunk size in characters, kept compact so the context prefix
/// (title/breadcrumb/heading) plus body stays within the model's window.
const MAX_CHARS: usize = 1600;
const OVERLAP_CHARS: usize = 160;

static HEADING_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^(#{1,3})\s+(.*)$").unwrap());
static SLUG_STRIP_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"[^\w\s-]").unwrap());
static SLUG_SPACE_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\s+").unwrap());
static PARA_SPLIT_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"\n{2,}").unwrap());
static FM_BOUNDARY_RE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^-{3,}\s*$").unwrap());

#[derive(Debug, Clone, Default)]
pub struct DocMeta {
    pub title: String,
    pub breadcrumb: String,
    pub canonical_url: String,
    pub last_updated: String,
}

#[derive(Debug, Clone)]
pub struct ParsedDoc {
    pub meta: DocMeta,
    pub body: String,
}

// --- char-based helpers (Python str semantics are per-character) ----------

fn clen(s: &str) -> usize {
    s.chars().count()
}

/// `s[start..end]` in character units (`end == None` → to the end).
fn cslice(s: &str, start: usize, end: Option<usize>) -> String {
    match end {
        Some(e) => s.chars().skip(start).take(e.saturating_sub(start)).collect(),
        None => s.chars().skip(start).collect(),
    }
}

// --- frontmatter ----------------------------------------------------------

fn scalar_to_string(v: &serde_yaml::Value) -> String {
    match v {
        serde_yaml::Value::String(s) => s.clone(),
        serde_yaml::Value::Bool(b) => {
            // Match Python str(bool): "True"/"False".
            if *b {
                "True".to_string()
            } else {
                "False".to_string()
            }
        }
        serde_yaml::Value::Number(n) => n.to_string(),
        serde_yaml::Value::Null => String::new(),
        _ => String::new(),
    }
}

fn as_string(v: Option<&serde_yaml::Value>) -> String {
    match v {
        None => String::new(),
        Some(serde_yaml::Value::Sequence(seq)) => seq
            .iter()
            .map(scalar_to_string)
            .collect::<Vec<_>>()
            .join(" > "),
        Some(other) => scalar_to_string(other),
    }
}

/// Split leading YAML frontmatter (`---` fenced) from the body. On any parse
/// issue, returns no metadata and the raw text as body (matches the Python
/// fallback of indexing the raw body with empty meta).
fn split_frontmatter(text: &str) -> (Option<serde_yaml::Value>, String) {
    if !text.starts_with("---") {
        return (None, text.to_string());
    }
    let mut it = FM_BOUNDARY_RE.find_iter(text);
    let first = it.next();
    let second = it.next();
    if let (Some(f), Some(s)) = (first, second) {
        if f.start() == 0 {
            let yaml_str = &text[f.end()..s.start()];
            let body = &text[s.end()..];
            let body = body.strip_prefix('\n').unwrap_or(body);
            if let Ok(v) = serde_yaml::from_str::<serde_yaml::Value>(yaml_str) {
                return (Some(v), body.to_string());
            }
        }
    }
    (None, text.to_string())
}

/// Split frontmatter from body and normalize the fields we index on.
pub fn parse_doc(markdown: &str) -> ParsedDoc {
    let (meta_value, body) = split_frontmatter(markdown);
    let get = |key: &str| -> String { as_string(meta_value.as_ref().and_then(|v| v.get(key))) };
    ParsedDoc {
        meta: DocMeta {
            title: get("title"),
            breadcrumb: get("breadcrumb"),
            canonical_url: get("canonical_url"),
            last_updated: get("last_updated"),
        },
        body,
    }
}

/// Slugify a heading into a GitHub-style anchor.
pub fn slug(heading: &str) -> String {
    let lowered = heading.to_lowercase();
    let stripped = SLUG_STRIP_RE.replace_all(&lowered, "");
    let trimmed = stripped.trim();
    SLUG_SPACE_RE.replace_all(trimmed, "-").into_owned()
}

#[derive(Debug, Clone)]
struct Section {
    heading: String,
    text: String,
}

/// Group body lines into sections delimited by H1-H3 ATX headings.
fn split_sections(body: &str) -> Vec<Section> {
    let mut sections: Vec<Section> = Vec::new();
    let mut heading = String::new();
    let mut buf: Vec<&str> = Vec::new();

    for line in body.split('\n') {
        if let Some(caps) = HEADING_RE.captures(line) {
            // flush
            let text = buf.join("\n").trim().to_string();
            if !text.is_empty() || !heading.is_empty() {
                sections.push(Section {
                    heading: heading.clone(),
                    text,
                });
            }
            buf.clear();
            heading = caps.get(2).map(|m| m.as_str().trim()).unwrap_or("").to_string();
        } else {
            buf.push(line);
        }
    }
    // final flush
    let text = buf.join("\n").trim().to_string();
    if !text.is_empty() || !heading.is_empty() {
        sections.push(Section { heading, text });
    }

    sections
        .into_iter()
        .filter(|s| !s.text.is_empty() || !s.heading.is_empty())
        .collect()
}

/// Break an over-long section body into overlapping windows on paragraph
/// boundaries where possible.
fn window_text(text: &str) -> Vec<String> {
    if clen(text) <= MAX_CHARS {
        return vec![text.to_string()];
    }
    let mut windows: Vec<String> = Vec::new();
    let mut cur = String::new();
    for p in PARA_SPLIT_RE.split(text) {
        if !cur.is_empty() && clen(&cur) + clen(p) + 2 > MAX_CHARS {
            windows.push(cur.clone());
            let start = clen(&cur).saturating_sub(OVERLAP_CHARS);
            cur = cslice(&cur, start, None);
        }
        cur = if cur.is_empty() {
            p.to_string()
        } else {
            format!("{cur}\n\n{p}")
        };
        // A single huge paragraph: hard-split it.
        while clen(&cur) > MAX_CHARS {
            windows.push(cslice(&cur, 0, Some(MAX_CHARS)));
            cur = cslice(&cur, MAX_CHARS - OVERLAP_CHARS, None);
        }
    }
    if !cur.trim().is_empty() {
        windows.push(cur);
    }
    windows
}

/// Produce embeddable chunks for one file. `file_path` and `release` identify it.
pub fn chunk_doc(markdown: &str, file_path: &str, release: &str) -> Vec<IndexChunk> {
    let parsed = parse_doc(markdown);
    let meta = &parsed.meta;
    let mut chunks: Vec<IndexChunk> = Vec::new();
    for section in split_sections(&parsed.body) {
        for window in window_text(&section.text) {
            if window.trim().is_empty() && section.heading.is_empty() {
                continue;
            }
            chunks.push(IndexChunk {
                path: file_path.to_string(),
                anchor: if section.heading.is_empty() {
                    String::new()
                } else {
                    slug(&section.heading)
                },
                title: meta.title.clone(),
                breadcrumb: meta.breadcrumb.clone(),
                heading: section.heading.clone(),
                release: release.to_string(),
                content: window.trim().to_string(),
                canonical_url: meta.canonical_url.clone(),
                last_updated: meta.last_updated.clone(),
            });
        }
    }
    // A file with only frontmatter / no body still gets one title-only chunk so
    // it is searchable by title.
    if chunks.is_empty() && !meta.title.is_empty() {
        chunks.push(IndexChunk {
            path: file_path.to_string(),
            anchor: String::new(),
            title: meta.title.clone(),
            breadcrumb: meta.breadcrumb.clone(),
            heading: meta.title.clone(),
            release: release.to_string(),
            content: meta.breadcrumb.clone(),
            canonical_url: meta.canonical_url.clone(),
            last_updated: meta.last_updated.clone(),
        });
    }
    chunks
}

/// The text actually fed to the embedder for a chunk: context + body.
pub fn chunk_embed_text(c: &IndexChunk) -> String {
    let mut seen: Vec<&str> = Vec::new();
    for v in [c.breadcrumb.as_str(), c.title.as_str(), c.heading.as_str()] {
        if !v.is_empty() && !seen.contains(&v) {
            seen.push(v);
        }
    }
    let ctx = seen.join(" — ");
    if ctx.is_empty() {
        c.content.clone()
    } else {
        format!("{ctx}\n\n{}", c.content)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DOC: &str = "---
title: GlideRecord
breadcrumb:
  - API
  - GlideRecord
canonical_url: https://example.com/r/api/glide-record
---

Intro paragraph about GlideRecord.

# Query Methods

Use addQuery to filter.

## get()

Fetch a single record.
";

    #[test]
    fn chunks_split_by_heading_with_anchors() {
        let chunks = chunk_doc(DOC, "api/glide-record.md", "zurich");
        let headings: Vec<&str> = chunks.iter().map(|c| c.heading.as_str()).collect();
        assert!(headings.contains(&"Query Methods"));
        assert!(headings.contains(&"get()"));
        // Anchor is the slug of the heading.
        let qm = chunks.iter().find(|c| c.heading == "Query Methods").unwrap();
        assert_eq!(qm.anchor, "query-methods");
        // Frontmatter is parsed onto every chunk.
        assert_eq!(qm.title, "GlideRecord");
        assert_eq!(qm.breadcrumb, "API > GlideRecord");
        assert_eq!(qm.release, "zurich");
    }

    #[test]
    fn lead_section_has_empty_anchor() {
        let chunks = chunk_doc(DOC, "p.md", "zurich");
        let lead = chunks.iter().find(|c| c.heading.is_empty());
        assert!(lead.is_some(), "expected a lead (pre-heading) section");
        assert_eq!(lead.unwrap().anchor, "");
    }

    #[test]
    fn slug_matches_github_style() {
        assert_eq!(slug("Query Methods"), "query-methods");
        assert_eq!(slug("get() & set()"), "get-set");
        assert_eq!(slug("  Trim  Me  "), "trim-me");
    }

    #[test]
    fn embed_text_prepends_dedup_context() {
        let c = IndexChunk {
            path: "p.md".into(),
            anchor: "h".into(),
            title: "GlideRecord".into(),
            breadcrumb: "API > GlideRecord".into(),
            heading: "Query Methods".into(),
            release: "zurich".into(),
            content: "body text".into(),
            canonical_url: String::new(),
            last_updated: String::new(),
        };
        let t = chunk_embed_text(&c);
        assert_eq!(
            t,
            "API > GlideRecord — GlideRecord — Query Methods\n\nbody text"
        );
    }

    #[test]
    fn title_only_fallback() {
        let doc = "---\ntitle: Only Title\nbreadcrumb: Docs\n---\n";
        let chunks = chunk_doc(doc, "t.md", "zurich");
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].heading, "Only Title");
        assert_eq!(chunks[0].content, "Docs");
    }

    #[test]
    fn no_frontmatter_indexes_raw_body() {
        let doc = "# Heading\n\nsome content";
        let chunks = chunk_doc(doc, "t.md", "zurich");
        assert!(!chunks.is_empty());
        assert_eq!(chunks[0].title, "");
        assert_eq!(chunks[0].heading, "Heading");
    }
}
