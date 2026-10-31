//! Transport-agnostic agent request types and backend abstractions.
//!
//! The product's agent plane and the MCP sidecar (`restream-mcp`) both depend
//! on this crate; it depends on neither, and on no runtime, SQL or HTTP stack.

#![cfg_attr(not(test), deny(clippy::print_stdout, clippy::print_stderr))]

mod backend;
mod errors;
mod tools;
mod types;

pub use backend::{AgentBackend, AgentFuture, AgentResult};
pub use errors::AgentError;
pub use tools::{AGENT_TOOLS, AgentTool, AgentToolKind, agent_tool_names};
pub use types::{
    ApprovalRequest, InvestigationRequest, OperationCreateRequest, PlanRequest, ProposedChange,
    VerifyRequest,
};
