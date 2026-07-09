//! sndoc — local-first library for ServiceNow product documentation.
//!
//! Hybrid search + fetch as Markdown over the official ServiceNow docs mirror,
//! usable by humans and AI agents. On first run it clones the docs repo; it
//! refreshes daily and reindexes when the docs change. The same capabilities
//! are available over MCP via `sndoc serve`.

pub mod core;
pub mod index;
pub mod mcp;
pub mod state;
