//! Agent HTTP handlers expose the optional planning and execution surfaces used
//! by operator-facing automation features. This module stays at the API
//! boundary: it authenticates requests, selects the feature-gated surface to
//! expose, and shapes responses from the agent planning/execution layers.

use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use std::sync::Arc;

#[cfg(any(not(feature = "agent-plane"), not(feature = "agent-execution")))]
use axum::response::Response;

#[cfg(any(feature = "agent-plane", feature = "agent-execution"))]
use crate::domain::state::DesiredOutputState;

use super::state::{AppState, require_authenticated};

#[cfg(any(not(feature = "agent-plane"), not(feature = "agent-execution")))]
/// Shared 404 payload for agent routes that are compiled out by feature flags.
fn feature_unavailable_response(feature: &'static str) -> Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": format!("{feature} feature is not compiled in"),
            "feature": feature,
            "compiledIn": false
        })),
    )
        .into_response()
}

#[cfg(not(feature = "agent-plane"))]
/// Shortcut response for builds without the agent planning surface.
fn agent_plane_unavailable() -> Response {
    feature_unavailable_response("agent-plane")
}

#[cfg(not(feature = "agent-execution"))]
/// Shortcut response for builds without the agent execution surface.
fn agent_execution_unavailable() -> Response {
    feature_unavailable_response("agent-execution")
}

#[cfg(feature = "agent-plane")]
use super::state::recording_enabled_map;
#[cfg(feature = "agent-execution")]
use super::state::to_hex;
#[cfg(feature = "agent-plane")]
use super::telemetry::{process_resource_snapshot, system_status};
#[cfg(feature = "agent-plane")]
use crate::alerts;
#[cfg(feature = "agent-plane")]
use crate::api_runtime_views::{ResourceMapOptions, ResourceMapView};
#[cfg(feature = "agent-plane")]
use crate::api_view_models;
#[cfg(feature = "agent-plane")]
use crate::application::models::{Ingest, Pipeline};
#[cfg(any(feature = "agent-plane", feature = "agent-execution"))]
use crate::domain::output_spec::OutputUrlScheme;
#[cfg(feature = "agent-plane")]
use crate::events;
#[cfg(feature = "agent-plane")]
use std::path::Path as FsPath;
#[cfg(feature = "agent-plane")]
use sysinfo::{Disks, System};

#[cfg(feature = "agent-plane")]
const AGENT_PROCESSING_GRAPH_OUTPUT_LIMIT: usize = 50;

#[cfg(any(feature = "agent-plane", feature = "agent-execution"))]
/// Builds the immediate health snapshot shared by agent planning and execution
/// flows so prompts and verification reflect current runtime state.
async fn agent_health_snapshot(
    state: &AppState,
    pipeline_ids: &[String],
) -> (std::collections::HashMap<String, bool>, serde_json::Value) {
    let recording_enabled = recording_enabled_map(state, pipeline_ids).await;

    // Agent surfaces prefer an immediate snapshot because investigation and
    // verification prompts should reflect the current runtime state, not a
    // grace-window-smoothed operator view.
    let health = crate::api_runtime_views::health_snapshot(
        &state.engine,
        pipeline_ids,
        &recording_enabled,
        0,
    )
    .await;

    (recording_enabled, health)
}

#[cfg(feature = "agent-plane")]
/// Returns the compiled capability manifest for authenticated agent-plane
/// clients.
pub async fn agent_capabilities_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    Json(crate::agent_plane::capabilities()).into_response()
}

#[cfg(not(feature = "agent-plane"))]
pub async fn agent_capabilities_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_plane_unavailable()
}

#[cfg(feature = "agent-plane")]
/// Returns the full planning context document used by agent-plane clients.
pub async fn agent_context_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let context = build_agent_context(&state).await;
    Json(context).into_response()
}

#[cfg(not(feature = "agent-plane"))]
pub async fn agent_context_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_plane_unavailable()
}

#[cfg(feature = "agent-plane")]
/// Returns an investigation payload for one optional pipeline/output focus by
/// joining health, alerts, graphs, telemetry, and events.
pub async fn agent_investigation_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_plane::InvestigationRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let catalog = state.agent_service.load_pipeline_output_catalog().await;
    let pipelines = catalog.pipelines;
    let outputs = catalog.outputs;
    let pipeline_exists = request
        .pipeline_id
        .as_deref()
        .is_none_or(|pid| pipelines.iter().any(|p| p.id == pid));
    let selected_pipeline = request
        .pipeline_id
        .as_deref()
        .and_then(|pid| pipelines.iter().find(|pipeline| pipeline.id == pid))
        .map(crate::agent_plane::redact_secrets_from_serializable);
    let output_exists = request.output_id.as_deref().is_none_or(|oid| {
        outputs.iter().any(|output| {
            output.id == oid
                && request
                    .pipeline_id
                    .as_deref()
                    .is_none_or(|pid| output.pipeline_id == pid)
        })
    });
    let selected_output = request
        .output_id
        .as_deref()
        .and_then(|oid| {
            outputs.iter().find(|output| {
                output.id == oid
                    && request
                        .pipeline_id
                        .as_deref()
                        .is_none_or(|pid| output.pipeline_id == pid)
            })
        })
        .map(crate::agent_plane::redact_secrets_from_serializable);

    let pipeline_ids: Vec<String> = request
        .pipeline_id
        .clone()
        .map(|pid| vec![pid])
        .unwrap_or_else(|| pipelines.iter().map(|p| p.id.clone()).collect());
    let (_recording_enabled, health) = agent_health_snapshot(&state, &pipeline_ids).await;
    let alerts = alerts::derive_alerts(&health);
    let graph = if let Some(pid) = request.pipeline_id.as_deref()
        && pipeline_exists
        && outputs
            .iter()
            .filter(|output| output.pipeline_id == pid)
            .count()
            <= AGENT_PROCESSING_GRAPH_OUTPUT_LIMIT
    {
        Some(crate::api_runtime_views::processing_graph(&state.engine, pid, &outputs).await)
    } else {
        None
    };
    let telemetry = if let Some(pid) = request.pipeline_id.as_deref() {
        crate::api_runtime_views::pipeline_telemetry(&state.engine, pid).await
    } else {
        crate::api_runtime_views::engine_telemetry(&state.engine).await
    };
    let sys = System::new_all();
    let resource_map = crate::api_runtime_views::resource_map(
        &state.engine,
        process_resource_snapshot(&sys),
        request.pipeline_id.as_deref(),
        ResourceMapOptions::new(ResourceMapView::Grouped, Some(25)),
    )
    .await;
    let events = state.engine.recent_events(
        request.event_limit.min(events::MAX_EVENTS),
        request.pipeline_id.as_deref(),
    );

    Json(crate::agent_plane::investigation_response(
        request,
        pipeline_exists,
        output_exists,
        selected_pipeline,
        selected_output,
        health,
        graph,
        telemetry,
        resource_map,
        alerts,
        events,
    ))
    .into_response()
}

