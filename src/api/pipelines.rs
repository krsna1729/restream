//! Pipeline HTTP handlers sit at the boundary between the dashboard API and the
//! application services. This module keeps request validation and response
//! shaping close to the transport layer so the underlying services can stay
//! focused on pipeline state transitions.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;
use std::sync::Arc;

use crate::api_view_models;
use crate::application::services::ServiceError;
use crate::application::srt_ingest::serialize_persisted_srt_ingest_policy;
use crate::domain::srt_ingest::SrtPipelineIngestConfig;

use super::error::ApiError;
use super::file_ingest::{
    PipelineFileIngestPayload, apply_pipeline_file_ingest_payload,
    validate_pipeline_file_ingest_payload,
};
use super::state::{
    AppState, MAX_NAME_LEN, MAX_STREAM_KEY_LEN, MAX_URL_LEN, check_field_len,
    refresh_srt_ingest_policy_store, require_authenticated, to_hex,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
/// Transport payload for pipeline create and update requests.
///
/// Optional nested `Option` fields preserve the difference between leaving a
/// field unchanged, clearing it, and replacing it with a new value.
pub struct PipelinePayload {
    pub name: String,
    pub stream_key: Option<String>,
    pub input_source: Option<Option<String>>,
    pub srt_ingest_policy: Option<SrtPipelineIngestConfig>,
    pub file_ingest: Option<Option<PipelineFileIngestPayload>>,
}

/// Generates a dashboard-managed stream key when callers do not provide one.
fn generate_stream_key() -> String {
    crate::application::pipeline_inputs::generated_stream_key()
}

/// Normalizes storage-layer duplicate-key variants into one conflict branch at
/// the HTTP boundary.
fn is_duplicate_stream_key_error(err: &ServiceError) -> bool {
    let message = err.to_string();
    message.contains("duplicate stream key")
        || message.contains("idx_pipeline_inputs_stream_key_unique")
        || message.contains("UNIQUE constraint failed: pipeline_inputs.stream_key")
}

/// Shared conflict response for user-visible stream-key collisions.
fn duplicate_stream_key_response() -> Response {
    (
        StatusCode::CONFLICT,
        Json(serde_json::json!({
            "error": "A pipeline input with this stream key already exists"
        })),
    )
        .into_response()
}

/// Trims caller-supplied stream keys and treats empty values as "generate one
/// for me" so create handlers can fall back to random keys.
fn requested_stream_key(stream_key: Option<&str>) -> Option<String> {
    stream_key
        .map(str::trim)
        .filter(|key| !key.is_empty())
        .map(str::to_string)
}

/// Rejects transport-level payload issues before any pipeline mutation starts.
fn validate_pipeline_payload(payload: &PipelinePayload) -> Option<Response> {
    if let Some(response) = check_field_len("name", &payload.name, MAX_NAME_LEN) {
        return Some(response);
    }
    if let Some(stream_key) = payload.stream_key.as_deref()
        && let Some(response) = check_field_len("stream_key", stream_key, MAX_STREAM_KEY_LEN)
    {
        return Some(response);
    }
    if let Some(Some(source)) = payload.input_source.as_ref()
        && let Some(response) = check_field_len("input_source", source, MAX_URL_LEN)
    {
        return Some(response);
    }
    if let Some(Some(file_ingest)) = payload.file_ingest.as_ref()
        && let Some(response) = validate_pipeline_file_ingest_payload(file_ingest)
    {
        return Some(response);
    }
    if let Some(mut policy) = payload.srt_ingest_policy.clone()
        && let Err(error) = policy.validate()
    {
        return Some((StatusCode::BAD_REQUEST, error).into_response());
    }
    if payload.name.trim().is_empty() {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Pipeline name cannot be empty"})),
            )
                .into_response(),
        );
    }

    None
}

/// Serializes the optional ingest policy into the persisted wire format while
/// containing serialization failures at the transport boundary.
fn serialize_srt_ingest_policy(
    policy: Option<&SrtPipelineIngestConfig>,
) -> Result<Option<String>, Box<Response>> {
    match policy {
        Some(policy) => serialize_persisted_srt_ingest_policy(policy)
            .map(Some)
            .map_err(|_| Box::new(StatusCode::INTERNAL_SERVER_ERROR.into_response())),
        None => Ok(None),
    }
}

