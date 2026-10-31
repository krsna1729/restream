//! Errors shared by MCP-facing backends and handlers.

use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentError {
    InvalidRequest(String),
    Transport(String),
    Upstream { status: u16, body: String },
}

impl Display for AgentError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidRequest(message) => write!(f, "{message}"),
            Self::Transport(message) => write!(f, "{message}"),
            Self::Upstream { status, body } => {
                write!(
                    f,
                    "upstream agent route failed with status {status}: {body}"
                )
            }
        }
    }
}

impl std::error::Error for AgentError {}

impl From<serde_json::Error> for AgentError {
    fn from(value: serde_json::Error) -> Self {
        Self::InvalidRequest(value.to_string())
    }
}
