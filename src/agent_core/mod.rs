//! Transport-agnostic agent request types and backend abstractions.
//!
//! Edge transports and the in-process agent plane depend on this module. It
//! deliberately exposes a curated root API so its implementation modules can
//! become a standalone crate without preserving internal module paths.

pub(crate) mod backend;
pub(crate) mod errors;
pub(crate) mod types;

pub use backend::{AgentBackend, AgentFuture, AgentResult};
pub use errors::AgentError;
pub use types::{
    ApprovalRequest, InvestigationRequest, OperationCreateRequest, PlanRequest, ProposedChange,
    VerifyRequest,
};
