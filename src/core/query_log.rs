//! Opt-in log of searches and fetches, for building retrieval eval sets from
//! real use (see `examples/eval-questions.tsv` and `sndoc query-log`).
//!
//! Enabled with `SNDOC_QUERY_LOG=1` (the Docker image turns it on). Events go
//! to `<data dir>/query-log.db`, a SQLite file separate from the index (which
//! is rebuilt freely). Stored per event: time, source (`cli`, `mcp-stdio`,
//! `mcp-http`), a session id (MCP session header over HTTP, one id per process
//! otherwise), the query or fetched path, and for searches the returned paths.
//! No client addresses or tokens are stored. Queries can contain anything a
//! user typed, so treat the file as sensitive; `SNDOC_QUERY_LOG_RETENTION_DAYS`
//! drops older events.
//!
//! Logging is best-effort: a failure is reported once on stderr and never
//! fails the search or fetch that triggered it.
//!
//! A search followed, in the same session, by a fetch of one of its results is
//! the signal [`labelled_pairs`] turns into `(query, answer page)` candidates.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;

use anyhow::Result;
use once_cell::sync::{Lazy, OnceCell};
use rusqlite::{params, Connection};

use crate::core::constants::data_dir;
use crate::core::models::{FetchResult, SearchHit};

/// Where an event came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    Cli,
    McpStdio,
    McpHttp,
}

impl Source {
    fn as_str(self) -> &'static str {
        match self {
            Source::Cli => "cli",
            Source::McpStdio => "mcp-stdio",
            Source::McpHttp => "mcp-http",
        }
    }
}

/// One logged action.
#[derive(Debug, Clone)]
pub enum Event<'a> {
    /// A search and the repo paths it returned, best first.
    Search {
        query: &'a str,
        results: Vec<String>,
    },
    /// A fetch: the resolved repo path (or the raw input if it did not
    /// resolve) and whether it succeeded.
    Fetch {
        path: &'a str,
        release: Option<&'a str>,
        ok: bool,
    },
}

/// Whether query logging is on (`SNDOC_QUERY_LOG` = 1/true/on/yes).
pub fn enabled() -> bool {
    std::env::var("SNDOC_QUERY_LOG")
        .map(|v| {
            matches!(
                v.trim().to_lowercase().as_str(),
                "1" | "true" | "on" | "yes"
            )
        })
        .unwrap_or(false)
}

pub fn log_path() -> PathBuf {
    data_dir().join("query-log.db")
}

/// Session id for sources without one of their own: one per process (a stdio
/// MCP server serves exactly one client; a CLI run is one command).
pub fn process_session() -> &'static str {
    static ID: Lazy<String> = Lazy::new(|| {
        let t = chrono::Utc::now().format("%Y%m%dT%H%M%S");
        format!("{t}-{}", std::process::id())
    });
    &ID
}

const SCHEMA: &str = "
    CREATE TABLE IF NOT EXISTS events (
        id       INTEGER PRIMARY KEY,
        ts       TEXT NOT NULL,      -- RFC 3339, UTC
        source   TEXT NOT NULL,      -- cli | mcp-stdio | mcp-http
        session  TEXT NOT NULL,
        kind     TEXT NOT NULL,      -- search | fetch
        query    TEXT,               -- search text
        path     TEXT,               -- fetched repo path
        release  TEXT,               -- fetch version, if given
        results  TEXT,               -- search: JSON array of repo paths
        ok       INTEGER NOT NULL DEFAULT 1
    );
    CREATE INDEX IF NOT EXISTS events_session_ts ON events (session, ts);";

fn open(path: &PathBuf) -> Result<Connection> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let db = Connection::open(path)?;
    db.busy_timeout(std::time::Duration::from_secs(5))?;
    db.pragma_update(None, "journal_mode", "WAL")?;
    db.execute_batch(SCHEMA)?;
    if let Some(days) = std::env::var("SNDOC_QUERY_LOG_RETENTION_DAYS")
        .ok()
        .and_then(|d| d.trim().parse::<i64>().ok())
        .filter(|d| *d > 0)
    {
        let cutoff = (chrono::Utc::now() - chrono::Duration::days(days)).to_rfc3339();
        db.execute("DELETE FROM events WHERE ts < ?1", params![cutoff])?;
    }
    Ok(db)
}

