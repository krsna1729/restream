//! File-ingest HTTP handlers translate dashboard requests into ingest-service
//! operations. Validation and error mapping stay in this module so the service
//! layer can focus on ingest lifecycle and runtime coordination.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::path::Path as FsPath;
use std::sync::Arc;

use crate::application::services::{ApiError, file_ingest_service::FileIngestStartError};

use super::file_ingest::validate_file_ingest_filename;
use super::state::{
    AppState, MAX_NAME_LEN, MAX_STREAM_KEY_LEN, check_field_len, require_authenticated, to_hex,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct IngestPayload {
    pub filename: String,
    pub stream_key: String,
    #[serde(alias = "loop")]
    pub loop_flag: Option<bool>,
    pub start_time: Option<String>,
    pub live_optimized: Option<bool>,
    pub target_gop_seconds: Option<u32>,
}

#[derive(Debug, Clone)]
struct NormalizedIngestPayload {
    filename: String,
    stream_key: String,
    loop_flag: bool,
    start_time: String,
    live_optimized: bool,
    target_gop_seconds: u32,
}

pub fn sanitize_target_gop_seconds(value: Option<u32>) -> u32 {
    value
        .unwrap_or(crate::application::models::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS)
        .max(1)
}

fn ingest_response(
    ingest: &crate::application::models::Ingest,
    running: bool,
) -> serde_json::Value {
    serde_json::json!({
        "id": ingest.id,
        "filename": ingest.filename,
        "streamKey": ingest.stream_key,
        "loop": ingest.loop_flag,
        "startTime": ingest.start_time,
        "liveOptimized": ingest.live_optimized,
        "targetGopSeconds": ingest.target_gop_seconds,
        "running": running
    })
}

fn ingest_json_response(ingest: &crate::application::models::Ingest, running: bool) -> Response {
    Json(ingest_response(ingest, running)).into_response()
}

// Normalize optional API defaults once so create/update handlers can pass a
// single service-facing request shape instead of mixing raw and validated data.
fn normalize_ingest_payload(payload: IngestPayload) -> Result<NormalizedIngestPayload, Response> {
    if let Some(response) = check_field_len("filename", &payload.filename, MAX_NAME_LEN) {
        return Err(response);
    }
    if let Some(response) = validate_file_ingest_filename(&payload.filename) {
        return Err(response);
    }
    if let Some(response) = check_field_len("stream_key", &payload.stream_key, MAX_STREAM_KEY_LEN) {
        return Err(response);
    }
    if let Some(start_time) = payload.start_time.as_deref()
        && let Some(response) = check_field_len("start_time", start_time, 64)
    {
        return Err(response);
    }

    Ok(NormalizedIngestPayload {
        filename: payload.filename,
        stream_key: payload.stream_key,
        loop_flag: payload.loop_flag.unwrap_or(false),
        start_time: payload.start_time.unwrap_or_default(),
        live_optimized: payload.live_optimized.unwrap_or(false),
        target_gop_seconds: sanitize_target_gop_seconds(payload.target_gop_seconds),
    })
}

fn map_start_ingest_error(error: FileIngestStartError) -> Response {
    match error {
        FileIngestStartError::NotFound => {
            (StatusCode::NOT_FOUND, "Ingest not found").into_response()
        }
        FileIngestStartError::MissingPipelineForStreamKey => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "No pipeline found for stream key"})),
        )
            .into_response(),
        FileIngestStartError::IngestLookup => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        FileIngestStartError::PipelineStore(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": format!("Failed to resolve pipeline: {err}")})),
        )
            .into_response(),
        FileIngestStartError::AlreadyRunning => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Ingest already running"})),
        )
            .into_response(),
        FileIngestStartError::InvalidMediaPath => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({
                "error": "Filename must be a relative path under the media directory"
            })),
        )
            .into_response(),
        FileIngestStartError::MediaFileNotFound => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Media file not found"})),
        )
            .into_response(),
        FileIngestStartError::PipelineAlreadyActive => (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "Pipeline already has an active ingest"})),
        )
            .into_response(),
        FileIngestStartError::Spawn(err) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({"error": err})),
        )
            .into_response(),
    }
}

