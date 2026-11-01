//! MCP sidecar for the restream agent plane: tool catalog, handlers and the
//! stdio/streamable-HTTP transport over the shared `AgentBackend` contract,
//! plus the HTTP backend that calls the product's `/api/v1/agent/*` routes.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

pub(crate) mod approval;
pub mod auth;
pub mod handlers;
#[cfg(feature = "mcp-http-backend")]
pub mod http;
pub(crate) mod inputs;
pub mod tools;
pub mod transport;

pub use auth::McpAuthContext;
pub use handlers::AgentToolHandler;
pub use tools::{McpToolDefinition, available_tool_catalog, tool_catalog};
pub use transport::{CallContext, TransportConfig, TransportMode, run_server};
