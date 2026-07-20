//! Shared application-service error boundary.
//!
//! Services report typed not-found, conflict, and internal failures without
//! depending on the transport that presents those failures to callers.

use std::fmt;

/// Typed application error returned by service methods.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServiceError {
    NotFound(String),
    Conflict(String),
    Internal(String),
}

impl fmt::Display for ServiceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(msg) => write!(f, "not found: {msg}"),
            Self::Conflict(msg) => write!(f, "conflict: {msg}"),
            Self::Internal(msg) => write!(f, "internal: {msg}"),
        }
    }
}

impl std::error::Error for ServiceError {}

impl ServiceError {
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

impl From<String> for ServiceError {
    fn from(msg: String) -> Self {
        Self::Internal(msg)
    }
}

pub type ServiceResult<T> = Result<T, ServiceError>;
