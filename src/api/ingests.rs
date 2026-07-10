use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use std::path::Path as FsPath;
use std::sync::Arc;

use crate::application::services::{ApiError, file_ingest_service::FileIngestStartError};

use super::file_ingest::validate_file_ingest_filename;
use super::state::{
    AppState, MAX_NAME_LEN, MAX_STREAM_KEY_LEN, check_field_len, get_session_token_from_headers,
    require_authenticated, to_hex,
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

pub fn sanitize_target_gop_seconds(value: Option<u32>) -> u32 {
    value
        .unwrap_or(crate::types::DEFAULT_FILE_INGEST_TARGET_GOP_SECONDS)
        .max(1)
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
        res.push(serde_json::json!({
            "id": i.id,
            "filename": i.filename,
            "streamKey": i.stream_key,
            "loop": i.loop_flag,
            "startTime": i.start_time,
            "liveOptimized": i.live_optimized,
            "targetGopSeconds": i.target_gop_seconds,
            "running": running
        }));
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

    if let Some(r) = check_field_len("filename", &payload.filename, MAX_NAME_LEN) {
        return Ok(r);
    }
    if let Some(r) = validate_file_ingest_filename(&payload.filename) {
        return Ok(r);
    }
    if let Some(r) = check_field_len("stream_key", &payload.stream_key, MAX_STREAM_KEY_LEN) {
        return Ok(r);
    }
    if let Some(ref s) = payload.start_time
        && let Some(r) = check_field_len("start_time", s, 64)
    {
        return Ok(r);
    }
    let id = format!("ingest_{}", to_hex(&rand::random::<[u8; 8]>()));
    let loop_val = payload.loop_flag.unwrap_or(false);
    let start_time = payload.start_time.unwrap_or_default();
    let live_optimized = payload.live_optimized.unwrap_or(false);
    let target_gop_seconds = sanitize_target_gop_seconds(payload.target_gop_seconds);

    let ingest = state
        .ingest_service
        .create_ingest(
            &id,
            &payload.filename,
            &payload.stream_key,
            loop_val,
            &start_time,
            live_optimized,
            target_gop_seconds,
        )
        .await?;

    Ok(Json(serde_json::json!({
        "id": ingest.id,
        "filename": ingest.filename,
        "streamKey": ingest.stream_key,
        "loop": ingest.loop_flag,
        "startTime": ingest.start_time,
        "liveOptimized": ingest.live_optimized,
        "targetGopSeconds": ingest.target_gop_seconds,
        "running": false
    }))
    .into_response())
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

    if let Some(ref s) = payload.start_time
        && let Some(r) = check_field_len("start_time", s, 64)
    {
        return Ok(r);
    }
    if let Some(r) = check_field_len("filename", &payload.filename, MAX_NAME_LEN) {
        return Ok(r);
    }
    if let Some(r) = validate_file_ingest_filename(&payload.filename) {
        return Ok(r);
    }
    if let Some(r) = check_field_len("stream_key", &payload.stream_key, MAX_STREAM_KEY_LEN) {
        return Ok(r);
    }
    let loop_val = payload.loop_flag.unwrap_or(false);
    let start_time = payload.start_time.unwrap_or_default();
    let live_optimized = payload.live_optimized.unwrap_or(false);
    let target_gop_seconds = sanitize_target_gop_seconds(payload.target_gop_seconds);

    let ingest = state
        .ingest_service
        .update_ingest(
            &id,
            &payload.filename,
            &payload.stream_key,
            loop_val,
            &start_time,
            live_optimized,
            target_gop_seconds,
        )
        .await?;

    let running = state.engine.is_file_ingest_running(&ingest.id).await;
    Ok(Json(serde_json::json!({
        "id": ingest.id,
        "filename": ingest.filename,
        "streamKey": ingest.stream_key,
        "loop": ingest.loop_flag,
        "startTime": ingest.start_time,
        "liveOptimized": ingest.live_optimized,
        "targetGopSeconds": ingest.target_gop_seconds,
        "running": running
    }))
    .into_response())
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
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let ingest = match state
        .file_ingest_service
        .start_ingest(state.engine.clone(), FsPath::new(&state.media_dir), &id)
        .await
    {
        Ok(ingest) => ingest,
        Err(FileIngestStartError::NotFound) => {
            return (StatusCode::NOT_FOUND, "Ingest not found").into_response();
        }
        Err(FileIngestStartError::MissingPipelineForStreamKey) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "No pipeline found for stream key"})),
            )
                .into_response();
        }
        Err(FileIngestStartError::IngestLookup) => {
            return StatusCode::INTERNAL_SERVER_ERROR.into_response();
        }
        Err(FileIngestStartError::PipelineStore(err)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": format!("Failed to resolve pipeline: {err}")})),
            )
                .into_response();
        }
        Err(FileIngestStartError::AlreadyRunning) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "Ingest already running"})),
            )
                .into_response();
        }
        Err(FileIngestStartError::InvalidMediaPath) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Filename must be a relative path under the media directory"
                })),
            )
                .into_response();
        }
        Err(FileIngestStartError::MediaFileNotFound) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Media file not found"})),
            )
                .into_response();
        }
        Err(FileIngestStartError::PipelineAlreadyActive) => {
            return (
                StatusCode::CONFLICT,
                Json(serde_json::json!({"error": "Pipeline already has an active ingest"})),
            )
                .into_response();
        }
        Err(FileIngestStartError::Spawn(err)) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(serde_json::json!({"error": err})),
            )
                .into_response();
        }
    };

    Json(serde_json::json!({
        "id": ingest.id,
        "filename": ingest.filename,
        "streamKey": ingest.stream_key,
        "loop": ingest.loop_flag,
        "startTime": ingest.start_time,
        "liveOptimized": ingest.live_optimized,
        "targetGopSeconds": ingest.target_gop_seconds,
        "running": true
    }))
    .into_response()
}

pub async fn ingests_stop_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let ingest = match state
        .file_ingest_service
        .stop_ingest_with_runtime_cleanup(&state.engine, &id)
        .await
    {
        Ok(ingest) => ingest,
        Err(ApiError::NotFound(_)) => {
            return (StatusCode::NOT_FOUND, "Ingest not found").into_response();
        }
        Err(ApiError::Internal(_)) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        Err(ApiError::Conflict(_)) => return StatusCode::CONFLICT.into_response(),
    };

    Json(serde_json::json!({
        "id": ingest.id,
        "filename": ingest.filename,
        "streamKey": ingest.stream_key,
        "loop": ingest.loop_flag,
        "startTime": ingest.start_time,
        "liveOptimized": ingest.live_optimized,
        "targetGopSeconds": ingest.target_gop_seconds,
        "running": false
    }))
    .into_response()
}
