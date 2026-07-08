use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use std::collections::HashSet;
use std::sync::Arc;

use crate::alerts;
use crate::api_view_models;
use crate::db;
use crate::domain::srt_ingest::SrtPipelineIngestConfig;
use crate::media::srt::serialize_pipeline_srt_ingest_policy;

use super::file_ingest::{
    PipelineFileIngestPayload, apply_pipeline_file_ingest_payload,
    validate_pipeline_file_ingest_payload,
};
use super::state::{
    AppState, DEFAULT_INGEST_HOST, MAX_NAME_LEN, MAX_STREAM_KEY_LEN, MAX_URL_LEN, STREAM_KEYS,
    check_field_len, get_ingest_host, get_session_token_from_headers, recording_enabled_map,
    refresh_srt_ingest_policy_store, require_authenticated, to_hex,
};

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelinePayload {
    pub name: String,
    pub stream_key: Option<String>,
    pub input_source: Option<Option<String>>,
    pub srt_ingest_policy: Option<SrtPipelineIngestConfig>,
    pub file_ingest: Option<Option<PipelineFileIngestPayload>>,
}

pub async fn pipelines_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    match db::list_pipelines(&state.db).await {
        Ok(pipelines) => {
            let ingest_host = get_ingest_host(&state.db)
                .await
                .unwrap_or_else(|_| DEFAULT_INGEST_HOST.to_string());
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

pub async fn pipeline_detail_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let pipeline = match db::get_pipeline(&state.db, &id).await {
        Ok(Some(pipeline)) => pipeline,
        Ok(None) => return (StatusCode::NOT_FOUND, "Pipeline not found").into_response(),
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let outputs = match db::list_outputs_for_pipeline(&state.db, &id).await {
        Ok(outputs) => outputs,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    let ingest_host = get_ingest_host(&state.db)
        .await
        .unwrap_or_else(|_| DEFAULT_INGEST_HOST.to_string());

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

pub async fn pipelines_post_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(payload): Json<PipelinePayload>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    if let Some(r) = check_field_len("name", &payload.name, MAX_NAME_LEN) {
        return r;
    }
    if let Some(ref k) = payload.stream_key
        && let Some(r) = check_field_len("stream_key", k, MAX_STREAM_KEY_LEN)
    {
        return r;
    }
    if let Some(Some(ref source)) = payload.input_source
        && let Some(r) = check_field_len("input_source", source, MAX_URL_LEN)
    {
        return r;
    }
    if let Some(Some(ref file_ingest)) = payload.file_ingest
        && let Some(r) = validate_pipeline_file_ingest_payload(file_ingest)
    {
        return r;
    }
    if let Some(mut policy) = payload.srt_ingest_policy.clone()
        && let Err(error) = policy.validate()
    {
        return (StatusCode::BAD_REQUEST, error).into_response();
    }
    if payload.name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Pipeline name cannot be empty"})),
        )
            .into_response();
    }

    let stream_key = if let Some(ref key) = payload.stream_key {
        key.clone()
    } else {
        let active_pipelines = db::list_pipelines(&state.db).await.unwrap_or_default();
        let used: HashSet<String> = active_pipelines.into_iter().map(|p| p.stream_key).collect();
        let found = STREAM_KEYS.iter().find(|&&(key, _)| !used.contains(key));
        match found {
            Some(&(key, _)) => key.to_string(),
            None => {
                return (
                    StatusCode::BAD_REQUEST,
                    Json(serde_json::json!({"error": "No available stream keys"})),
                )
                    .into_response();
            }
        }
    };

    if let Ok(active_pipelines) = db::list_pipelines(&state.db).await
        && active_pipelines.iter().any(|p| p.stream_key == stream_key)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "A pipeline with this stream key already exists"})),
        )
            .into_response();
    }

    let id = format!("pipeline_{}", to_hex(&rand::random::<[u8; 8]>()));

    let input_source = payload
        .input_source
        .as_ref()
        .and_then(|source| source.as_deref());
    let srt_ingest_policy = match payload.srt_ingest_policy.as_ref() {
        Some(policy) => match serialize_pipeline_srt_ingest_policy(policy) {
            Ok(value) => Some(value),
            Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
        },
        None => None,
    };

    match db::create_pipeline(
        &state.db,
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
            let ingest_host = get_ingest_host(&state.db)
                .await
                .unwrap_or_else(|_| DEFAULT_INGEST_HOST.to_string());
            (
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
                .into_response()
        }
        Err(err) => {
            if err.to_string().contains("duplicate stream key") {
                (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"error": "A pipeline with this stream key already exists"})),
                )
                    .into_response()
            } else {
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
    }
}