fn map_stop_ingest_error(error: ApiError) -> Response {
    match error {
        ApiError::NotFound(_) => (StatusCode::NOT_FOUND, "Ingest not found").into_response(),
        ApiError::Internal(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        ApiError::Conflict(_) => StatusCode::CONFLICT.into_response(),
    }
}

pub async fn ingests_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let ingests = state.ingest_service.list_ingests().await?;
    let mut res = Vec::new();
    for i in ingests {
        let running = state.engine.is_file_ingest_running(&i.id).await;
        res.push(ingest_response(&i, running));
    }
    Ok(Json(res).into_response())
}

pub async fn ingests_post_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<IngestPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let normalized = match normalize_ingest_payload(payload) {
        Ok(normalized) => normalized,
        Err(response) => return Ok(response),
    };
    let id = format!("ingest_{}", to_hex(&rand::random::<[u8; 8]>()));

    let ingest = state
        .ingest_service
        .create_ingest(
            &id,
            &normalized.filename,
            &normalized.stream_key,
            normalized.loop_flag,
            &normalized.start_time,
            normalized.live_optimized,
            normalized.target_gop_seconds,
        )
        .await?;

    Ok(ingest_json_response(&ingest, false))
}

pub async fn ingests_update_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<IngestPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let normalized = match normalize_ingest_payload(payload) {
        Ok(normalized) => normalized,
        Err(response) => return Ok(response),
    };

    let ingest = state
        .ingest_service
        .update_ingest(
            &id,
            &normalized.filename,
            &normalized.stream_key,
            normalized.loop_flag,
            &normalized.start_time,
            normalized.live_optimized,
            normalized.target_gop_seconds,
        )
        .await?;

    let running = state.engine.is_file_ingest_running(&ingest.id).await;
    Ok(ingest_json_response(&ingest, running))
}

pub async fn ingests_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    state
        .file_ingest_service
        .delete_ingest_with_runtime_cleanup(&state.engine, &id)
        .await?;
    Ok(Json(serde_json::json!({"deleted": true})).into_response())
}

pub async fn ingests_start_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    // Starting a file ingest touches both persisted ingest metadata and the
    // engine's active runtime state, so transport-level errors are mapped here.
    let ingest = match state
        .file_ingest_service
        .start_ingest(state.engine.clone(), FsPath::new(&state.media_dir), &id)
        .await
    {
        Ok(ingest) => ingest,
        Err(error) => return map_start_ingest_error(error),
    };

    ingest_json_response(&ingest, true)
}

pub async fn ingests_stop_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let ingest = match state
        .file_ingest_service
        .stop_ingest_with_runtime_cleanup(&state.engine, &id)
        .await
    {
        Ok(ingest) => ingest,
        Err(error) => return map_stop_ingest_error(error),
    };

    ingest_json_response(&ingest, false)
}

#[cfg(test)]
mod tests {
    use super::{
        IngestPayload, map_start_ingest_error, normalize_ingest_payload,
        sanitize_target_gop_seconds,
    };
    use crate::application::services::file_ingest_service::FileIngestStartError;
    use axum::http::StatusCode;

    fn test_ingest_payload() -> IngestPayload {
        IngestPayload {
            filename: "clips/example.ts".to_string(),
            stream_key: "stream-key".to_string(),
            loop_flag: None,
            start_time: None,
            live_optimized: None,
            target_gop_seconds: None,
        }
    }

    #[test]
    fn sanitize_target_gop_seconds_clamps_to_positive_values() {
        assert_eq!(sanitize_target_gop_seconds(Some(0)), 1);
    }

    #[test]
    fn normalize_ingest_payload_applies_api_defaults() {
        let normalized =
            normalize_ingest_payload(test_ingest_payload()).expect("payload should validate");

        assert_eq!(normalized.filename, "clips/example.ts");
        assert_eq!(normalized.stream_key, "stream-key");
        assert!(!normalized.loop_flag);
        assert!(normalized.start_time.is_empty());
        assert!(!normalized.live_optimized);
        assert!(normalized.target_gop_seconds >= 1);
    }

    #[test]
    fn map_start_ingest_error_preserves_conflict_status() {
        let response = map_start_ingest_error(FileIngestStartError::AlreadyRunning);

        assert_eq!(response.status(), StatusCode::CONFLICT);
    }
}