static WRITER: OnceCell<Mutex<Connection>> = OnceCell::new();
static WARNED: AtomicBool = AtomicBool::new(false);

/// Record an event if logging is enabled. Never fails the caller.
pub fn record(source: Source, session: &str, event: Event<'_>) {
    if !enabled() {
        return;
    }
    if let Err(e) = try_record(source, session, &event) {
        if !WARNED.swap(true, Ordering::Relaxed) {
            eprintln!("[sndoc] query log disabled for this run: {e:#}");
        }
    }
}

/// Log a search and its hits.
pub fn log_search(source: Source, session: &str, query: &str, hits: &[SearchHit]) {
    record(
        source,
        session,
        Event::Search {
            query,
            results: hits.iter().map(|h| h.path.clone()).collect(),
        },
    );
}

/// Log a fetch: the resolved path on success, the raw input otherwise.
pub fn log_fetch(
    source: Source,
    session: &str,
    input: &str,
    version: Option<&str>,
    result: &Result<FetchResult>,
) {
    let event = match result {
        Ok(r) => Event::Fetch {
            path: &r.path,
            release: Some(&r.release),
            ok: true,
        },
        Err(_) => Event::Fetch {
            path: input,
            release: version,
            ok: false,
        },
    };
    record(source, session, event);
}

fn try_record(source: Source, session: &str, event: &Event<'_>) -> Result<()> {
    let cell = WRITER.get_or_try_init(|| open(&log_path()).map(Mutex::new))?;
    let db = cell
        .lock()
        .map_err(|_| anyhow::anyhow!("query log mutex poisoned"))?;
    let ts = chrono::Utc::now().to_rfc3339();
    match event {
        Event::Search { query, results } => db.execute(
            "INSERT INTO events (ts, source, session, kind, query, results)
             VALUES (?1, ?2, ?3, 'search', ?4, ?5)",
            params![
                ts,
                source.as_str(),
                session,
                query,
                serde_json::to_string(results)?
            ],
        )?,
        Event::Fetch { path, release, ok } => db.execute(
            "INSERT INTO events (ts, source, session, kind, path, release, ok)
             VALUES (?1, ?2, ?3, 'fetch', ?4, ?5, ?6)",
            params![ts, source.as_str(), session, path, release, *ok as i64],
        )?,
    };
    Ok(())
}

/// A `(query, page)` candidate derived from the log.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct LabelledPair {
    pub query: String,
    pub path: String,
    /// 1-based rank of `path` in that search's results.
    pub rank: usize,
    /// How many sessions produced this pair.
    pub sessions: usize,
}

/// Summary counts for `sndoc query-log`.
#[derive(Debug, Clone, serde::Serialize)]
pub struct Summary {
    pub searches: usize,
    pub fetches: usize,
    pub sessions: usize,
    pub distinct_queries: usize,
    /// Searches followed by a fetch of one of their results.
    pub searches_with_followup_fetch: usize,
    pub first_event: Option<String>,
    pub last_event: Option<String>,
}

struct Row {
    ts: chrono::DateTime<chrono::FixedOffset>,
    session: String,
    kind: String,
    query: Option<String>,
    path: Option<String>,
    results: Vec<String>,
    ok: bool,
}

fn read_rows(db: &Connection) -> Result<Vec<Row>> {
    let mut stmt = db.prepare(
        "SELECT ts, session, kind, query, path, results, ok FROM events ORDER BY session, ts, id",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok((
            r.get::<_, String>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
            r.get::<_, Option<String>>(3)?,
            r.get::<_, Option<String>>(4)?,
            r.get::<_, Option<String>>(5)?,
            r.get::<_, i64>(6)?,
        ))
    })?;
    let mut out = Vec::new();
    for row in rows {
        let (ts, session, kind, query, path, results, ok) = row?;
        let Ok(ts) = chrono::DateTime::parse_from_rfc3339(&ts) else {
            continue;
        };
        let results = results
            .and_then(|r| serde_json::from_str::<Vec<String>>(&r).ok())
            .unwrap_or_default();
        out.push(Row {
            ts,
            session,
            kind,
            query,
            path,
            results,
            ok: ok != 0,
        });
    }
    Ok(out)
}