pub async fn pipelines_update_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(id): Path<String>,
    Json(payload): Json<PipelinePayload>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    if let Some(r) = check_field_len("name", &payload.name, MAX_NAME_LEN) {
        return r;
    }
    if let Some(ref k) = payload.stream_key
        && let Some(r) = check_field_len("stream_key", k, MAX_STREAM_KEY_LEN)
    {
        return r;
    }
    if let Some(Some(ref source)) = payload.input_source
        && let Some(r) = check_field_len("input_source", source, MAX_URL_LEN)
    {
        return r;
    }
    if let Some(Some(ref file_ingest)) = payload.file_ingest
        && let Some(r) = validate_pipeline_file_ingest_payload(file_ingest)
    {
        return r;
    }
    if let Some(mut policy) = payload.srt_ingest_policy.clone()
        && let Err(error) = policy.validate()
    {
        return (StatusCode::BAD_REQUEST, error).into_response();
    }
    if payload.name.trim().is_empty() {
        return (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({"error": "Pipeline name cannot be empty"})),
        )
            .into_response();
    }

    let existing = match db::get_pipeline(&state.db, &id).await {
        Ok(Some(p)) => p,
        _ => return (StatusCode::NOT_FOUND, "Pipeline not found").into_response(),
    };

    let existing_stream_key = existing.stream_key.clone();
    let existing_input_source = existing.input_source.clone();
    let existing_srt_ingest_policy = existing.srt_ingest_policy.clone();

    let stream_key = payload
        .stream_key
        .unwrap_or_else(|| existing_stream_key.clone());
    let input_source = payload.input_source.unwrap_or(existing_input_source);
    let srt_ingest_policy = match payload
        .srt_ingest_policy
        .as_ref()
        .map(serialize_pipeline_srt_ingest_policy)
        .transpose()
    {
        Ok(Some(value)) => Some(value),
        Ok(None) => existing_srt_ingest_policy,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };

    if let Ok(active_pipelines) = db::list_pipelines(&state.db).await
        && active_pipelines
            .iter()
            .any(|p| p.id != id && p.stream_key == stream_key)
    {
        return (
            StatusCode::CONFLICT,
            Json(serde_json::json!({"error": "A pipeline with this stream key already exists"})),
        )
            .into_response();
    }

    match db::update_pipeline(
        &state.db,
        &id,
        &payload.name,
        &stream_key,
        input_source.as_deref(),
        srt_ingest_policy.as_deref(),
    )
    .await
    {
        Ok(Some(updated)) => {
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
            let ingest_host = get_ingest_host(&state.db)
                .await
                .unwrap_or_else(|_| DEFAULT_INGEST_HOST.to_string());
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
            if err.to_string().contains("duplicate stream key") {
                (
                    StatusCode::CONFLICT,
                    Json(serde_json::json!({"error": "A pipeline with this stream key already exists"})),
                )
                    .into_response()
            } else {
                StatusCode::INTERNAL_SERVER_ERROR.into_response()
            }
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    }
}

pub async fn pipelines_delete_handler(
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

    if let Ok(outputs) = db::list_outputs(&state.db).await {
        for output in outputs.iter().filter(|o| o.pipeline_id == id) {
            state.engine.unregister_egress(&output.id).await;
        }
    }

    if let Ok(Some(pipeline)) = db::get_pipeline(&state.db, &id).await
        && let Ok(ingests) = db::list_ingests(&state.db).await
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

    match db::delete_pipeline(&state.db, &id).await {
        Ok(true) => {
            refresh_srt_ingest_policy_store(&state).await;
            Json(serde_json::json!({"message": format!("Pipeline {} deleted", id)})).into_response()
        }
        _ => (StatusCode::NOT_FOUND, "Pipeline not found").into_response(),
    }
}

pub async fn pipeline_probe_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    match state.engine.probe_snapshot(&pipeline_id).await {
        Some(probe) => Json(probe).into_response(),
        None => (StatusCode::NOT_FOUND, "No active ingest for this pipeline").into_response(),
    }
}

