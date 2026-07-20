//! Backend implementations for the shared `AgentBackend` trait.

#[cfg(feature = "mcp-http-backend")]
pub mod http;
#[cfg(feature = "mcp-embedded")]
pub mod in_process;