/// Pair each successful fetch with the most recent search in the same session
/// (within `window_mins`) whose results contain the fetched page. Pairs are
/// merged across sessions; sorted by how often they occurred.
fn pairs_from_rows(rows: &[Row], window_mins: i64) -> Vec<LabelledPair> {
    let window = chrono::Duration::minutes(window_mins);
    let mut merged: HashMap<(String, String), (usize, HashSet<String>)> = HashMap::new();
    let mut last_searches: Vec<&Row> = Vec::new();
    let mut session = "";
    for row in rows {
        if row.session != session {
            session = &row.session;
            last_searches.clear();
        }
        match row.kind.as_str() {
            "search" => last_searches.push(row),
            "fetch" if row.ok => {
                let Some(path) = row.path.as_deref() else {
                    continue;
                };
                let hit = last_searches.iter().rev().find_map(|s| {
                    if row.ts - s.ts > window {
                        return None;
                    }
                    let rank = s.results.iter().position(|p| p == path)?;
                    Some((s.query.clone()?, rank + 1))
                });
                if let Some((query, rank)) = hit {
                    let e = merged
                        .entry((query, path.to_string()))
                        .or_insert((rank, HashSet::new()));
                    e.0 = e.0.min(rank);
                    e.1.insert(row.session.clone());
                }
            }
            _ => {}
        }
    }
    let mut out: Vec<LabelledPair> = merged
        .into_iter()
        .map(|((query, path), (rank, sessions))| LabelledPair {
            query,
            path,
            rank,
            sessions: sessions.len(),
        })
        .collect();
    out.sort_by(|a, b| {
        b.sessions
            .cmp(&a.sessions)
            .then(a.query.cmp(&b.query))
            .then(a.path.cmp(&b.path))
    });
    out
}

fn open_existing() -> Result<Connection> {
    let path = log_path();
    if !path.exists() {
        anyhow::bail!(
            "no query log at {} (enable it with SNDOC_QUERY_LOG=1)",
            path.display()
        );
    }
    open(&path)
}

/// Candidate eval pairs from the log; see [`pairs_from_rows`].
pub fn labelled_pairs(window_mins: i64) -> Result<Vec<LabelledPair>> {
    Ok(pairs_from_rows(&read_rows(&open_existing()?)?, window_mins))
}