#[cfg(not(feature = "agent-plane"))]
pub async fn agent_investigation_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(_request): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_plane_unavailable()
}

#[cfg(feature = "agent-plane")]
/// Generates a full plan response for the requested agent action.
pub async fn agent_plan_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_plane::PlanRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let response = build_agent_plan(&state, request).await;
    Json(response).into_response()
}

#[cfg(not(feature = "agent-plane"))]
pub async fn agent_plan_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(_request): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_plane_unavailable()
}

#[cfg(feature = "agent-plane")]
/// Extracts only the validation view from a full agent plan response.
fn agent_plan_validation_json(response: &crate::agent_plane::PlanResponse) -> serde_json::Value {
    serde_json::json!({
        "generatedAt": response.generated_at,
        "planId": response.plan_id,
        "validation": response.validation,
    })
}

#[cfg(feature = "agent-plane")]
/// Extracts only the graph preview view from a full agent plan response.
fn agent_plan_graph_preview_json(response: &crate::agent_plane::PlanResponse) -> serde_json::Value {
    serde_json::json!({
        "generatedAt": response.generated_at,
        "planId": response.plan_id,
        "graphPreview": response.graph_preview,
        "impact": response.impact,
    })
}

#[cfg(feature = "agent-plane")]
/// Generates a plan and returns only its validation payload.
pub async fn agent_plan_validate_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_plane::PlanRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let response = build_agent_plan(&state, request).await;
    Json(agent_plan_validation_json(&response)).into_response()
}

#[cfg(not(feature = "agent-plane"))]
pub async fn agent_plan_validate_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(_request): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_plane_unavailable()
}

#[cfg(feature = "agent-plane")]
/// Generates a plan and returns only its graph-diff preview payload.
pub async fn agent_graph_diff_preview_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_plane::PlanRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let response = build_agent_plan(&state, request).await;
    Json(agent_plan_graph_preview_json(&response)).into_response()
}

#[cfg(not(feature = "agent-plane"))]
pub async fn agent_graph_diff_preview_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(_request): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_plane_unavailable()
}

#[cfg(feature = "agent-execution")]
use super::outputs::{
    CUSTOM_OUTPUT_ENCODING_ERROR, MONITORING_URL_SCHEME_ERROR, OUTPUT_URL_SCHEME_ERROR,
    is_supported_monitoring_url, is_supported_output_url, normalize_monitoring_url,
};
#[cfg(feature = "agent-execution")]
use super::state::MAX_NAME_LEN;
#[cfg(feature = "agent-execution")]
use super::state::MAX_OUTPUT_CONFIG_LEN;
#[cfg(feature = "agent-execution")]
use super::state::MAX_URL_LEN;
#[cfg(feature = "agent-execution")]
use crate::domain::output_spec::OutputConfig;

#[cfg(feature = "agent-execution")]
/// Creates or reuses an execution record for one requested agent operation.
pub async fn agent_operation_create_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_execution::OperationCreateRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let plan = build_agent_plan(&state, request.plan_request()).await;
    let pre_alert_count = current_agent_alert_count(&state).await;
    let result = match state.agent_execution.create(request, plan, pre_alert_count) {
        Ok(result) => result,
        Err(err) => return agent_operation_store_error(err),
    };
    let status = if result.reused {
        StatusCode::OK
    } else {
        StatusCode::CREATED
    };
    (status, Json(result.operation)).into_response()
}

#[cfg(not(feature = "agent-execution"))]
pub async fn agent_operation_create_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(_request): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_execution_unavailable()
}

#[cfg(feature = "agent-execution")]
/// Returns one stored agent operation record by ID.
pub async fn agent_operation_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    match state.agent_execution.get(&operation_id) {
        Some(record) => Json(crate::agent_execution::public_record(&record)).into_response(),
        None => (StatusCode::NOT_FOUND, "Operation not found").into_response(),
    }
}

#[cfg(not(feature = "agent-execution"))]
pub async fn agent_operation_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(_operation_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_execution_unavailable()
}

#[cfg(feature = "agent-execution")]
/// Applies an approval decision to one pending agent operation.
pub async fn agent_operation_approve_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<String>,
    Json(request): Json<crate::agent_execution::ApprovalRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    match state.agent_execution.approve(&operation_id, request) {
        Ok(record) => Json(crate::agent_execution::public_record(&record)).into_response(),
        Err(err) => agent_operation_store_error(err),
    }
}

#[cfg(not(feature = "agent-execution"))]
pub async fn agent_operation_approve_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(_operation_id): Path<String>,
    Json(_request): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_execution_unavailable()
}

#[cfg(feature = "agent-execution")]
/// Applies one approved agent operation and records either its completed
/// outcome or its failure result.
pub async fn agent_operation_apply_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let record = match state.agent_execution.start_apply(&operation_id) {
        Ok(record) => record,
        Err(err) => return agent_operation_store_error(err),
    };

    match execute_agent_operation(&state, &record).await {
        Ok(result) => match state.agent_execution.complete_apply(
            &operation_id,
            result.state_transitions,
            result.progress_snapshots,
            result.execution_result,
        ) {
            Some(record) => Json(crate::agent_execution::public_record(&record)).into_response(),
            None => (StatusCode::NOT_FOUND, "Operation not found").into_response(),
        },
        Err(err) => match state.agent_execution.fail_apply(&operation_id, err.clone()) {
            Some(record) => (
                StatusCode::BAD_REQUEST,
                Json(crate::agent_execution::public_record(&record)),
            )
                .into_response(),
            None => (StatusCode::BAD_REQUEST, err).into_response(),
        },
    }
}

#[cfg(not(feature = "agent-execution"))]
pub async fn agent_operation_apply_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(_operation_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_execution_unavailable()
}

#[cfg(feature = "agent-execution")]
/// Verifies one stored operation by ID against current persisted/runtime state.
pub async fn agent_operation_verify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(operation_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    verify_agent_operation_by_id(&state, &operation_id).await
}

#[cfg(not(feature = "agent-execution"))]
pub async fn agent_operation_verify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Path(_operation_id): Path<String>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_execution_unavailable()
}

#[cfg(feature = "agent-execution")]
/// Verifies one stored operation from an explicit verify request payload.
pub async fn agent_verify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_execution::VerifyRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    verify_agent_operation_by_id(&state, &request.operation_id).await
}

