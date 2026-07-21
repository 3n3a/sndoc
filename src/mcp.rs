//! MCP server: exposes the same capabilities as the CLI subcommands over
//! either stdio or Streamable HTTP, via the official Rust MCP SDK (rmcp),
//! reusing the shared core. Stdio is for Claude Code, Claude Desktop, or the
//! MCP inspector (`sndoc serve`); HTTP is for running sndoc on a server and
//! reaching it remotely, gated by a bearer token (`sndoc serve --http`).

use std::sync::Arc;

use anyhow::{Context, Result as AnyResult};
use axum::extract::{Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{Implementation, InitializeResult, ServerCapabilities, ServerInfo};
use rmcp::transport::stdio;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use rmcp::{tool, tool_handler, tool_router, ErrorData, ServerHandler, ServiceExt};
use schemars::JsonSchema;
use serde::Deserialize;
use tokio_util::sync::CancellationToken;

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

/// Run blocking core work off the async executor (git2/rusqlite/model2vec and
/// the blocking HTTP client all need a non-async thread).
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

/// Default doc fetch to live-over-HTTP: a git op per fetch can't reliably run
/// under some MCP clients (Claude Desktop on Windows), and under the HTTP
/// transport a shared clone shouldn't be read while a background refresh is
/// fetching into it. Overridable by an explicit `SNDOC_FETCH_SOURCE=local`.
fn default_fetch_source_to_live() {
    if std::env::var_os("SNDOC_FETCH_SOURCE").is_none() {
        std::env::set_var("SNDOC_FETCH_SOURCE", "live");
    }
}

/// Ensure the clone + index are ready, then run the stdio transport.
pub fn serve() -> AnyResult<()> {
    default_fetch_source_to_live();

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

/// Ensure the clone + index are ready, then run the Streamable HTTP transport
/// at `addr` (e.g. `127.0.0.1:8080`), gated by a bearer token on every
/// request. sndoc only speaks plain HTTP — put a reverse proxy in front for
/// TLS when exposing this beyond localhost.
pub fn serve_http(addr: &str, token: String) -> AnyResult<()> {
    default_fetch_source_to_live();

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    rt.block_on(async {
        tokio::task::spawn_blocking(|| crate::state::ensure_ready(false, false, true, true))
            .await??;

        let ct = CancellationToken::new();
        let mcp_service: StreamableHttpService<Sndoc, LocalSessionManager> =
            StreamableHttpService::new(
                || Ok(Sndoc::new()),
                LocalSessionManager::default().into(),
                StreamableHttpServerConfig::default()
                    .with_cancellation_token(ct.child_token())
                    // rmcp's Host allow-list defends a locally-run server
                    // against DNS rebinding; behind a reverse proxy the
                    // inbound Host is the public domain, not this bind
                    // address, so the check would reject legitimate traffic.
                    // The bearer token below is what actually gates access.
                    .disable_allowed_hosts(),
            );

        let app = axum::Router::new()
            .nest_service("/mcp", mcp_service)
            .layer(middleware::from_fn_with_state(
                Arc::new(token),
                require_bearer_token,
            ));

        let listener = tokio::net::TcpListener::bind(addr)
            .await
            .with_context(|| format!("binding {addr}"))?;
        eprintln!(
            "sndoc-mcp {} server ready (http, http://{addr}/mcp).",
            env!("CARGO_PKG_VERSION")
        );
        axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                let _ = tokio::signal::ctrl_c().await;
                ct.cancel();
            })
            .await?;
        Ok::<(), anyhow::Error>(())
    })
}

/// Reject any request without an `Authorization: Bearer <token>` header
/// matching the configured token.
async fn require_bearer_token(
    State(token): State<Arc<String>>,
    headers: HeaderMap,
    request: Request,
    next: Next,
) -> Result<Response, StatusCode> {
    let provided = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "));
    match provided {
        Some(provided) if constant_time_eq(provided, &token) => Ok(next.run(request).await),
        _ => Err(StatusCode::UNAUTHORIZED),
    }
}

/// Byte comparison that doesn't short-circuit on the first mismatching byte
/// (it still leaks the token length via the initial size check, which isn't
/// worth avoiding for a single bearer token).
fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}