pub async fn pipeline_graph_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    if !db::list_pipelines(&state.db)
        .await
        .unwrap_or_default()
        .iter()
        .any(|pipeline| pipeline.id == pipeline_id)
    {
        return (StatusCode::NOT_FOUND, "Pipeline not found").into_response();
    }

    let outputs = db::list_outputs(&state.db).await.unwrap_or_default();
    let graph =
        crate::api_runtime_views::processing_graph(&state.engine, &pipeline_id, &outputs).await;
    Json(graph).into_response()
}

pub async fn pipeline_alerts_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let recording_enabled = recording_enabled_map(&state, std::slice::from_ref(&pipeline_id)).await;

    let snapshot = crate::api_runtime_views::health_snapshot(
        &state.engine,
        std::slice::from_ref(&pipeline_id),
        &recording_enabled,
        0,
    )
    .await;
    let generated_at = snapshot["generatedAt"].as_str().unwrap_or("").to_string();
    let mut alert_list = alerts::derive_alerts(&snapshot);
    state
        .alert_tracker
        .track_pipeline(&pipeline_id, &mut alert_list);
    Json(serde_json::json!({
        "generatedAt": generated_at,
        "alerts": alert_list,
    }))
    .into_response()
}

pub async fn v1_pipeline_summary_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let recording_enabled = recording_enabled_map(&state, std::slice::from_ref(&pipeline_id)).await;

    let snapshot = crate::api_runtime_views::health_snapshot(
        &state.engine,
        std::slice::from_ref(&pipeline_id),
        &recording_enabled,
        0,
    )
    .await;

    let generated_at = snapshot["generatedAt"].as_str().unwrap_or("").to_string();

    let exists = db::list_pipelines(&state.db)
        .await
        .unwrap_or_default()
        .iter()
        .any(|p| p.id == pipeline_id);
    if !exists {
        return (StatusCode::NOT_FOUND, "Pipeline not found").into_response();
    }

    let pip = &snapshot["pipelines"][&pipeline_id];
    let pipeline_outputs = db::list_outputs(&state.db)
        .await
        .unwrap_or_default()
        .into_iter()
        .filter(|output| output.pipeline_id == pipeline_id)
        .collect::<Vec<_>>();
    let graph =
        crate::api_runtime_views::processing_graph(&state.engine, &pipeline_id, &pipeline_outputs)
            .await;

    let graph_nodes = graph["nodes"]
        .as_array()
        .map(|nodes| nodes.len())
        .unwrap_or(0);
    let graph_edges = graph["edges"]
        .as_array()
        .map(|edges| edges.len())
        .unwrap_or(0);
    let graph_active_nodes = graph["nodes"]
        .as_array()
        .map(|nodes| {
            nodes
                .iter()
                .filter(|node| node["active"].as_bool().unwrap_or(false))
                .count()
        })
        .unwrap_or(0);

    let mut alert_list = alerts::derive_alerts(&snapshot);
    state
        .alert_tracker
        .track_pipeline(&pipeline_id, &mut alert_list);

    let input_status = pip["input"]["status"].as_str().unwrap_or("off");
    let bitrate_kbps = pip["input"]["bitrateKbps"].as_f64();

    let outputs = pip["outputs"].as_object().map(|map| {
        map.iter()
            .map(|(id, v)| {
                serde_json::json!({
                    "id": id,
                    "status": v["status"].as_str().unwrap_or("unknown"),
                    "bitrateKbps": v["bitrateKbps"],
                })
            })
            .collect::<Vec<_>>()
    });

    let total_outputs = outputs.as_ref().map(|o| o.len()).unwrap_or(0);
    let running_outputs = outputs
        .as_ref()
        .map(|o| {
            o.iter()
                .filter(|v| v["status"].as_str() == Some("running"))
                .count()
        })
        .unwrap_or(0);

    Json(serde_json::json!({
        "generatedAt": generated_at,
        "pipelineId": pipeline_id,
        "input": pip["input"],
        "source": {
            "status": input_status,
            "bitrateKbps": bitrate_kbps,
            "protocol": pip["input"]["publisher"]["protocol"],
            "readers": pip["input"]["readers"],
        },
        "outputs": {
            "total": total_outputs,
            "running": running_outputs,
            "list": outputs,
        },
        "recording": pip["recording"],
        "hlsPreview": pip["hlsPreview"],
        "graph": {
            "nodes": graph_nodes,
            "edges": graph_edges,
            "activeNodes": graph_active_nodes,
            "inactiveNodes": graph_nodes.saturating_sub(graph_active_nodes),
            "hasGraph": graph_nodes > 0,
        },
        "alerts": alert_list,
    }))
    .into_response()
}