#[cfg(not(feature = "agent-execution"))]
pub async fn agent_verify_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(_request): Json<serde_json::Value>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_execution_unavailable()
}

#[cfg(feature = "agent-plane")]
/// Builds the large redacted agent context document consumed by planning and
/// investigation surfaces.
async fn build_agent_context(state: &AppState) -> serde_json::Value {
    let catalog = state.agent_context_catalog().await;
    let pipelines = catalog.pipelines;
    let pipeline_ids: Vec<String> = pipelines.iter().map(|p| p.id.clone()).collect();
    let outputs = catalog.outputs;
    let jobs = catalog.jobs;
    let jobs_json = api_view_models::job_response_json_list(&jobs);
    let ingests = catalog.ingests;
    let (recording_enabled, health) = agent_health_snapshot(state, &pipeline_ids).await;
    let alerts = alerts::derive_alerts(&health);
    let events = state.engine.recent_events(events::MAX_EVENTS, None);
    let engine_telemetry = crate::api_runtime_views::engine_telemetry(&state.engine).await;
    let sys = System::new_all();
    let resource_map = crate::api_runtime_views::resource_map(
        &state.engine,
        process_resource_snapshot(&sys),
        None,
        ResourceMapOptions::summary(),
    )
    .await;
    let mut pipeline_telemetry = Vec::new();
    let mut graphs = Vec::new();
    for pipeline_id in &pipeline_ids {
        pipeline_telemetry
            .push(crate::api_runtime_views::pipeline_telemetry(&state.engine, pipeline_id).await);
        graphs.push(
            crate::api_runtime_views::processing_graph(&state.engine, pipeline_id, &outputs).await,
        );
    }
    let desired_vs_actual = agent_desired_vs_actual(
        &pipelines,
        &outputs,
        &ingests,
        &jobs,
        &recording_enabled,
        &health,
    );
    let diagnostics = agent_diagnostics_summary(&pipelines, &outputs, &health, &graphs);
    let dependencies = agent_dependency_summary(
        state,
        &pipelines,
        &outputs,
        &ingests,
        &recording_enabled,
        &health,
    )
    .await;

    let bonding_available = state.engine.bonding_available();
    let (mut status, _) = crate::runtime_info::status_and_sbom(bonding_available);
    status["os"] = system_status(&sys);

    let settings = catalog.settings;
    let custom_encoding_len = catalog.custom_encoding_len;
    let configuration = serde_json::json!({
        "serverName": settings
            .as_ref()
            .map(|settings| settings.server_name.clone())
            .unwrap_or_else(|| "Name".to_string()),
        "ingestHost": settings
            .as_ref()
            .map(|settings| settings.ingest_host.clone())
            .unwrap_or_default(),
        "ingestSecurity": settings
            .as_ref()
            .map(|settings| settings.ingest_security.clone())
            .unwrap_or_else(|| state.ingest_security_config()),
        "transcodeProfiles": settings
            .as_ref()
            .map(|settings| settings.transcode_profiles.clone())
            .unwrap_or_else(crate::application::transcode_profiles::default_transcode_profiles),
        "customEncoding": {
            "configured": custom_encoding_len > 0,
            "byteLength": custom_encoding_len,
        },
        "ports": {
            "rtmp": state.ports.rtmp,
            "srt": state.ports.srt,
        }
    });
    let media = agent_media_inventory(state).await;
    let storage = agent_storage_summary(state, &media).await;

    crate::agent_plane::redacted_context(
        &pipelines,
        &outputs,
        &jobs_json,
        &ingests,
        status,
        health,
        engine_telemetry,
        pipeline_telemetry,
        resource_map,
        graphs,
        alerts,
        events,
        configuration,
        media,
        desired_vs_actual,
        diagnostics,
        dependencies,
        storage,
    )
}

#[cfg(feature = "agent-execution")]
struct AgentOperationApplyOutcome {
    state_transitions: Vec<serde_json::Value>,
    progress_snapshots: Vec<serde_json::Value>,
    execution_result: serde_json::Value,
}

#[cfg(feature = "agent-execution")]
/// Executes the change list for one stored agent operation and captures the
/// transition/progress snapshots needed for the public record.
async fn execute_agent_operation(
    state: &AppState,
    record: &crate::agent_execution::OperationRecord,
) -> Result<AgentOperationApplyOutcome, String> {
    let request = record.request.plan_request();
    let catalog = state
        .agent_service
        .try_load_pipeline_output_catalog()
        .await?;
    let pipelines = catalog.pipelines;
    let outputs = catalog.outputs;
    let validation = crate::agent_plane::validate_plan(&request, &pipelines, &outputs);
    if !validation.valid {
        return Err(format!(
            "operation plan is no longer valid: {}",
            serde_json::to_string(&validation.errors).unwrap_or_default()
        ));
    }

    let mut state_transitions = Vec::new();
    let mut progress_snapshots = Vec::new();
    let mut change_results = Vec::new();
    let total = request.proposed_changes.len();

    for (idx, change) in request.proposed_changes.iter().enumerate() {
        let pipeline_id = change
            .pipeline_id
            .as_deref()
            .or(request.pipeline_id.as_deref())
            .ok_or_else(|| "change is missing pipelineId".to_string())?;

        let result = apply_agent_change(state, pipeline_id, change).await?;
        state_transitions.push(serde_json::json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "kind": change.kind,
            "pipelineId": pipeline_id,
            "outputId": result["outputId"],
            "from": result["from"],
            "to": result["to"],
        }));
        progress_snapshots.push(serde_json::json!({
            "at": chrono::Utc::now().to_rfc3339(),
            "completed": idx + 1,
            "total": total,
            "currentChange": change.kind,
            "pipelineId": pipeline_id,
            "outputId": result["outputId"],
        }));
        change_results.push(result);
    }

    Ok(AgentOperationApplyOutcome {
        state_transitions,
        progress_snapshots,
        execution_result: crate::agent_plane::redact_secrets(serde_json::json!({
            "success": true,
            "appliedAt": chrono::Utc::now().to_rfc3339(),
            "changeCount": total,
            "changeResults": change_results,
        })),
    })
}

#[cfg(feature = "agent-execution")]
/// Dispatches one proposed change to the concrete output/state mutation helper
/// that owns that change type.
async fn apply_agent_change(
    state: &AppState,
    pipeline_id: &str,
    change: &crate::agent_plane::ProposedChange,
) -> Result<serde_json::Value, String> {
    match change.kind.as_str() {
        "addOutput" => apply_agent_add_output(state, pipeline_id, change).await,
        "updateOutput" => apply_agent_update_output(state, pipeline_id, change).await,
        "removeOutput" => apply_agent_remove_output(state, pipeline_id, change).await,
        "startOutput" => {
            apply_agent_desired_state(state, pipeline_id, change, DesiredOutputState::Running).await
        }
        "stopOutput" => {
            apply_agent_desired_state(state, pipeline_id, change, DesiredOutputState::Stopped).await
        }
        other => Err(format!("unsupported change kind '{other}'")),
    }
}

