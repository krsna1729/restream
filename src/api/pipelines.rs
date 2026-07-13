use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use serde::Deserialize;
use std::sync::Arc;

use crate::alerts;
use crate::api_view_models;
use crate::application::services::ApiError;
use crate::domain::srt_ingest::SrtPipelineIngestConfig;
use crate::domain::state::StageBackendKind;
use crate::logging::types::AppLogFilters;
use crate::media::srt::serialize_pipeline_srt_ingest_policy;
use crate::runtime::graph::{GraphRole, StageGraphPlan};

use super::file_ingest::{
    PipelineFileIngestPayload, apply_pipeline_file_ingest_payload,
    validate_pipeline_file_ingest_payload,
};
use super::state::{
    AppState, MAX_NAME_LEN, MAX_STREAM_KEY_LEN, MAX_URL_LEN, check_field_len,
    get_session_token_from_headers, recording_enabled_map, refresh_srt_ingest_policy_store,
    require_authenticated, to_hex,
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

fn generate_stream_key() -> String {
    use rand::RngCore;

    let mut bytes = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut bytes);
    format!("sk_{}", to_hex(&bytes))
}

fn is_duplicate_stream_key_error(err: &ApiError) -> bool {
    let message = err.to_string();
    message.contains("duplicate stream key")
        || message.contains("idx_pipelines_stream_key_unique")
        || message.contains("UNIQUE constraint failed: pipelines.stream_key")
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
        Err(e) => return e.into_response(),
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

    let requested_stream_key = payload
        .stream_key
        .as_ref()
        .map(|key| key.trim())
        .filter(|key| !key.is_empty())
        .map(str::to_string);

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
                if is_duplicate_stream_key_error(&err) && requested_stream_key.is_none() {
                    if attempt + 1 < max_attempts {
                        continue;
                    }
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
                if is_duplicate_stream_key_error(&err) {
                    return (
                        StatusCode::CONFLICT,
                        Json(serde_json::json!({"error": "A pipeline with this stream key already exists"})),
                    )
                    .into_response();
                } else {
                    return StatusCode::INTERNAL_SERVER_ERROR.into_response();
                }
            }
        }
    }
    StatusCode::INTERNAL_SERVER_ERROR.into_response()
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

    if let Ok(active_pipelines) = state.pipeline_service.list_pipelines().await
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
        Err(error) => error.into_response(),
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

    let ingests = state.engine.ingests.active.read().await;
    match ingests.get(&pipeline_id) {
        Some(ingest) => {
            let probe = crate::api_view_models::probe_snapshot(&pipeline_id, ingest);
            Json(probe).into_response()
        }
        None => (StatusCode::NOT_FOUND, "No active ingest for this pipeline").into_response(),
    }
}

pub async fn pipeline_graph_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }
    if state
        .pipeline_service
        .get_by_id(&pipeline_id)
        .await
        .is_err()
    {
        return Ok((StatusCode::NOT_FOUND, "Pipeline not found").into_response());
    }

    let pipeline_outputs = state.output_service.list_for_pipeline(&pipeline_id).await?;
    let mut graph =
        crate::api_runtime_views::processing_graph(&state.engine, &pipeline_id, &pipeline_outputs)
            .await;
    let ingest_codec = state.engine.ingest_video_codec(&pipeline_id).await;
    let backend_policy = state.engine.backend_policy();
    let desired_graphs = crate::application::graph::desired_pipeline_graphs(
        &pipeline_id,
        ingest_codec.as_deref(),
        &pipeline_outputs,
        &backend_policy,
    );
    if let Some(graph_obj) = graph.as_object_mut() {
        graph_obj.insert(
            "desiredGraph".to_string(),
            stage_graph_plan_json(&desired_graphs.aggregate),
        );
        graph_obj.insert(
            "desiredOutputGraphs".to_string(),
            stage_graph_plans_json(&desired_graphs.outputs),
        );
        graph_obj.insert(
            "runtimeGraph".to_string(),
            serde_json::json!({
                "nodes": graph_obj.get("nodes").cloned().unwrap_or_default(),
                "edges": graph_obj.get("edges").cloned().unwrap_or_default(),
            }),
        );
    }
    Ok(Json(graph).into_response())
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