/// Lists persisted pipelines and reshapes them into the dashboard summary view.
pub async fn pipelines_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    match state.pipeline_service.list_pipelines().await {
        Ok(pipelines) => {
            let ingest_host = state.pipeline_service.get_ingest_host().await;
            let pipelines = pipelines
                .iter()
                .map(|pipeline| {
                    api_view_models::pipeline_response_json(
                        pipeline,
                        &ingest_host,
                        state.ports.rtmp,
                        state.ports.srt,
                    )
                })
                .collect::<Vec<_>>();
            Json(serde_json::json!({ "pipelines": pipelines })).into_response()
        }
        Err(_) => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

/// Returns one pipeline plus its outputs so the detail view can hydrate in one
/// authenticated round-trip.
pub async fn pipeline_detail_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let pipeline = match state.pipeline_service.get_by_id(&id).await {
        Ok(pipeline) => pipeline,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let outputs = match state.output_service.list_for_pipeline(&id).await {
        Ok(outputs) => outputs,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let ingest_host = state.pipeline_service.get_ingest_host().await;

    Json(serde_json::json!({
        "pipeline": api_view_models::pipeline_response_json(
            &pipeline,
            &ingest_host,
            state.ports.rtmp,
            state.ports.srt
        ),
        "outputs": api_view_models::output_response_json_list(&outputs),
    }))
    .into_response()
}

/// Creates a pipeline, retrying rare auto-generated stream-key collisions
/// before surfacing an internal error.
pub async fn pipelines_post_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<PipelinePayload>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    if let Some(response) = validate_pipeline_payload(&payload) {
        return response;
    }

    let requested_stream_key = requested_stream_key(payload.stream_key.as_deref());

    let input_source = payload
        .input_source
        .as_ref()
        .and_then(|source| source.as_deref());
    let srt_ingest_policy = match serialize_srt_ingest_policy(payload.srt_ingest_policy.as_ref()) {
        Ok(policy) => policy,
        Err(response) => return *response,
    };

    // Auto-generated stream keys retry on collisions so callers do not need to
    // handle rare random-key conflicts themselves.
    let max_attempts = if requested_stream_key.is_some() {
        1
    } else {
        16
    };
    for attempt in 0..max_attempts {
        let stream_key = requested_stream_key
            .clone()
            .unwrap_or_else(generate_stream_key);
        let id = format!("pipeline_{}", to_hex(&rand::random::<[u8; 8]>()));
        match state
            .pipeline_service
            .create_pipeline(
                &id,
                &payload.name,
                &stream_key,
                input_source,
                srt_ingest_policy.as_deref(),
            )
            .await
        {
            Ok(pipeline) => {
                refresh_srt_ingest_policy_store(&state).await;
                let file_ingest = match apply_pipeline_file_ingest_payload(
                    &state,
                    &pipeline,
                    None,
                    payload.file_ingest,
                )
                .await
                {
                    Ok(file_ingest) => file_ingest,
                    Err(response) => return response,
                };
                let ingest_host = state.pipeline_service.get_ingest_host().await;
                return (
                    StatusCode::CREATED,
                    Json(serde_json::json!({
                        "message": "Pipeline created",
                        "pipeline": api_view_models::pipeline_response_json_with_file_ingest(
                            &pipeline,
                            &ingest_host,
                            state.ports.rtmp,
                            state.ports.srt,
                            file_ingest.ingest,
                            file_ingest.running,
                        )
                    })),
                )
                    .into_response();
            }
            Err(err) => {
                let duplicate_stream_key = is_duplicate_stream_key_error(&err);
                if duplicate_stream_key && requested_stream_key.is_none() {
                    if attempt + 1 < max_attempts {
                        continue;
                    }
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                if duplicate_stream_key {
                    return duplicate_stream_key_response();
                } else {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }
        }
    }
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
}

/// Updates one pipeline while preserving stored values for patch fields callers
/// omit from the request body.
pub async fn pipelines_update_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<PipelinePayload>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    if let Some(response) = validate_pipeline_payload(&payload) {
        return response;
    }

    let existing = match state.pipeline_service.get_by_id(&id).await {
        Ok(p) => p,
        Err(_) => return (StatusCode::NOT_FOUND, "Pipeline not found").into_response(),
    };

    let existing_stream_key = existing.stream_key.clone();
    let existing_input_source = existing.input_source.clone();
    let existing_srt_ingest_policy = existing.srt_ingest_policy.clone();

    let stream_key = payload
        .stream_key
        .unwrap_or_else(|| existing_stream_key.clone());
    let input_source = payload.input_source.unwrap_or(existing_input_source);
    let srt_ingest_policy = match serialize_srt_ingest_policy(payload.srt_ingest_policy.as_ref()) {
        Ok(Some(value)) => Some(value),
        Ok(None) => existing_srt_ingest_policy,
        Err(response) => return *response,
    };

    if let Ok(active_pipelines) = state.pipeline_service.list_pipelines().await
        && active_pipelines
            .iter()
            .any(|p| p.id != id && p.stream_key == stream_key)
    {
        return duplicate_stream_key_response();
    }

    match state
        .pipeline_service
        .update_pipeline(
            &id,
            &payload.name,
            &stream_key,
            input_source.as_deref(),
            srt_ingest_policy.as_deref(),
        )
        .await
    {
        Ok(updated) => {
            refresh_srt_ingest_policy_store(&state).await;
            let file_ingest = match apply_pipeline_file_ingest_payload(
                &state,
                &updated,
                Some(existing_stream_key.as_str()),
                payload.file_ingest,
            )
            .await
            {
                Ok(file_ingest) => file_ingest,
                Err(response) => return response,
            };
            let ingest_host = state.pipeline_service.get_ingest_host().await;
            Json(serde_json::json!({
                "message": "Pipeline updated",
                "pipeline": api_view_models::pipeline_response_json_with_file_ingest(
                    &updated,
                    &ingest_host,
                    state.ports.rtmp,
                    state.ports.srt,
                    file_ingest.ingest,
                    file_ingest.running,
                )
            }))
            .into_response()
        }
        Err(err) => {
            if is_duplicate_stream_key_error(&err) {
                duplicate_stream_key_response()
            } else {
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

/// Deletes a pipeline only after tearing down runtime state that still points
/// at its outputs, ingest, stages, and HLS helpers.
pub async fn pipelines_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    // The engine owns live runtime state, so we tear that down before removing
    // the persisted pipeline row.
    if let Ok(outputs) = state.output_service.list_outputs().await {
        for output in outputs.iter().filter(|o| o.pipeline_id == id) {
            state.engine.unregister_egress(&output.id).await;
        }
    }

    if let Ok(pipeline) = state.pipeline_service.get_by_id(&id).await
        && let Ok(ingests) = state.ingest_service.list_ingests().await
    {
        for ingest in ingests
            .iter()
            .filter(|i| i.stream_key == pipeline.stream_key)
        {
            let _ = state.engine.stop_file_ingest_child(&ingest.id).await;
        }
    }

    state.engine.remove_pipeline(&id).await;
    state.engine.unregister_ingest(&id).await;
    state.engine.cleanup_pipeline_stages(&id).await;
    state.engine.shutdown_hls_preview_segmenter(&id).await;
    state.engine.shutdown_hls_segmenter(&id).await;

    match state.pipeline_service.delete_pipeline(&id).await {
        Ok(true) => {
            refresh_srt_ingest_policy_store(&state).await;
            Json(serde_json::json!({"message": format!("Pipeline {} deleted", id)})).into_response()
        }
        Ok(false) => (StatusCode::NOT_FOUND, "Pipeline not found").into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

/// Returns the live ingest probe snapshot for one active pipeline input.
pub async fn pipeline_probe_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let ingests = state.engine.ingests.active.read().await;
    match ingests.get(&pipeline_id) {
        Some(ingest) => {
            let probe = crate::api_view_models::probe_snapshot(&pipeline_id, ingest);
            Json(probe).into_response()
        }
        None => (StatusCode::NOT_FOUND, "No active ingest for this pipeline").into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::{PipelinePayload, requested_stream_key, validate_pipeline_payload};
    use axum::http::StatusCode;

    fn payload_with_name(name: &str) -> PipelinePayload {
        PipelinePayload {
            name: name.to_string(),
            stream_key: None,
            input_source: None,
            srt_ingest_policy: None,
            file_ingest: None,
        }
    }

    #[test]
    fn requested_stream_key_trims_and_drops_empty_values() {
        assert_eq!(
            requested_stream_key(Some("  example  ")),
            Some("example".to_string())
        );
        assert_eq!(requested_stream_key(Some("   ")), None);
        assert_eq!(requested_stream_key(None), None);
    }

    #[test]
    fn validate_pipeline_payload_rejects_blank_names() {
        let response = validate_pipeline_payload(&payload_with_name("   "))
            .expect("blank names should be rejected");
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }
}
