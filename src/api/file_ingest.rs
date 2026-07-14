//! Pipeline file-ingest handlers sit at the boundary between pipeline API
//! payloads and the file-ingest service. This module validates filenames and
//! normalizes optional ingest settings before handing runtime work to services.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use std::path::{Component, Path as FsPath};
use std::sync::Arc;

use crate::api_view_models;
use crate::application::services::{ApiError, file_ingest_service::FileIngestConfigInput};

use super::ingests::sanitize_target_gop_seconds;
use super::state::{AppState, MAX_NAME_LEN, check_field_len, require_authenticated};

#[derive(Deserialize, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PipelineFileIngestPayload {
    pub filename: String,
    #[serde(alias = "loop")]
    pub loop_flag: Option<bool>,
    pub start_time: Option<String>,
    pub live_optimized: Option<bool>,
    pub target_gop_seconds: Option<u32>,
}

// The API still exposes the historical custom-encoding endpoint, but the
// transport contract is intentionally frozen at 410 until preset/source-only
// output selection is replaced with a supported custom workflow.
fn custom_encoding_gone_response() -> Response {
    (
        StatusCode::GONE,
        Json(serde_json::json!({
            "error": "Custom encoding is not available yet; choose source or a preset encoding"
        })),
    )
        .into_response()
}

pub fn validate_pipeline_file_ingest_payload(
    payload: &PipelineFileIngestPayload,
) -> Option<Response> {
    if let Some(r) = check_field_len("filename", &payload.filename, MAX_NAME_LEN) {
        return Some(r);
    }
    if let Some(ref start_time) = payload.start_time
        && let Some(r) = check_field_len("start_time", start_time, 64)
    {
        return Some(r);
    }
    validate_file_ingest_filename(&payload.filename)
}

pub fn validate_file_ingest_filename(filename: &str) -> Option<Response> {
    if filename.trim().is_empty() {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Filename cannot be empty"})),
            )
                .into_response(),
        );
    }

    let path = FsPath::new(filename);
    let mut components = path.components().peekable();
    if path.is_absolute()
        || components.peek().is_none()
        || components.any(|component| !matches!(component, Component::Normal(_)))
    {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({
                    "error": "Filename must be a relative path under the media directory"
                })),
            )
                .into_response(),
        );
    }

    None
}

fn file_ingest_state_response(
    state: crate::application::ingest::PipelineFileIngestState,
) -> Response {
    Json(api_view_models::file_ingest_response(
        state.ingest,
        state.running,
    ))
    .into_response()
}

fn deleted_response() -> Response {
    Json(serde_json::json!({"deleted": true})).into_response()
}

fn file_ingest_config_input(payload: PipelineFileIngestPayload) -> FileIngestConfigInput {
    // Normalize optional request fields once so service callers do not each need
    // to re-apply the API defaults.
    FileIngestConfigInput {
        filename: payload.filename,
        loop_flag: payload.loop_flag.unwrap_or(false),
        start_time: payload.start_time.unwrap_or_default(),
        live_optimized: payload.live_optimized.unwrap_or(false),
        target_gop_seconds: sanitize_target_gop_seconds(payload.target_gop_seconds),
    }
}

pub async fn apply_pipeline_file_ingest_payload(
    state: &Arc<AppState>,
    pipeline: &crate::application::models::Pipeline,
    previous_stream_key: Option<&str>,
    payload: Option<Option<PipelineFileIngestPayload>>,
) -> Result<crate::application::ingest::PipelineFileIngestState, Response> {
    let payload = payload.map(|payload| payload.map(file_ingest_config_input));
    state
        .file_ingest_service
        .apply_file_ingest_payload(&state.engine, pipeline, previous_stream_key, payload)
        .await
        .map_err(IntoResponse::into_response)
}

pub async fn pipeline_file_ingest_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let pipeline = state.file_ingest_service.get_pipeline(&pipeline_id).await?;
    let file_ingest_state = state
        .file_ingest_service
        .load_pipeline_file_ingest_state(&state.engine, &pipeline)
        .await?;

    Ok(file_ingest_state_response(file_ingest_state))
}

pub async fn pipeline_file_ingest_put_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
    Json(payload): Json<PipelineFileIngestPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    if let Some(r) = validate_pipeline_file_ingest_payload(&payload) {
        return Ok(r);
    }

    let pipeline = state.file_ingest_service.get_pipeline(&pipeline_id).await?;
    let file_ingest_state = state
        .file_ingest_service
        .apply_file_ingest_payload(
            &state.engine,
            &pipeline,
            None,
            Some(Some(file_ingest_config_input(payload))),
        )
        .await?;

    Ok(file_ingest_state_response(file_ingest_state))
}

pub async fn pipeline_file_ingest_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    let pipeline = state.file_ingest_service.get_pipeline(&pipeline_id).await?;
    state
        .file_ingest_service
        .apply_file_ingest_payload(&state.engine, &pipeline, None, Some(None))
        .await?;

    Ok(deleted_response())
}

pub async fn custom_encoding_get(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    Ok(custom_encoding_gone_response())
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CustomEncodingPayload {
    pub ffmpeg_args: String,
}

pub async fn custom_encoding_put(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<CustomEncodingPayload>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }
    drop(payload.ffmpeg_args);

    Ok(custom_encoding_gone_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_filename(filename: &str) -> bool {
        validate_file_ingest_filename(filename).is_none()
    }

    #[test]
    fn file_ingest_filename_must_stay_relative_under_media_dir() {
        assert!(valid_filename("clip.mp4"));
        assert!(valid_filename("shows/clip.mp4"));

        assert!(!valid_filename(""));
        assert!(!valid_filename("   "));
        assert!(!valid_filename("../clip.mp4"));
        assert!(!valid_filename("shows/../clip.mp4"));
        assert!(!valid_filename("./clip.mp4"));
        assert!(!valid_filename("/tmp/clip.mp4"));
    }

    #[test]
    fn custom_encoding_gone_response_stays_gone() {
        assert_eq!(custom_encoding_gone_response().status(), StatusCode::GONE);
    }

    #[test]
    fn deleted_response_uses_ok_status() {
        assert_eq!(deleted_response().status(), StatusCode::OK);
    }
}