pub async fn pipeline_diagnostics_context_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    if state
        .pipeline_service
        .get_by_id(&pipeline_id)
        .await
        .is_err()
    {
        return Ok((StatusCode::NOT_FOUND, "Pipeline not found").into_response());
    }

    let recording_enabled = recording_enabled_map(&state, std::slice::from_ref(&pipeline_id)).await;
    let health = crate::api_runtime_views::health_snapshot(
        &state.engine,
        std::slice::from_ref(&pipeline_id),
        &recording_enabled,
        0,
    )
    .await;
    let generated_at = health["generatedAt"].as_str().unwrap_or("").to_string();

    let pipeline_outputs = state.output_service.list_for_pipeline(&pipeline_id).await?;
    let ingest_codec = state.engine.ingest_video_codec(&pipeline_id).await;
    let backend_policy = state.engine.backend_policy();
    let desired_graphs = crate::application::graph::desired_pipeline_graphs(
        &pipeline_id,
        ingest_codec.as_deref(),
        &pipeline_outputs,
        &backend_policy,
    );
    let runtime_graph =
        crate::api_runtime_views::processing_graph(&state.engine, &pipeline_id, &pipeline_outputs)
            .await;

    let mut alert_list = alerts::derive_alerts(&health);
    state
        .alert_tracker
        .track_pipeline(&pipeline_id, &mut alert_list);
    let recent_events = state.engine.recent_events(100, Some(&pipeline_id));
    let recent_logs = state
        .log_service
        .list_logs(&AppLogFilters {
            pipeline_id: Some(pipeline_id.clone()),
            limit: Some(100),
            order: Some("desc".to_string()),
            ..empty_log_filters()
        })
        .await?;
    let backend_stderr_tail = state
        .log_service
        .list_logs(&AppLogFilters {
            pipeline_id: Some(pipeline_id.clone()),
            prefix: Some("[ext-transcoder] ffmpeg stderr".to_string()),
            limit: Some(20),
            order: Some("desc".to_string()),
            ..empty_log_filters()
        })
        .await?;

    Ok(Json(serde_json::json!({
        "generatedAt": generated_at,
        "pipelineId": pipeline_id,
        "health": health,
        "graph": {
            "desired": stage_graph_plan_json(&desired_graphs.aggregate),
            "desiredOutputs": stage_graph_plans_json(&desired_graphs.outputs),
            "runtime": runtime_graph,
        },
        "alerts": alert_list,
        "recentEvents": recent_events,
        "recentLogs": recent_logs,
        "backendStderrTail": backend_stderr_tail,
    }))
    .into_response())
}

pub async fn v1_pipeline_summary_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> Result<impl IntoResponse, ApiError> {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return Ok(response);
    }

    if state
        .pipeline_service
        .get_by_id(&pipeline_id)
        .await
        .is_err()
    {
        return Ok((StatusCode::NOT_FOUND, "Pipeline not found").into_response());
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

    let pip = &snapshot["pipelines"][&pipeline_id];
    let pipeline_outputs = state.output_service.list_for_pipeline(&pipeline_id).await?;
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

    Ok(Json(serde_json::json!({
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
    .into_response())
}

fn empty_log_filters() -> AppLogFilters {
    AppLogFilters {
        after_id: None,
        level: Some("debug".to_string()),
        since: None,
        until: None,
        target: None,
        scope: None,
        pipeline_id: None,
        output_id: None,
        event_class: None,
        prefix: None,
        limit: None,
        order: None,
    }
}

fn stage_graph_plan_json(plan: &StageGraphPlan) -> serde_json::Value {
    serde_json::json!({
        "pipelineId": plan.pipeline_id.as_str(),
        "role": graph_role_json(&plan.role),
        "terminalStage": plan.terminal_stage.to_string(),
        "stages": plan
            .stages
            .iter()
            .map(|stage| {
                serde_json::json!({
                    "stage": stage.key.to_string(),
                    "kind": stage.kind.to_string(),
                    "backend": stage_backend_name(stage.backend),
                    "input": stage.input.as_ref().map(|input| input.to_string()),
                })
            })
            .collect::<Vec<_>>(),
        "edges": plan
            .edges
            .iter()
            .map(|edge| {
                serde_json::json!({
                    "from": edge.from.to_string(),
                    "to": edge.to.to_string(),
                })
            })
            .collect::<Vec<_>>(),
    })
}

fn stage_graph_plans_json(plans: &[StageGraphPlan]) -> serde_json::Value {
    serde_json::Value::Array(plans.iter().map(stage_graph_plan_json).collect())
}

fn graph_role_json(role: &GraphRole) -> serde_json::Value {
    match role {
        GraphRole::Output { output_id } => serde_json::json!({
            "kind": "output",
            "outputId": output_id.as_str(),
        }),
        GraphRole::HlsPreview => serde_json::json!({ "kind": "hlsPreview" }),
        GraphRole::HlsOutput { output_id } => serde_json::json!({
            "kind": "hlsOutput",
            "outputId": output_id.as_str(),
        }),
        GraphRole::Recording => serde_json::json!({ "kind": "recording" }),
        GraphRole::Diagnostic => serde_json::json!({ "kind": "diagnostic" }),
    }
}

fn stage_backend_name(backend: StageBackendKind) -> &'static str {
    match backend {
        StageBackendKind::AudioRouter => "audioRouter",
        StageBackendKind::InternalFfmpeg => "internalFfmpeg",
        StageBackendKind::ExternalFfmpeg => "externalFfmpeg",
        StageBackendKind::HlsSegmenter => "hlsSegmenter",
        StageBackendKind::Recording => "recording",
    }
}