#[cfg(feature = "agent-execution")]
/// Creates one output described by an agent change after validating its
/// transport-facing fields.
async fn apply_agent_add_output(
    state: &AppState,
    pipeline_id: &str,
    change: &crate::agent_plane::ProposedChange,
) -> Result<serde_json::Value, String> {
    let name = required_change_field(change.name.as_deref(), "name")?;
    let url = required_change_field(change.url.as_deref(), "url")?.trim();
    let monitoring_url = normalize_monitoring_url(change.monitoring_url.as_deref())?;
    let config = change
        .config
        .as_ref()
        .ok_or_else(|| "change is missing required field 'config'".to_string())?;
    let desired_state = change
        .desired_state
        .as_deref()
        .unwrap_or(DesiredOutputState::Stopped.as_str())
        .trim();
    validate_output_fields(name, url, monitoring_url.as_deref(), config, desired_state)?;

    let output_id = change
        .output_id
        .clone()
        .unwrap_or_else(|| format!("output_agent_{}", to_hex(&rand::random::<[u8; 8]>())));
    let output = state
        .output_service
        .create_output(
            &output_id,
            pipeline_id,
            name.trim(),
            url,
            monitoring_url.as_deref(),
            desired_state,
            config,
        )
        .await
        .map_err(|err| format!("failed to create output: {err}"))?;

    Ok(serde_json::json!({
        "kind": "addOutput",
        "pipelineId": pipeline_id,
        "outputId": output.id,
        "status": "created",
        "from": null,
        "to": output,
    }))
}

#[cfg(feature = "agent-execution")]
/// Updates one existing output described by an agent change, including desired
/// state transitions when requested.
async fn apply_agent_update_output(
    state: &AppState,
    pipeline_id: &str,
    change: &crate::agent_plane::ProposedChange,
) -> Result<serde_json::Value, String> {
    let output_id = required_change_field(change.output_id.as_deref(), "outputId")?;
    let existing = state
        .output_service
        .get_by_id(pipeline_id, output_id)
        .await
        .map_err(|err| format!("failed to read output: {err}"))?;
    let name = change.name.as_deref().unwrap_or(&existing.name);
    let url = change.url.as_deref().unwrap_or(&existing.url).trim();
    let monitoring_url = change
        .monitoring_url
        .as_deref()
        .map(str::trim)
        .map(str::to_string)
        .or_else(|| existing.monitoring_url.clone());
    let config = change.config.as_ref().unwrap_or(&existing.config);
    let desired_state = change
        .desired_state
        .as_deref()
        .unwrap_or(existing.desired_state.as_str())
        .trim();
    validate_output_fields(name, url, monitoring_url.as_deref(), config, desired_state)?;

    let mut updated = state
        .output_service
        .update_output(
            pipeline_id,
            output_id,
            name.trim(),
            url,
            monitoring_url.as_deref(),
            config,
        )
        .await
        .map_err(|err| format!("failed to update output: {err}"))?;
    let desired_state = DesiredOutputState::from(desired_state);
    if desired_state != existing.desired_state {
        updated = match desired_state {
            DesiredOutputState::Running => state
                .output_service
                .request_start(pipeline_id, output_id)
                .await
                .map_err(|err| format!("failed to update desired state: {err}"))?,
            DesiredOutputState::Stopped => state
                .output_service
                .request_stop(pipeline_id, output_id)
                .await
                .map_err(|err| format!("failed to update desired state: {err}"))?,
            DesiredOutputState::Failed => {
                return Err("agent output updates cannot request failed state".to_string());
            }
        };
    }

    Ok(serde_json::json!({
        "kind": "updateOutput",
        "pipelineId": pipeline_id,
        "outputId": output_id,
        "status": "updated",
        "from": existing,
        "to": updated,
    }))
}

#[cfg(feature = "agent-execution")]
/// Removes one output referenced by an agent change.
async fn apply_agent_remove_output(
    state: &AppState,
    pipeline_id: &str,
    change: &crate::agent_plane::ProposedChange,
) -> Result<serde_json::Value, String> {
    let output_id = required_change_field(change.output_id.as_deref(), "outputId")?;
    let existing = state
        .output_service
        .get_by_id(pipeline_id, output_id)
        .await
        .map_err(|err| format!("failed to read output: {err}"))?;
    state.engine.unregister_egress(output_id).await;
    let deleted = state
        .output_service
        .delete_output(pipeline_id, output_id)
        .await
        .map_err(|err| format!("failed to delete output: {err}"))?;
    if !deleted {
        return Err(format!(
            "output '{output_id}' not found on pipeline '{pipeline_id}'"
        ));
    }
    Ok(serde_json::json!({
        "kind": "removeOutput",
        "pipelineId": pipeline_id,
        "outputId": output_id,
        "status": "deleted",
        "from": existing,
        "to": null,
    }))
}

#[cfg(feature = "agent-execution")]
/// Applies a start/stop desired-state request for one output referenced by an
/// agent change.
async fn apply_agent_desired_state(
    state: &AppState,
    pipeline_id: &str,
    change: &crate::agent_plane::ProposedChange,
    desired_state: DesiredOutputState,
) -> Result<serde_json::Value, String> {
    let output_id = required_change_field(change.output_id.as_deref(), "outputId")?;
    let existing = state
        .output_service
        .get_by_id(pipeline_id, output_id)
        .await
        .map_err(|err| format!("failed to read output: {err}"))?;
    let output = match desired_state {
        DesiredOutputState::Running => state
            .output_service
            .request_start(pipeline_id, output_id)
            .await
            .map_err(|err| format!("failed to set desired state: {err}"))?,
        DesiredOutputState::Stopped => state
            .output_service
            .request_stop(pipeline_id, output_id)
            .await
            .map_err(|err| format!("failed to set desired state: {err}"))?,
        DesiredOutputState::Failed => {
            return Err("agent output actions cannot request failed state".to_string());
        }
    };
    Ok(serde_json::json!({
        "kind": change.kind,
        "pipelineId": pipeline_id,
        "outputId": output_id,
        "status": "desiredStateUpdated",
        "from": existing,
        "to": output,
    }))
}

#[cfg(feature = "agent-execution")]
/// Pulls one required string field out of a change payload and returns a
/// stable validation error when it is missing.
fn required_change_field<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("change is missing required field '{field}'"))
}

