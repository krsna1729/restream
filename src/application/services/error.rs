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

impl ApiError {
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
