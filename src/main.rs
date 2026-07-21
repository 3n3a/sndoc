//! sndoc — local-first CLI for ServiceNow product documentation.
//!
//! Hybrid search + fetch as Markdown over the official ServiceNow docs mirror,
//! usable by humans and AI agents. On first run it clones the docs repo; it
//! refreshes daily and reindexes when the docs change. The same capabilities
//! are available over MCP via `sndoc serve`.

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use serde::Serialize;

use sndoc::core::{constants, fetch as docs, format, repo, search};
use sndoc::{index as indexer, state};

#[derive(Parser)]
#[command(
    name = "sndoc",
    about = "ServiceNow documentation search & fetch (CLI + MCP).",
    disable_version_flag = false,
    arg_required_else_help = true
)]
struct Cli {
    /// Override the data directory (clone, index, state).
    #[arg(long = "data-dir", env = "SNDOC_DATA_DIR", global = false)]
    data_dir: Option<String>,

    /// Skip building/rebuilding the index on the auto-update path.
    #[arg(long = "no-index", global = false)]
    no_index: bool,

    /// Show the installed sndoc version and exit.
    #[arg(short = 'V', long = "version", global = false)]
    version: bool,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Hybrid search over the latest ServiceNow release.
    Search {
        /// Natural-language search query.
        query: String,
        /// Max results.
        #[arg(short = 'n', long, default_value_t = 8)]
        limit: i64,
        /// Emit JSON for agents.
        #[arg(long)]
        json: bool,
    },
    /// Fetch a documentation topic as clean Markdown by its repo path.
    Fetch {
        /// Repo path from a search result.
        path: String,
        /// Release name (default: latest).
        #[arg(short = 'v', long)]
        version: Option<String>,
        /// Fetch live from GitHub instead of the local clone.
        #[arg(long)]
        live: bool,
        /// Emit JSON for agents.
        #[arg(long)]
        json: bool,
    },
    /// Fetch a topic as clean Markdown given a docs URL or reader path.
    #[command(name = "fetch-url")]
    FetchUrl {
        /// docs.servicenow.com URL or 'r/...' path.
        url: String,
        /// Fetch live from GitHub instead of the local clone.
        #[arg(long)]
        live: bool,
        /// Emit JSON for agents.
        #[arg(long)]
        json: bool,
    },
    /// List available ServiceNow release versions (newest first).
    #[command(name = "list-versions")]
    ListVersions {
        /// Emit JSON for agents.
        #[arg(long)]
        json: bool,
    },
    /// Build or rebuild the search index from the local clone.
    Index {
        /// Release branch to index (default: latest).
        #[arg(short = 'b', long)]
        branch: Option<String>,
        /// Rebuild even if already up to date.
        #[arg(short = 'f', long)]
        force: bool,
    },
    /// Refresh the docs clone and reindex on change (cron/daemon entry point).
    Update {
        /// Refresh the clone but skip reindexing.
        #[arg(long = "no-index")]
        no_index: bool,
    },
    /// Run the MCP server: stdio by default (for Claude Code / Desktop /
    /// inspector), or Streamable HTTP when `--http` is given (for running
    /// sndoc on a server and reaching it remotely). HTTP requires `--token`
    /// and is bearer-token gated; put a reverse proxy in front for TLS.
    Serve {
        /// Bind address for the Streamable HTTP transport, e.g.
        /// 127.0.0.1:8080. Omit to serve over stdio instead.
        #[arg(long, env = "SNDOC_HTTP_ADDR")]
        http: Option<String>,
        /// Bearer token required on every HTTP request. Required with --http.
        #[arg(long, env = "SNDOC_HTTP_TOKEN")]
        token: Option<String>,
    },
    /// Check the environment: sqlite-vec + FTS5, index, and clone status.
    Doctor,
}

fn dump<T: Serialize>(v: &T) -> String {
    serde_json::to_string_pretty(v).unwrap_or_default()
}

fn short8(commit: &str) -> &str {
    &commit[..commit.len().min(8)]
}

/// Print a filesystem path with an existence marker, for `doctor`'s paths
/// section.
fn path_line(label: &str, p: &std::path::Path) {
    let mark = if p.exists() { "[ok]" } else { "[..]" };
    println!("{mark} {label}: {}", p.display());
}

fn main() {
    let cli = Cli::parse();

    if let Some(dir) = &cli.data_dir {
        std::env::set_var("SNDOC_DATA_DIR", dir);
    }
    if cli.version {
        println!("sndoc {}", env!("CARGO_PKG_VERSION"));
        return;
    }

    if let Err(err) = run(&cli) {
        eprintln!("Error: {err:#}");
        std::process::exit(1);
    }
}