#[cfg(feature = "agent-execution")]
/// Validates output-facing change fields before they are handed to the output
/// service.
fn validate_output_fields(
    name: &str,
    url: &str,
    monitoring_url: Option<&str>,
    config: &OutputConfig,
    desired_state: &str,
) -> Result<(), String> {
    let config_json = serde_json::to_string(config)
        .map_err(|err| format!("config must serialize to JSON: {err}"))?;
    validate_len("name", name, MAX_NAME_LEN)?;
    validate_len("url", url, MAX_URL_LEN)?;
    validate_len("config", &config_json, MAX_OUTPUT_CONFIG_LEN)?;
    if let Some(monitoring_url) = monitoring_url {
        validate_len("monitoring_url", monitoring_url, MAX_URL_LEN)?;
    }
    if config.is_custom_output() {
        return Err(CUSTOM_OUTPUT_ENCODING_ERROR.to_string());
    }
    if !is_supported_output_url(url) {
        return Err(OUTPUT_URL_SCHEME_ERROR.to_string());
    }
    if let Some(monitoring_url) = monitoring_url
        && !is_supported_monitoring_url(monitoring_url)
    {
        return Err(MONITORING_URL_SCHEME_ERROR.to_string());
    }
    if !matches!(desired_state, "running" | "stopped") {
        return Err("desiredState must be either 'running' or 'stopped'".to_string());
    }
    Ok(())
}

#[cfg(feature = "agent-execution")]
/// Shared max-length validator for change fields that map onto output service
/// limits.
fn validate_len(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.len() > max {
        Err(format!("{field} exceeds maximum length of {max} bytes"))
    } else {
        Ok(())
    }
}

#[cfg(any(feature = "agent-plane", feature = "agent-execution"))]
fn pipeline_input_is_on(pipeline_health: &serde_json::Value) -> bool {
    pipeline_health["input"]["status"].as_str() == Some("on")
}

#[cfg(feature = "agent-execution")]
fn output_runtime_is_running(runtime: &serde_json::Value) -> bool {
    runtime["status"].as_str() == Some("running")
}

#[cfg(feature = "agent-plane")]
fn desired_output_reason(
    desired_state: DesiredOutputState,
    actual_status: &str,
    input_is_on: bool,
) -> &'static str {
    if desired_state == DesiredOutputState::Running && !input_is_on {
        "pendingInput"
    } else if (desired_state == DesiredOutputState::Running && actual_status == "running")
        || (desired_state == DesiredOutputState::Stopped && actual_status != "running")
    {
        "converged"
    } else {
        "desiredActualMismatch"
    }
}

#[cfg(feature = "agent-execution")]
/// Verifies one stored operation by ID and translates a missing record into a
/// 404 response.
async fn verify_agent_operation_by_id(
    state: &AppState,
    operation_id: &str,
) -> axum::response::Response {
    let record = match state.agent_execution.get(operation_id) {
        Some(record) => record,
        None => return (StatusCode::NOT_FOUND, "Operation not found").into_response(),
    };
    let verification = verify_agent_operation(state, &record).await;
    match state
        .agent_execution
        .complete_verify(operation_id, verification)
    {
        Some(record) => Json(crate::agent_execution::public_record(&record)).into_response(),
        None => (StatusCode::NOT_FOUND, "Operation not found").into_response(),
    }
}

#[cfg(feature = "agent-execution")]
// Verification stays at the API boundary because it compares the requested
// change intent against both persisted desired state and the latest runtime
// health snapshot.
async fn verify_agent_operation(
    state: &AppState,
    record: &crate::agent_execution::OperationRecord,
) -> serde_json::Value {
    let catalog = state.agent_service.load_pipeline_output_catalog().await;
    let pipelines = catalog.pipelines;
    let pipeline_ids: Vec<String> = pipelines
        .iter()
        .map(|pipeline| pipeline.id.clone())
        .collect();
    let outputs = catalog.outputs;
    let (_recording_enabled, health) = agent_health_snapshot(state, &pipeline_ids).await;
    let alerts = alerts::derive_alerts(&health);
    let mut checks = Vec::new();
    let mut success = true;

    for change in &record.request.proposed_changes {
        let pipeline_id = change
            .pipeline_id
            .as_deref()
            .or(record.request.pipeline_id.as_deref())
            .unwrap_or_default();
        let output_id = agent_change_output_id(record, change);
        let output = output_id.as_deref().and_then(|oid| {
            outputs
                .iter()
                .find(|output| output.pipeline_id == pipeline_id && output.id == oid)
        });
        let runtime = output_id
            .as_deref()
            .map(|oid| &health["pipelines"][pipeline_id]["outputs"][oid]);
        let (passed, reason) = match change.kind.as_str() {
            "addOutput" | "updateOutput" => {
                if let Some(output) = output {
                    if let Some(desired) = change.desired_state.as_deref()
                        && output.desired_state.as_str() != desired
                    {
                        (false, "desiredStateMismatch")
                    } else if change.desired_state.as_deref() == Some("running") {
                        let status = runtime
                            .and_then(|runtime| runtime["status"].as_str())
                            .unwrap_or("off");
                        if status == "running" {
                            (true, "running")
                        } else if !pipeline_input_is_on(&health["pipelines"][pipeline_id]) {
                            (false, "pendingInput")
                        } else {
                            (false, "notRunning")
                        }
                    } else if change.desired_state.as_deref() == Some("stopped") {
                        let status = runtime.and_then(|runtime| runtime["status"].as_str());
                        if status != Some("running") {
                            (true, "stopped")
                        } else {
                            (false, "stillRunning")
                        }
                    } else {
                        (true, "persisted")
                    }
                } else {
                    (false, "outputMissing")
                }
            }
            "removeOutput" => {
                if output.is_none() {
                    (true, "removed")
                } else {
                    (false, "stillPresent")
                }
            }
            "startOutput" => {
                if output.is_some_and(|output| output.desired_state == DesiredOutputState::Running)
                    && runtime.is_some_and(output_runtime_is_running)
                {
                    (true, "running")
                } else if !pipeline_input_is_on(&health["pipelines"][pipeline_id]) {
                    (false, "pendingInput")
                } else {
                    (false, "notRunning")
                }
            }
            "stopOutput" => {
                if output.is_some_and(|output| output.desired_state == DesiredOutputState::Stopped)
                    && runtime.and_then(|runtime| runtime["status"].as_str()) != Some("running")
                {
                    (true, "stopped")
                } else {
                    (false, "stillRunning")
                }
            }
            _ => (false, "unsupportedChangeKind"),
        };
        success &= passed;
        checks.push(serde_json::json!({
            "kind": change.kind,
            "pipelineId": pipeline_id,
            "outputId": output_id,
            "passed": passed,
            "reason": reason,
            "runtime": runtime.cloned().unwrap_or(serde_json::Value::Null),
        }));
    }

    let mut graphs = Vec::new();
    for pipeline_id in &pipeline_ids {
        graphs.push(
            crate::api_runtime_views::processing_graph(&state.engine, pipeline_id, &outputs).await,
        );
    }
    let active_graph_nodes = graphs
        .iter()
        .filter_map(|graph| graph["nodes"].as_array())
        .flatten()
        .filter(|node| node["active"].as_bool().unwrap_or(false))
        .count();
    let alert_delta = alerts.len() as isize - record.pre_apply_alert_count.unwrap_or(0) as isize;

    crate::agent_plane::redact_secrets(serde_json::json!({
        "success": success,
        "verifiedAt": chrono::Utc::now().to_rfc3339(),
        "postChangeHealth": health,
        "freshnessRecovery": {
            "checked": true,
            "pipelineCount": pipeline_ids.len(),
        },
        "graphConvergence": {
            "checked": true,
            "graphCount": graphs.len(),
            "activeNodes": active_graph_nodes,
        },
        "incidentDelta": {
            "preApplyAlertCount": record.pre_apply_alert_count,
            "postApplyAlertCount": alerts.len(),
            "delta": alert_delta,
        },
        "checks": checks,
        "explanation": if success {
            "All operation checks matched persisted state and runtime expectations."
        } else {
            "One or more operation checks did not match persisted state or runtime expectations."
        },
    }))
}

