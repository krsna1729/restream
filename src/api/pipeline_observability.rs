//! Pipeline-scoped runtime observability HTTP handlers.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use std::sync::Arc;

use crate::alerts;
use crate::domain::state::StageBackendKind;
use crate::logging::types::AppLogFilters;
use crate::runtime::graph::{GraphRole, StageGraphPlan};

use super::error::ApiError;
use super::state::{AppState, recording_enabled_map, require_authenticated};

/// Captures an immediate pipeline-scoped health view for alerts and diagnostics
/// endpoints that should ignore dashboard grace-period smoothing.
async fn pipeline_health_snapshot(state: &AppState, pipeline_id: &str) -> serde_json::Value {
    let pipeline_id = pipeline_id.to_string();
    let pipeline_ids = std::slice::from_ref(&pipeline_id);
    let recording_enabled = recording_enabled_map(state, pipeline_ids).await;

    // Pipeline-scoped transport views intentionally use an immediate snapshot so
    // alerts and diagnostics reflect current runtime state without grace-window delay.
    crate::api_runtime_views::health_snapshot(&state.engine, pipeline_ids, &recording_enabled, 0)
        .await
}

async fn pipeline_health_summary_snapshot(
    state: &AppState,
    pipeline_id: &str,
) -> serde_json::Value {
    let pipeline_id = pipeline_id.to_string();
    let pipeline_ids = std::slice::from_ref(&pipeline_id);
    let recording_enabled = recording_enabled_map(state, pipeline_ids).await;

    crate::api_runtime_views::health_summary_snapshot(
        &state.engine,
        pipeline_ids,
        &recording_enabled,
        0,
    )
    .await
}

/// Pulls the shared generation timestamp from runtime health snapshots so
/// pipeline endpoints report one consistent clock value.
fn snapshot_generated_at(snapshot: &serde_json::Value) -> String {
    snapshot["generatedAt"].as_str().unwrap_or("").to_string()
}

/// Combines the runtime processing graph with the desired graph plan so the UI
/// can compare actual and intended topology in one response.
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

/// Derives the current alert set for one pipeline from the immediate health
/// snapshot and the alert tracker's sticky state.
pub async fn pipeline_alerts_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(pipeline_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let snapshot = pipeline_health_snapshot(&state, &pipeline_id).await;
    let mut alert_list = alerts::derive_alerts(&snapshot);
    state
        .alert_tracker
        .track_pipeline(&pipeline_id, &mut alert_list);
    Json(serde_json::json!({
        "generatedAt": snapshot_generated_at(&snapshot),
        "alerts": alert_list,
    }))
    .into_response()
}

/// Packages the main troubleshooting context for one pipeline, including
/// health, graphs, alerts, recent events, and recent logs.
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

    let health = pipeline_health_snapshot(&state, &pipeline_id).await;
    let generated_at = snapshot_generated_at(&health);

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

/// Builds the compact pipeline summary consumed by cards and lightweight
/// dashboard status surfaces.
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

    let snapshot = pipeline_health_summary_snapshot(&state, &pipeline_id).await;
    let generated_at = snapshot_generated_at(&snapshot);

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

/// Provides the default diagnostics log filter shell before callers add
/// pipeline-specific constraints.
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

/// Serializes one desired stage graph plan into the dashboard JSON shape.
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

/// Serializes the list form used by graph and diagnostics endpoints.
fn stage_graph_plans_json(plans: &[StageGraphPlan]) -> serde_json::Value {
    serde_json::Value::Array(plans.iter().map(stage_graph_plan_json).collect())
}

/// Normalizes graph roles into stable JSON discriminators for the dashboard.
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

/// Maps runtime backend enums to the dashboard's camelCase transport names.
fn stage_backend_name(backend: StageBackendKind) -> &'static str {
    match backend {
        StageBackendKind::AudioRouter => "audioRouter",
        StageBackendKind::InternalFfmpeg => "internalFfmpeg",
        StageBackendKind::ExternalFfmpeg => "externalFfmpeg",
        StageBackendKind::HlsSegmenter => "hlsSegmenter",
        StageBackendKind::Recording => "recording",
    }
}
