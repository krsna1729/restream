//! Shared application-service error boundary.
//!
//! Services return `ApiError` so handlers can keep transport-layer response
//! shaping in one place while service modules still report typed not-found,
//! conflict, and internal failures consistently.

use std::fmt;

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

/// Typed application error returned by service methods.
///
/// Handlers can convert this into an HTTP response via `IntoResponse`.
#[derive(Debug, Clone)]
pub enum ApiError {
    NotFound(String),
    Conflict(String),
    Internal(String),
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotFound(msg) => write!(f, "not found: {msg}"),
            Self::Conflict(msg) => write!(f, "conflict: {msg}"),
            Self::Internal(msg) => write!(f, "internal: {msg}"),
        }
    }
}

impl ApiError {
    /// Constructor for service-level not-found failures that should surface as
    /// `404 Not Found` at the HTTP boundary.
    pub fn not_found(msg: impl Into<String>) -> Self {
        Self::NotFound(msg.into())
    }

    /// Constructor for service-level conflicts that should surface as
    /// `409 Conflict` at the HTTP boundary.
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }

    /// Constructor for unexpected service failures that should surface as
    /// `500 Internal Server Error` at the HTTP boundary.
    pub fn internal(msg: impl Into<String>) -> Self {
        Self::Internal(msg.into())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self {
            Self::NotFound(msg) => (StatusCode::NOT_FOUND, msg),
            Self::Conflict(msg) => (StatusCode::CONFLICT, msg),
            Self::Internal(msg) => (StatusCode::INTERNAL_SERVER_ERROR, msg),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

impl From<String> for ApiError {
    fn from(msg: String) -> Self {
        Self::Internal(msg)
    }
}

/// Convenience alias for handler return types.
pub type ApiResult<T> = Result<T, ApiError>;