#[cfg(feature = "agent-execution")]
fn agent_change_output_id(
    record: &crate::agent_execution::OperationRecord,
    change: &crate::agent_plane::ProposedChange,
) -> Option<String> {
    if let Some(output_id) = &change.output_id {
        return Some(output_id.clone());
    }
    let change_results = record
        .execution_result
        .as_ref()
        .and_then(|result| result["changeResults"].as_array())?;
    change_results
        .iter()
        .find(|result| {
            result["kind"].as_str() == Some(change.kind.as_str())
                && result["pipelineId"].as_str()
                    == change
                        .pipeline_id
                        .as_deref()
                        .or(record.request.pipeline_id.as_deref())
        })
        .and_then(|result| result["outputId"].as_str())
        .map(ToOwned::to_owned)
}

#[cfg(feature = "agent-execution")]
async fn current_agent_alert_count(state: &AppState) -> usize {
    let catalog = state.agent_service.load_pipeline_output_catalog().await;
    let pipelines = catalog.pipelines;
    let pipeline_ids: Vec<String> = pipelines
        .iter()
        .map(|pipeline| pipeline.id.clone())
        .collect();
    let (_recording_enabled, health) = agent_health_snapshot(state, &pipeline_ids).await;
    alerts::derive_alerts(&health).len()
}

#[cfg(feature = "agent-plane")]
async fn agent_media_inventory(state: &AppState) -> serde_json::Value {
    let files = state
        .media_library_service
        .list_media_files(&state.media_dir)
        .await;
    serde_json::json!({
        "mediaDir": state.media_dir,
        "files": files,
    })
}

#[cfg(feature = "agent-plane")]
// Desired-vs-actual is a read-only summary for agent context consumers, so it
// lives here with the transport-facing JSON shaping instead of in persistence.
/// Summarizes desired-versus-actual pipeline/output state for planning and
/// investigation consumers.
fn agent_desired_vs_actual(
    pipelines: &[Pipeline],
    outputs: &[crate::application::models::Output],
    ingests: &[Ingest],
    jobs: &[crate::application::models::Job],
    recording_enabled: &std::collections::HashMap<String, bool>,
    health: &serde_json::Value,
) -> serde_json::Value {
    let mut pipeline_reports = Vec::new();
    let mut drift_count = 0usize;
    let mut converged_count = 0usize;
    let mut pending_count = 0usize;

    for pipeline in pipelines {
        let pipeline_health = &health["pipelines"][&pipeline.id];
        let input_status = pipeline_health["input"]["status"].as_str().unwrap_or("off");
        let input_is_on = pipeline_input_is_on(pipeline_health);
        let file_ingests: Vec<_> = ingests
            .iter()
            .filter(|ingest| ingest.stream_key == pipeline.stream_key)
            .collect();
        let input_desired = if file_ingests.is_empty() {
            "externalPublisherOptional"
        } else {
            "fileIngestConfigured"
        };

        let pipeline_outputs: Vec<_> = outputs
            .iter()
            .filter(|output| output.pipeline_id == pipeline.id)
            .collect();
        let mut output_reports = Vec::new();
        for output in pipeline_outputs {
            let runtime = &pipeline_health["outputs"][&output.id];
            let actual = runtime["status"].as_str().unwrap_or("stopped");
            let reason = desired_output_reason(output.desired_state, actual, input_is_on);
            match reason {
                "pendingInput" => pending_count += 1,
                "converged" => converged_count += 1,
                _ => drift_count += 1,
            }
            let recent_jobs: Vec<_> = jobs
                .iter()
                .filter(|job| job.pipeline_id == pipeline.id && job.output_id == output.id)
                .take(5)
                .map(|job| {
                    crate::agent_plane::redact_secrets_from_serializable(
                        &api_view_models::job_response_json(job),
                    )
                })
                .collect();
            output_reports.push(serde_json::json!({
                "outputId": output.id,
                "name": output.name,
                "desiredState": output.desired_state,
                "actualStatus": actual,
                "actualPhase": runtime["phase"],
                "converged": reason == "converged",
                "reason": reason,
                "config": output.config,
                "recentJobs": recent_jobs,
            }));
        }

        let recording_desired = recording_enabled
            .get(&pipeline.id)
            .copied()
            .unwrap_or(false);
        let recording_active = pipeline_health["recording"]["active"]
            .as_bool()
            .unwrap_or(false);
        let recording_reason = if recording_desired == recording_active {
            "converged"
        } else if recording_desired && input_status != "on" {
            "pendingInput"
        } else {
            "desiredActualMismatch"
        };

        pipeline_reports.push(serde_json::json!({
            "pipelineId": pipeline.id,
            "name": pipeline.name,
            "input": {
                "desired": input_desired,
                "actualStatus": input_status,
                "fileIngestCount": file_ingests.len(),
                "externalPublishersAllowed": true
            },
            "outputs": output_reports,
            "recording": {
                "desiredEnabled": recording_desired,
                "actualActive": recording_active,
                "converged": recording_reason == "converged",
                "reason": recording_reason
            },
            "hlsPreview": {
                "desired": "onDemand",
                "actualActive": pipeline_health["hlsPreview"]["active"].as_bool().unwrap_or(false)
            }
        }));
    }

    serde_json::json!({
        "summary": {
            "pipelines": pipelines.len(),
            "outputs": outputs.len(),
            "convergedOutputs": converged_count,
            "pendingOutputs": pending_count,
            "driftedOutputs": drift_count,
        },
        "pipelines": pipeline_reports,
    })
}

