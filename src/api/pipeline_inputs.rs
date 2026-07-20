use std::sync::Arc;

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
};
use serde::Deserialize;

use crate::domain::pipeline_input::PipelineInput;

use super::error::ApiError;
use super::state::{
    AppState, MAX_NAME_LEN, check_field_len, refresh_srt_ingest_policy_store, require_authenticated,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateInputPayload {
    label: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateInputPayload {
    label: Option<String>,
    enabled: Option<bool>,
}

async fn input_json(state: &AppState, input: &PipelineInput, host: &str) -> serde_json::Value {
    let runtime = state
        .engine
        .ingests
        .sessions
        .read()
        .await
        .get(&input.id)
        .cloned();
    let runtime = runtime
        .as_ref()
        .map(|ingest| {
            let metadata = ingest.metadata();
            serde_json::json!({
                "connected": true,
                "forwardingState": ingest.gate.state().as_str(),
                "protocol": ingest.protocol,
                "uptimeSeconds": ingest.start_time.elapsed().as_secs_f64(),
                "bytesReceived": ingest.bytes_received.load(std::sync::atomic::Ordering::Relaxed),
                "remoteAddr": metadata.remote_addr,
                "video": metadata.video,
                "audio": metadata.audio,
                "quality": metadata.quality,
            })
        })
        .unwrap_or_else(|| {
            serde_json::json!({
                "connected": false,
                "forwardingState": null,
                "protocol": null,
                "uptimeSeconds": null,
                "bytesReceived": 0,
                "remoteAddr": null,
                "video": null,
                "audio": null,
                "quality": null,
            })
        });
    serde_json::json!({
        "id": input.id,
        "pipelineId": input.pipeline_id,
        "label": input.label,
        "streamKey": input.stream_key,
        "role": input.role.as_str(),
        "enabled": input.enabled,
        "selected": input.selected,
        "ingestUrls": {
            "rtmp": format!("rtmp://{host}:{}/live/{}", state.ports.rtmp, input.stream_key),
            "srt": format!("srt://{host}:{}?streamid=publish:{}", state.ports.srt, input.stream_key),
        },
        "previewUrl": format!("/hls/inputs/{}/master.m3u8", input.id),
        "runtime": runtime,
    })
}

fn validate_label(label: &str) -> Option<Response> {
    if let Some(response) = check_field_len("label", label, MAX_NAME_LEN) {
        return Some(response);
    }
    if label.trim().is_empty() {
        return Some(
            (
                StatusCode::BAD_REQUEST,
                Json(serde_json::json!({"error": "Input label cannot be empty"})),
            )
                .into_response(),
        );
    }
    None
}

pub async fn pipeline_inputs_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    let inputs = match state.pipeline_input_service.list(&pipeline_id).await {
        Ok(inputs) => inputs,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let host = state.pipeline_service.get_ingest_host().await;
    let selected_input_id = inputs
        .iter()
        .find(|input| input.selected)
        .map(|input| input.id.clone());
    let mut values = Vec::with_capacity(inputs.len());
    for input in &inputs {
        values.push(input_json(&state, input, &host).await);
    }
    Json(serde_json::json!({
        "inputs": values,
        "selectedInputId": selected_input_id,
    }))
    .into_response()
}

pub async fn pipeline_inputs_post_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
    Json(payload): Json<CreateInputPayload>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    if let Some(response) = validate_label(&payload.label) {
        return response;
    }
    let input = match state
        .pipeline_input_service
        .create(&pipeline_id, payload.label.trim())
        .await
    {
        Ok(input) => input,
        Err(error) => return ApiError::from(error).into_response(),
    };
    refresh_srt_ingest_policy_store(&state).await;
    let host = state.pipeline_service.get_ingest_host().await;
    (
        StatusCode::CREATED,
        Json(serde_json::json!({
            "input": input_json(&state, &input, &host).await
        })),
    )
        .into_response()
}

pub async fn pipeline_input_patch_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, input_id)): Path<(String, String)>,
    Json(payload): Json<UpdateInputPayload>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    if let Some(label) = payload.label.as_deref()
        && let Some(response) = validate_label(label)
    {
        return response;
    }
    let current = match state
        .pipeline_input_service
        .get(&pipeline_id, &input_id)
        .await
    {
        Ok(input) => input,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let label = payload
        .label
        .as_deref()
        .map(str::trim)
        .unwrap_or(&current.label);
    let input = match state
        .pipeline_input_service
        .update(
            &pipeline_id,
            &input_id,
            label,
            payload.enabled.unwrap_or(current.enabled),
        )
        .await
    {
        Ok(input) => input,
        Err(error) => return ApiError::from(error).into_response(),
    };
    if !input.enabled {
        state.engine.cancel_pipeline_input(&input.id).await;
    }
    refresh_srt_ingest_policy_store(&state).await;
    let host = state.pipeline_service.get_ingest_host().await;
    Json(serde_json::json!({
        "input": input_json(&state, &input, &host).await
    }))
    .into_response()
}

pub async fn pipeline_input_delete_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, input_id)): Path<(String, String)>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    match state
        .pipeline_input_service
        .delete(&pipeline_id, &input_id)
        .await
    {
        Ok(true) => {
            state.engine.cancel_pipeline_input(&input_id).await;
            refresh_srt_ingest_policy_store(&state).await;
            Json(serde_json::json!({"deleted": true})).into_response()
        }
        Ok(false) => StatusCode::NOT_FOUND.into_response(),
        Err(error) => ApiError::from(error).into_response(),
    }
}

pub async fn pipeline_input_promote_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path((pipeline_id, input_id)): Path<(String, String)>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    let input = match state
        .pipeline_input_service
        .promote(&pipeline_id, &input_id)
        .await
    {
        Ok(input) => input,
        Err(error) => return ApiError::from(error).into_response(),
    };
    let connected = state
        .engine
        .select_pipeline_input(&pipeline_id, &input_id)
        .await;
    let host = state.pipeline_service.get_ingest_host().await;
    Json(serde_json::json!({
        "input": input_json(&state, &input, &host).await,
        "connected": connected,
    }))
    .into_response()
}
