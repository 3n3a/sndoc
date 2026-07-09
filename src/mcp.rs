//! MCP stdio server: exposes the same capabilities as the CLI subcommands over
//! stdio via the official Rust MCP SDK (rmcp), reusing the shared core. For use
//! in Claude Code, Claude Desktop, or the MCP inspector. Run with `sndoc serve`.

use anyhow::Result as AnyResult;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, InitializeResult, ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::core::fetch as docs;
use crate::core::format::{format_fetch, format_search, format_versions};
use crate::core::search::search as search_docs;

#[derive(Debug, Deserialize, JsonSchema)]
struct SearchArgs {
    /// Natural-language search query.
    query: String,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FetchArgs {
    /// Repo path from a search result.
    path: String,
    /// Release name (default: latest).
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    live: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FetchUrlArgs {
    /// docs.servicenow.com URL or 'r/...' reader path.
    url: String,
    #[serde(default)]
    live: bool,
}

#[derive(Clone)]
pub struct Sndoc {
    #[allow(dead_code)]
    tool_router: ToolRouter<Sndoc>,
}

/// Run blocking core work off the async executor (gix/rusqlite/model2vec and the
/// blocking HTTP client all need a non-async thread).
async fn blocking<F>(f: F) -> Result<String, ErrorData>
where
    F: FnOnce() -> AnyResult<String> + Send + 'static,
{
    match tokio::task::spawn_blocking(f).await {
        Ok(Ok(s)) => Ok(s),
        Ok(Err(e)) => Err(ErrorData::internal_error(e.to_string(), None)),
        Err(e) => Err(ErrorData::internal_error(
            format!("task join error: {e}"),
            None,
        )),
    }
}

#[tool_router]
impl Sndoc {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    #[tool(
        name = "search_servicenow_docs",
        description = "Semantic + keyword search over the official ServiceNow product \
                       documentation (the latest release). Returns top matching topics with a \
                       repo `path` to fetch. Use for any question about how a ServiceNow \
                       feature, API, table, or behavior works."
    )]
    async fn search_servicenow_docs(
        &self,
        Parameters(args): Parameters<SearchArgs>,
    ) -> Result<String, ErrorData> {
        blocking(move || Ok(format_search(&search_docs(&args.query, 8)?))).await
    }

    #[tool(
        name = "fetch_servicenow_doc",
        description = "Fetch a ServiceNow documentation topic as clean Markdown by its `path` \
                       (from a search result). Optionally pass `version` (a release name from \
                       list_servicenow_versions) to read a specific release; defaults to latest. \
                       Reads the doc live from GitHub."
    )]
    async fn fetch_servicenow_doc(
        &self,
        Parameters(args): Parameters<FetchArgs>,
    ) -> Result<String, ErrorData> {
        blocking(move || {
            Ok(format_fetch(&docs::fetch(
                &args.path,
                args.version.as_deref(),
                args.live,
            )?))
        })
        .await
    }

    #[tool(
        name = "fetch_servicenow_doc_by_url",
        description = "Fetch a ServiceNow documentation topic as clean Markdown, given a \
                       docs.servicenow.com URL or an 'r/...' reader path. Reads the doc live \
                       from GitHub."
    )]
    async fn fetch_servicenow_doc_by_url(
        &self,
        Parameters(args): Parameters<FetchUrlArgs>,
    ) -> Result<String, ErrorData> {
        blocking(move || Ok(format_fetch(&docs::fetch(&args.url, None, args.live)?))).await
    }

    #[tool(
        name = "list_servicenow_versions",
        description = "List the available ServiceNow documentation versions (release branches), \
                       newest first. Use the names with `fetch_servicenow_doc`'s `version` arg."
    )]
    async fn list_servicenow_versions(&self) -> Result<String, ErrorData> {
        blocking(|| Ok(format_versions(&docs::list_versions()))).await
    }
}

impl Default for Sndoc {
    fn default() -> Self {
        Self::new()
    }
}

#[tool_handler]
impl ServerHandler for Sndoc {
    fn get_info(&self) -> ServerInfo {
        InitializeResult::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("sndoc-mcp", env!("CARGO_PKG_VERSION")))
    }
}

/// Ensure the clone + index are ready, then run the stdio transport.
pub fn serve() -> AnyResult<()> {
    // Claude Desktop (esp. on Windows) can't reliably run a git op per fetch;
    // read doc bodies live over HTTP so fetch never blocks. Overridable by an
    // explicit SNDOC_FETCH_SOURCE=local.
    if std::env::var_os("SNDOC_FETCH_SOURCE").is_none() {
        std::env::set_var("SNDOC_FETCH_SOURCE", "live");
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        tokio::task::spawn_blocking(|| crate::state::ensure_ready(false, false, true, true))
            .await??;
        // stdio transport owns stdout; keep diagnostics on stderr.
        eprintln!(
            "sndoc-mcp {} server ready (stdio).",
            env!("CARGO_PKG_VERSION")
        );
        let service = Sndoc::new().serve(stdio()).await?;
        service.waiting().await?;
        Ok::<(), anyhow::Error>(())
    })
}