pub fn summary(window_mins: i64) -> Result<Summary> {
    let db = open_existing()?;
    let rows = read_rows(&db)?;
    let searches: Vec<&Row> = rows.iter().filter(|r| r.kind == "search").collect();
    let pairs = pairs_from_rows(&rows, window_mins);
    let followed: HashSet<&str> = pairs.iter().map(|p| p.query.as_str()).collect();
    let (first, last): (Option<String>, Option<String>) =
        db.query_row("SELECT MIN(ts), MAX(ts) FROM events", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
    Ok(Summary {
        searches: searches.len(),
        fetches: rows.iter().filter(|r| r.kind == "fetch").count(),
        sessions: rows
            .iter()
            .map(|r| r.session.as_str())
            .collect::<HashSet<_>>()
            .len(),
        distinct_queries: searches
            .iter()
            .filter_map(|r| r.query.as_deref())
            .collect::<HashSet<_>>()
            .len(),
        searches_with_followup_fetch: searches
            .iter()
            .filter(|r| r.query.as_deref().is_some_and(|q| followed.contains(q)))
            .count(),
        first_event: first,
        last_event: last,
    })
}

/// Pairs as `examples/eval.rs --queries` TSV (`manual<TAB>query<TAB>path`),
/// consecutive lines per query so the eval treats them as alternatives.
pub fn pairs_to_tsv(pairs: &[LabelledPair]) -> String {
    let mut by_query: Vec<(&str, Vec<&LabelledPair>)> = Vec::new();
    for p in pairs {
        match by_query.iter_mut().find(|(q, _)| *q == p.query) {
            Some((_, v)) => v.push(p),
            None => by_query.push((&p.query, vec![p])),
        }
    }
    let clean = |s: &str| s.replace(['\t', '\n', '\r'], " ");
    let mut out = String::new();
    for (q, ps) in by_query {
        for p in ps {
            out.push_str(&format!("manual\t{}\t{}\n", clean(q), clean(&p.path)));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(
        min: i64,
        session: &str,
        kind: &str,
        query: Option<&str>,
        path: Option<&str>,
        results: &[&str],
    ) -> Row {
        let base = chrono::DateTime::parse_from_rfc3339("2026-11-12T10:00:00+00:00").unwrap();
        Row {
            ts: base + chrono::Duration::minutes(min),
            session: session.into(),
            kind: kind.into(),
            query: query.map(String::from),
            path: path.map(String::from),
            results: results.iter().map(|s| s.to_string()).collect(),
            ok: true,
        }
    }

    #[test]
    fn fetch_of_a_result_pairs_with_the_latest_matching_search() {
        let rows = vec![
            row(
                0,
                "a",
                "search",
                Some("decision table api"),
                None,
                &["x.md", "y.md"],
            ),
            row(1, "a", "search", Some("unrelated"), None, &["z.md"]),
            row(2, "a", "fetch", None, Some("y.md"), &[]),
            // other session: same pair once more -> sessions = 2
            row(
                0,
                "b",
                "search",
                Some("decision table api"),
                None,
                &["y.md"],
            ),
            row(3, "b", "fetch", None, Some("y.md"), &[]),
            // fetch of a page no search returned: no pair
            row(4, "b", "fetch", None, Some("q.md"), &[]),
            // outside the window: no pair
            row(0, "c", "search", Some("late"), None, &["l.md"]),
            row(90, "c", "fetch", None, Some("l.md"), &[]),
        ];
        let pairs = pairs_from_rows(&rows, 30);
        assert_eq!(pairs.len(), 1);
        assert_eq!(pairs[0].query, "decision table api");
        assert_eq!(pairs[0].path, "y.md");
        assert_eq!(pairs[0].rank, 1);
        assert_eq!(pairs[0].sessions, 2);
        assert_eq!(pairs_to_tsv(&pairs), "manual\tdecision table api\ty.md\n");
    }

    #[test]
    fn sessions_do_not_leak_into_each_other() {
        let rows = vec![
            row(0, "a", "search", Some("q"), None, &["p.md"]),
            row(1, "b", "fetch", None, Some("p.md"), &[]),
        ];
        assert!(pairs_from_rows(&rows, 30).is_empty());
    }

    #[test]
    fn round_trip_through_sqlite() {
        let dir = std::env::temp_dir().join(format!("sndoc_qlog_{}", std::process::id()));
        let path = dir.join("q.db");
        let _ = std::fs::remove_dir_all(&dir);
        let db = open(&path).unwrap();
        for (ts, kind, query, p, results) in [
            (
                "2026-11-12T10:00:00+00:00",
                "search",
                Some("acl"),
                None,
                Some(r#"["a.md","b.md"]"#),
            ),
            (
                "2026-11-12T10:01:00+00:00",
                "fetch",
                None,
                Some("b.md"),
                None,
            ),
        ] {
            db.execute(
                "INSERT INTO events (ts, source, session, kind, query, path, results)
                 VALUES (?1, 'mcp-http', 's1', ?2, ?3, ?4, ?5)",
                params![ts, kind, query, p, results],
            )
            .unwrap();
        }
        let pairs = pairs_from_rows(&read_rows(&db).unwrap(), 30);
        assert_eq!(pairs.len(), 1);
        assert_eq!((pairs[0].path.as_str(), pairs[0].rank), ("b.md", 2));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
