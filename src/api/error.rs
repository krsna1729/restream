//! HTTP mapping for application-service failures.

use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

use crate::application::services::ServiceError;

/// API-owned response adapter for transport-independent application failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError(ServiceError);

impl ApiError {
    pub fn internal(message: impl Into<String>) -> Self {
        Self(ServiceError::internal(message))
    }
}

impl From<ServiceError> for ApiError {
    fn from(error: ServiceError) -> Self {
        Self(error)
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let (status, message) = match self.0 {
            ServiceError::NotFound(message) => (StatusCode::NOT_FOUND, message),
            ServiceError::Conflict(message) => (StatusCode::CONFLICT, message),
            ServiceError::Internal(message) => (StatusCode::INTERNAL_SERVER_ERROR, message),
        };
        (status, Json(serde_json::json!({ "error": message }))).into_response()
    }
}

#[cfg(test)]
mod tests {
    use axum::response::IntoResponse;
    use http_body_util::BodyExt;

    use super::*;

    #[tokio::test]
    async fn application_error_variants_preserve_http_status_and_json_body() {
        let contracts = [
            (
                ServiceError::not_found("missing"),
                StatusCode::NOT_FOUND,
                "missing",
            ),
            (ServiceError::conflict("busy"), StatusCode::CONFLICT, "busy"),
            (
                ServiceError::internal("failed"),
                StatusCode::INTERNAL_SERVER_ERROR,
                "failed",
            ),
        ];

        for (error, expected_status, expected_message) in contracts {
            let response = ApiError::from(error).into_response();
            assert_eq!(response.status(), expected_status);

            let body = response
                .into_body()
                .collect()
                .await
                .expect("error response body should be readable")
                .to_bytes();
            let value: serde_json::Value =
                serde_json::from_slice(&body).expect("error response body should be JSON");
            assert_eq!(value, serde_json::json!({ "error": expected_message }));
        }
    }
}