fn run(cli: &Cli) -> Result<()> {
    let no_index = cli.no_index;
    let command = match &cli.command {
        Some(c) => c,
        // arg_required_else_help handles the no-subcommand case; nothing to do.
        None => return Ok(()),
    };

    match command {
        Command::Search { query, limit, json } => {
            state::ensure_ready(no_index, false, true, true)?;
            let hits = search::search(query, (*limit).max(0) as usize)?;
            if *json {
                println!("{}", dump(&hits));
            } else {
                println!("{}", format::format_search(&hits));
            }
        }
        Command::Fetch {
            path,
            version,
            live,
            json,
        } => {
            state::ensure_ready(no_index, false, false, false)?;
            let res = docs::fetch(path, version.as_deref(), *live)?;
            if *json {
                println!("{}", dump(&res));
            } else {
                println!("{}", format::format_fetch(&res));
            }
        }
        Command::FetchUrl { url, live, json } => {
            state::ensure_ready(no_index, false, false, false)?;
            let res = docs::fetch(url, None, *live)?;
            if *json {
                println!("{}", dump(&res));
            } else {
                println!("{}", format::format_fetch(&res));
            }
        }
        Command::ListVersions { json } => {
            state::ensure_ready(no_index, false, false, false)?;
            let versions = docs::list_versions();
            if *json {
                println!("{}", dump(&versions));
            } else {
                println!("{}", format::format_versions(&versions));
            }
        }
        Command::Index { branch, force } => {
            state::ensure_ready(true, false, false, false)?;
            let target = branch
                .clone()
                .unwrap_or_else(repo::resolve_latest_branch)
                .to_lowercase();
            if !force && indexer::index_exists() {
                if let Some(manifest) = indexer::read_manifest() {
                    if manifest.commit == repo::branch_tip_commit(&target)? {
                        println!(
                            "Index already up to date for '{target}'. Use --force to rebuild."
                        );
                        return Ok(());
                    }
                }
            }
            let manifest = indexer::build_index(Some(&target))?;
            println!(
                "Indexed {} chunks from {} files ({} @ {}).",
                manifest.chunk_count,
                manifest.file_count,
                manifest.branch,
                short8(&manifest.commit)
            );
        }
        Command::Update { no_index: cmd_no_index } => {
            state::ensure_ready(*cmd_no_index || no_index, true, true, false)?;
            println!("Update complete.");
        }
        Command::Serve { http, token } => match http {
            Some(addr) => {
                let token = token.clone().context(
                    "--http requires --token (or SNDOC_HTTP_TOKEN) to authenticate requests",
                )?;
                sndoc::mcp::serve_http(addr, token)?;
            }
            None => {
                sndoc::mcp::serve()?;
            }
        },
        Command::Doctor => {
            return doctor();
        }
    }
    Ok(())
}

fn doctor() -> Result<()> {
    println!("[ok] sndoc version: {}", env!("CARGO_PKG_VERSION"));

    let mut ok = true;
    match sndoc::core::index_store::probe() {
        Ok(vec_version) => {
            println!("[ok] sqlite (rusqlite) + sqlite-vec {vec_version} + fts5");
        }
        Err(err) => {
            ok = false;
            println!("[FAIL] sqlite-vec/fts5: {err}");
        }
    }

    println!(
        "[..] clone: {}",
        if repo::is_cloned() {
            "present"
        } else {
            "absent (run any command to clone)"
        }
    );

    match indexer::read_manifest() {
        Some(m) => println!(
            "[ok] index: {} @ {} ({} chunks, built {})",
            m.branch,
            short8(&m.commit),
            m.chunk_count,
            m.built_at
        ),
        None => println!("[..] index: not built yet (run `sndoc index`)"),
    }

    println!();
    println!("paths:");
    path_line("data dir", &constants::data_dir());
    path_line("clone", &constants::repo_dir());
    path_line("index dir", &constants::index_dir());
    path_line("index db", &constants::index_db_path());
    path_line("manifest", &constants::manifest_path());
    path_line("state", &constants::state_path());

    println!();
    println!("config (env overrides):");
    println!(
        "[..] SNDOC_DATA_DIR: {}",
        std::env::var("SNDOC_DATA_DIR").unwrap_or_else(|_| "(default)".into())
    );
    println!("[..] git url: {}", constants::git_url());
    println!("[..] embed model: {}", constants::embed_model());
    println!(
        "[..] fetch source: {}",
        if constants::fetch_live_default() {
            "live"
        } else {
            "local"
        }
    );

    if !ok {
        std::process::exit(1);
    }
    Ok(())
}