#[cfg(feature = "agent-plane")]
/// Builds the condensed diagnostics summary section used in agent context and
/// investigation responses.
fn agent_diagnostics_summary(
    pipelines: &[Pipeline],
    outputs: &[crate::application::models::Output],
    health: &serde_json::Value,
    graphs: &[serde_json::Value],
) -> serde_json::Value {
    let mut pipeline_reports = Vec::new();
    for pipeline in pipelines {
        let pipeline_health = &health["pipelines"][&pipeline.id];
        let graph = graphs
            .iter()
            .find(|graph| graph["pipelineId"].as_str() == Some(pipeline.id.as_str()));
        let inactive_nodes = graph
            .and_then(|graph| graph["nodes"].as_array())
            .map(|nodes| {
                nodes
                    .iter()
                    .filter(|node| !node["active"].as_bool().unwrap_or(false))
                    .map(|node| {
                        serde_json::json!({
                            "id": node["id"],
                            "type": node["type"],
                            "label": node["label"],
                        })
                    })
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        let desired_running_outputs = outputs
            .iter()
            .filter(|output| {
                output.pipeline_id == pipeline.id
                    && output.desired_state == DesiredOutputState::Running
            })
            .count();
        let actual_running_outputs = pipeline_health["outputs"]
            .as_object()
            .map(|outputs| {
                outputs
                    .values()
                    .filter(|output| output["status"].as_str() == Some("running"))
                    .count()
            })
            .unwrap_or(0);
        let mut findings = Vec::new();
        if pipeline_health["input"]["status"].as_str() != Some("on") {
            findings.push(serde_json::json!({
                "severity": "critical",
                "code": "noActivePublisher",
                "message": "Pipeline has no active publisher."
            }));
        }
        if actual_running_outputs < desired_running_outputs {
            findings.push(serde_json::json!({
                "severity": "warning",
                "code": "desiredOutputsNotRunning",
                "message": "One or more desired running outputs are not active.",
                "desiredRunningOutputs": desired_running_outputs,
                "actualRunningOutputs": actual_running_outputs
            }));
        }
        pipeline_reports.push(serde_json::json!({
            "pipelineId": pipeline.id,
            "passive": true,
            "activeProbeEndpoint": format!("/api/v1/pipelines/{}/diagnostics/run", pipeline.id),
            "activeProbeMethod": "POST",
            "includedActiveProbeResults": false,
            "reason": "The context endpoint is read-only and does not run active diagnostics checks.",
            "inactiveGraphNodes": inactive_nodes,
            "findings": findings,
        }));
    }

    serde_json::json!({
        "activeProbeEndpointTemplate": "/api/v1/pipelines/:pipeline_id/diagnostics/run",
        "activeProbeMethod": "POST",
        "includedActiveProbeResults": false,
        "pipelines": pipeline_reports,
    })
}

#[cfg(feature = "agent-plane")]
/// Summarizes HLS, recording, file-ingest, and ingest-security dependencies
/// for agent planning surfaces.
async fn agent_dependency_summary(
    state: &AppState,
    pipelines: &[Pipeline],
    outputs: &[crate::application::models::Output],
    ingests: &[Ingest],
    recording_enabled: &std::collections::HashMap<String, bool>,
    health: &serde_json::Value,
) -> serde_json::Value {
    let mut hls = Vec::new();
    let mut recordings = Vec::new();
    for pipeline in pipelines {
        let snapshot = state.engine.hls_dependency_snapshot(&pipeline.id).await;
        hls.push(serde_json::json!({
            "pipelineId": pipeline.id,
            "storeExists": snapshot.store_exists,
            "active": snapshot.active,
            "persistentConsumers": snapshot.persistent_consumers,
            "lastAccessAgeMs": snapshot.last_access_age_ms,
            "segments": snapshot.segments,
            "playlistBytes": snapshot.playlist_bytes,
        }));

        let desired_enabled = recording_enabled
            .get(&pipeline.id)
            .copied()
            .unwrap_or(false);
        let active = state.engine.is_recording_active(&pipeline.id).await;
        recordings.push(serde_json::json!({
            "pipelineId": pipeline.id,
            "desiredEnabled": desired_enabled,
            "active": active,
            "inputStatus": health["pipelines"][&pipeline.id]["input"]["status"],
        }));
    }

    let mut file_ingest = Vec::new();
    let file_ingest_backend = if state.engine.config.use_internal_file_ingest {
        "internal"
    } else {
        "ffmpeg-subprocess"
    };
    for ingest in ingests {
        let media_path = FsPath::new(&state.media_dir).join(&ingest.filename);
        let runtime = state
            .engine
            .file_ingest_dependency_snapshot(&ingest.id)
            .await;
        file_ingest.push(serde_json::json!({
            "id": ingest.id,
            "filename": ingest.filename,
            "mediaExists": media_path.exists(),
            "markedActive": runtime.marked_active,
            "childRegistered": runtime.child_registered,
            "backend": file_ingest_backend,
            "loop": ingest.loop_flag,
            "startTime": ingest.start_time,
            "liveOptimized": ingest.live_optimized,
            "targetGopSeconds": ingest.target_gop_seconds,
            "streamKey": ingest.stream_key,
        }));
    }

    let hls_output_count = outputs
        .iter()
        .filter(|output| OutputUrlScheme::from_url(&output.url).is_hls_family())
        .count();

    serde_json::json!({
        "hls": {
            "config": {
                "minSegmentSecs": state.engine.config.hls_min_segment_ms,
                "segmentCapacity": state.engine.config.hls_segment_capacity_bytes,
                "maxSegments": state.engine.config.hls_max_segments,
            },
            "outputCount": hls_output_count,
            "pipelines": hls,
        },
        "recording": {
            "pipelines": recordings,
        },
        "fileIngest": {
            "configured": file_ingest.len(),
            "backend": file_ingest_backend,
            "ingests": file_ingest,
        },
        "ingestSecurity": {
            "config": state.ingest_security_config(),
            "loopbackExempt": true,
            "trackedIpRuntimeStateRedacted": true,
        }
    })
}

#[cfg(feature = "agent-plane")]
/// Builds the storage summary subsection used in the agent context payload.
async fn agent_storage_summary(state: &AppState, media: &serde_json::Value) -> serde_json::Value {
    let media_bytes = media["files"]
        .as_array()
        .map(|files| {
            files
                .iter()
                .filter_map(|file| file["size"].as_u64())
                .sum::<u64>()
        })
        .unwrap_or(0);
    let media_file_count = media["files"]
        .as_array()
        .map(|files| files.len())
        .unwrap_or(0);
    let media_root = std::fs::canonicalize(&state.media_dir)
        .ok()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| state.media_dir.clone());

    let disks = Disks::new_with_refreshed_list();
    let mut selected_disk = None;
    for disk in disks.list() {
        if FsPath::new(&media_root).starts_with(disk.mount_point()) {
            selected_disk = Some(serde_json::json!({
                "mountPoint": disk.mount_point().display().to_string(),
                "totalBytes": disk.total_space(),
                "availableBytes": disk.available_space(),
            }));
        }
    }

    serde_json::json!({
        "mediaDir": state.media_dir,
        "mediaRoot": media_root,
        "mediaFileCount": media_file_count,
        "mediaBytes": media_bytes,
        "disk": selected_disk,
        "databasePath": state.db_path,
    })
}

#[cfg(feature = "agent-plane")]
/// Builds the full plan response by combining the request with the current
/// pipeline/output catalog and optional current graph.
async fn build_agent_plan(
    state: &AppState,
    request: crate::agent_plane::PlanRequest,
) -> crate::agent_plane::PlanResponse {
    let catalog = state.agent_service.load_pipeline_output_catalog().await;
    let pipelines = catalog.pipelines;
    let outputs = catalog.outputs;
    let current_graph = if let Some(pid) = request.pipeline_id.as_deref()
        && pipelines.iter().any(|p| p.id == pid)
    {
        Some(crate::api_runtime_views::processing_graph(&state.engine, pid, &outputs).await)
    } else {
        None
    };
    crate::agent_plane::plan_response(request, &pipelines, &outputs, current_graph.as_ref())
}

#[cfg(feature = "agent-execution")]
/// Normalizes execution-store errors into stable HTTP responses for agent
/// operation routes.
fn agent_operation_store_error(
    err: crate::agent_execution::OperationStoreError,
) -> axum::response::Response {
    use crate::agent_execution::OperationStoreError;

    let (status, code, message) = match err {
        OperationStoreError::NotFound => (
            StatusCode::NOT_FOUND,
            "operationNotFound",
            "Operation not found",
        ),
        OperationStoreError::IdempotencyConflict => (
            StatusCode::CONFLICT,
            "idempotencyConflict",
            "Idempotency key was already used for a different operation",
        ),
        OperationStoreError::Invalid => (
            StatusCode::CONFLICT,
            "operationInvalid",
            "Operation is invalid and cannot advance",
        ),
        OperationStoreError::RequiresApproval => (
            StatusCode::CONFLICT,
            "approvalRequired",
            "Operation must be approved before apply",
        ),
        OperationStoreError::AlreadyApplying => (
            StatusCode::CONFLICT,
            "alreadyApplying",
            "Operation is already applying",
        ),
        OperationStoreError::AlreadyTerminal => (
            StatusCode::CONFLICT,
            "alreadyTerminal",
            "Operation has already reached a terminal state",
        ),
    };
    (
        status,
        Json(serde_json::json!({
            "error": message,
            "code": code,
        })),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "agent-plane")]
    use super::{
        agent_plan_graph_preview_json, agent_plan_validation_json, desired_output_reason,
        pipeline_input_is_on,
    };
    #[cfg(feature = "agent-plane")]
    use crate::domain::state::DesiredOutputState;

    #[cfg(feature = "agent-plane")]
    fn sample_plan_response() -> crate::agent_plane::PlanResponse {
        crate::agent_plane::PlanResponse {
            generated_at: "2026-07-14T00:00:00Z".to_string(),
            plan_id: "plan_123".to_string(),
            status: "draft",
            intent: "Test plan".to_string(),
            execution_enabled: true,
            execution_note: "compiled in",
            steps: Vec::new(),
            validation: crate::agent_plane::ValidationResult {
                valid: true,
                errors: Vec::new(),
                warnings: Vec::new(),
            },
            graph_preview: crate::agent_plane::GraphPreview {
                mode: "preview",
                added_nodes: Vec::new(),
                removed_nodes: Vec::new(),
                changed_edges: Vec::new(),
                notes: vec!["note".to_string()],
            },
            impact: crate::agent_plane::ImpactPreview {
                affected_pipelines: vec!["pipe-1".to_string()],
                affected_outputs: Vec::new(),
                shared_stage_candidates: Vec::new(),
                operator_summary: "summary".to_string(),
                engineering_notes: Vec::new(),
            },
        }
    }

    #[cfg(feature = "agent-plane")]
    #[test]
    fn agent_plan_validation_json_projects_validation_fields() {
        let value = agent_plan_validation_json(&sample_plan_response());

        assert_eq!(value["planId"], "plan_123");
        assert_eq!(value["validation"]["valid"], true);
    }

    #[cfg(feature = "agent-plane")]
    #[test]
    fn agent_plan_graph_preview_json_projects_graph_preview_fields() {
        let value = agent_plan_graph_preview_json(&sample_plan_response());

        assert_eq!(value["planId"], "plan_123");
        assert_eq!(value["graphPreview"]["mode"], "preview");
        assert_eq!(value["impact"]["operatorSummary"], "summary");
    }

    #[cfg(feature = "agent-plane")]
    #[test]
    fn runtime_status_helpers_capture_agent_output_policy() {
        assert!(pipeline_input_is_on(&serde_json::json!({
            "input": { "status": "on" }
        })));
        assert!(!pipeline_input_is_on(&serde_json::json!({
            "input": { "status": "off" }
        })));
        assert_eq!(
            serde_json::json!({
                "status": "running"
            })["status"]
                .as_str(),
            Some("running")
        );
        assert_ne!(
            serde_json::json!({
                "status": "stopped"
            })["status"]
                .as_str(),
            Some("running")
        );
    }

    #[cfg(feature = "agent-plane")]
    #[test]
    fn desired_output_reason_distinguishes_converged_pending_and_drifted() {
        assert_eq!(
            desired_output_reason(DesiredOutputState::Running, "running", true),
            "converged"
        );
        assert_eq!(
            desired_output_reason(DesiredOutputState::Running, "stopped", false),
            "pendingInput"
        );
        assert_eq!(
            desired_output_reason(DesiredOutputState::Stopped, "running", true),
            "desiredActualMismatch"
        );
    }
}
