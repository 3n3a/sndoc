//! Shared text formatting for results, used by both the CLI and the MCP stdio
//! server so human-facing output is identical everywhere.

use crate::core::models::{FetchResult, SearchHit, VersionInfo};

pub fn format_search(hits: &[SearchHit]) -> String {
    if hits.is_empty() {
        return "No ServiceNow documentation results found.".to_string();
    }
    let mut lines: Vec<String> = vec!["ServiceNow documentation search results:\n".to_string()];
    for (i, h) in hits.iter().enumerate() {
        let where_ = if h.breadcrumb.is_empty() {
            String::new()
        } else {
            format!(" — {}", h.breadcrumb)
        };
        lines.push(format!("{}. {}{} ({})", i + 1, h.title, where_, h.release));
        if !h.snippet.is_empty() {
            lines.push(format!("   {}", h.snippet));
        }
        lines.push(format!("   url: {}", h.url));
        lines.push(format!("   path: {}", h.path));
        lines.push(String::new());
    }
    lines.push(
        "To read a result, fetch it with `fetch_servicenow_doc` (pass its `path`; \
         add `version` for a specific release)."
            .to_string(),
    );
    lines.join("\n")
}

pub fn format_fetch(res: &FetchResult) -> String {
    format!(
        "> Source: {} (release: {})\n\n{}",
        res.source_url, res.release, res.markdown
    )
}

pub fn format_versions(versions: &[VersionInfo]) -> String {
    if versions.is_empty() {
        return "No ServiceNow documentation versions found.".to_string();
    }
    let mut lines: Vec<String> =
        vec!["ServiceNow documentation versions (newest first):\n".to_string()];
    for v in versions {
        lines.push(format!(
            "- {}{}",
            v.release,
            if v.is_latest { "  (latest)" } else { "" }
        ));
    }
    lines.push(
        "\nSearch covers the latest release. Fetch any version with \
         `fetch_servicenow_doc` and a `version`."
            .to_string(),
    );
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_results_message() {
        assert_eq!(
            format_search(&[]),
            "No ServiceNow documentation results found."
        );
    }

    #[test]
    fn versions_marks_latest() {
        let versions = vec![
            VersionInfo { release: "zurich".into(), is_latest: true },
            VersionInfo { release: "yokohama".into(), is_latest: false },
        ];
        let out = format_versions(&versions);
        assert!(out.contains("- zurich  (latest)"));
        assert!(out.contains("- yokohama\n") || out.ends_with("- yokohama") || out.contains("- yokohama"));
        assert!(out.starts_with("ServiceNow documentation versions (newest first):"));
    }

    #[test]
    fn fetch_has_source_header() {
        let res = FetchResult {
            markdown: "# Body".into(),
            source_url: "https://example.com/r/x".into(),
            path: "x.md".into(),
            release: "zurich".into(),
        };
        assert_eq!(
            format_fetch(&res),
            "> Source: https://example.com/r/x (release: zurich)\n\n# Body"
        );
    }

    #[test]
    fn search_lists_numbered_hits() {
        let hits = vec![SearchHit {
            path: "api/gr.md".into(),
            title: "GlideRecord".into(),
            breadcrumb: "API".into(),
            anchor: "q".into(),
            release: "zurich".into(),
            url: "https://example.com/r/api/gr#q".into(),
            snippet: "snip".into(),
            score: 1.0,
        }];
        let out = format_search(&hits);
        assert!(out.contains("1. GlideRecord — API (zurich)"));
        assert!(out.contains("   url: https://example.com/r/api/gr#q"));
        assert!(out.contains("   path: api/gr.md"));
    }
}
