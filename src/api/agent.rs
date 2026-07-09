use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::IntoResponse,
};
use std::sync::Arc;

#[cfg(feature = "agent-plane")]
use crate::domain::state::DesiredOutputState;

use super::state::{AppState, require_authenticated};

#[cfg(not(feature = "agent-plane"))]
fn agent_plane_unavailable() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": "agent-plane feature is not compiled in",
            "feature": "agent-plane",
            "compiledIn": false
        })),
    )
        .into_response()
}

#[cfg(not(feature = "agent-execution"))]
fn agent_execution_unavailable() -> axum::response::Response {
    (
        StatusCode::NOT_FOUND,
        Json(serde_json::json!({
            "error": "agent-execution feature is not compiled in",
            "feature": "agent-execution",
            "compiledIn": false
        })),
    )
        .into_response()
}

#[cfg(feature = "agent-plane")]
use super::state::recording_enabled_map;
#[cfg(feature = "agent-execution")]
use super::state::to_hex;
#[cfg(feature = "agent-plane")]
use super::telemetry::system_status;
#[cfg(feature = "agent-plane")]
use crate::alerts;
#[cfg(feature = "agent-plane")]
use crate::api_view_models;
#[cfg(feature = "agent-plane")]
use crate::application::ports::SqliteMetaStore;
#[cfg(feature = "agent-plane")]
use crate::application::settings::load_settings_snapshot;
#[cfg(feature = "agent-plane")]
use crate::db;
#[cfg(feature = "agent-plane")]
use crate::domain::output_spec::OutputUrlScheme;
#[cfg(feature = "agent-plane")]
use crate::events;
#[cfg(feature = "agent-plane")]
use crate::types::{Ingest, Pipeline};
#[cfg(feature = "agent-plane")]
use std::path::Path as FsPath;
#[cfg(feature = "agent-plane")]
use sysinfo::{Disks, System};

#[cfg(feature = "agent-plane")]
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
pub async fn agent_investigation_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_plane::InvestigationRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let pipelines = db::list_pipelines(&state.db).await.unwrap_or_default();
    let outputs = db::list_outputs(&state.db).await.unwrap_or_default();
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
    let recording_enabled = recording_enabled_map(&state, &pipeline_ids).await;
    let health = crate::api_runtime_views::health_snapshot(
        &state.engine,
        &pipeline_ids,
        &recording_enabled,
        0,
    )
    .await;
    let alerts = alerts::derive_alerts(&health);
    let graph = if let Some(pid) = request.pipeline_id.as_deref()
        && pipeline_exists
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
pub async fn agent_plan_validate_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_plane::PlanRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let response = build_agent_plan(&state, request).await;
    Json(serde_json::json!({
        "generatedAt": response.generated_at,
        "planId": response.plan_id,
        "validation": response.validation,
    }))
    .into_response()
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
pub async fn agent_graph_diff_preview_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_plane::PlanRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let response = build_agent_plan(&state, request).await;
    Json(serde_json::json!({
        "generatedAt": response.generated_at,
        "planId": response.plan_id,
        "graphPreview": response.graph_preview,
        "impact": response.impact,
    }))
    .into_response()
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
use super::state::MAX_ENCODING_LEN;
#[cfg(feature = "agent-execution")]
use super::state::MAX_NAME_LEN;
#[cfg(feature = "agent-execution")]
use super::state::MAX_URL_LEN;
#[cfg(feature = "agent-execution")]
use crate::domain::output_spec::OutputConfig;

#[cfg(feature = "agent-execution")]
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
    let result = state.agent_execution.create(request, plan, pre_alert_count);
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
async fn build_agent_context(state: &AppState) -> serde_json::Value {
    let pipelines = db::list_pipelines(&state.db).await.unwrap_or_default();
    let pipeline_ids: Vec<String> = pipelines.iter().map(|p| p.id.clone()).collect();
    let outputs = db::list_outputs(&state.db).await.unwrap_or_default();
    let jobs = db::list_jobs(&state.db).await.unwrap_or_default();
    let jobs_json = api_view_models::job_response_json_list(&jobs);
    let ingests = db::list_ingests(&state.db).await.unwrap_or_default();
    let recording_enabled = recording_enabled_map(state, &pipeline_ids).await;
    let health = crate::api_runtime_views::health_snapshot(
        &state.engine,
        &pipeline_ids,
        &recording_enabled,
        0,
    )
    .await;
    let alerts = alerts::derive_alerts(&health);
    let events = state.engine.recent_events(events::MAX_EVENTS, None);
    let engine_telemetry = crate::api_runtime_views::engine_telemetry(&state.engine).await;
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
    let sys = System::new_all();
    status["os"] = system_status(&sys);

    let settings_store = SqliteMetaStore::new(state.db.clone());
    let settings = load_settings_snapshot(&settings_store, &settings_store, &state.security)
        .await
        .ok();
    let custom_encoding_len = db::get_meta(&state.db, "custom_encoding")
        .await
        .ok()
        .flatten()
        .map(|value| value.len())
        .unwrap_or(0);
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
            .unwrap_or_else(|| state.security.get_config()),
        "transcodeProfiles": settings
            .as_ref()
            .map(|settings| settings.transcode_profiles.clone())
            .unwrap_or_else(crate::media::profiles::built_in_defaults),
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
async fn execute_agent_operation(
    state: &AppState,
    record: &crate::agent_execution::OperationRecord,
) -> Result<AgentOperationApplyOutcome, String> {
    let request = record.request.plan_request();
    let pipelines = db::list_pipelines(&state.db)
        .await
        .map_err(|err| format!("failed to list pipelines: {err}"))?;
    let outputs = db::list_outputs(&state.db)
        .await
        .map_err(|err| format!("failed to list outputs: {err}"))?;
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
async fn apply_agent_add_output(
    state: &AppState,
    pipeline_id: &str,
    change: &crate::agent_plane::ProposedChange,
) -> Result<serde_json::Value, String> {
    let name = required_change_field(change.name.as_deref(), "name")?;
    let url = required_change_field(change.url.as_deref(), "url")?.trim();
    let monitoring_url = normalize_monitoring_url(change.monitoring_url.as_deref());
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
fn required_change_field<'a>(value: Option<&'a str>, field: &str) -> Result<&'a str, String> {
    value
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("change is missing required field '{field}'"))
}

#[cfg(feature = "agent-execution")]
fn validate_output_fields(
    name: &str,
    url: &str,
    monitoring_url: Option<&str>,
    config: &OutputConfig,
    desired_state: &str,
) -> Result<(), String> {
    let encoding = config.to_encoding_string();
    validate_len("name", name, MAX_NAME_LEN)?;
    validate_len("url", url, MAX_URL_LEN)?;
    validate_len("config", &encoding, MAX_ENCODING_LEN)?;
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
fn validate_len(field: &str, value: &str, max: usize) -> Result<(), String> {
    if value.len() > max {
        Err(format!("{field} exceeds maximum length of {max} bytes"))
    } else {
        Ok(())
    }
}

#[cfg(feature = "agent-execution")]
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
async fn verify_agent_operation(
    state: &AppState,
    record: &crate::agent_execution::OperationRecord,
) -> serde_json::Value {
    let pipelines = db::list_pipelines(&state.db).await.unwrap_or_default();
    let pipeline_ids: Vec<String> = pipelines
        .iter()
        .map(|pipeline| pipeline.id.clone())
        .collect();
    let outputs = db::list_outputs(&state.db).await.unwrap_or_default();
    let recording_enabled = recording_enabled_map(state, &pipeline_ids).await;
    let health = crate::api_runtime_views::health_snapshot(
        &state.engine,
        &pipeline_ids,
        &recording_enabled,
        0,
    )
    .await;
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
                        let status = runtime.and_then(|runtime| runtime["status"].as_str());
                        let input_status = health["pipelines"][pipeline_id]["input"]["status"]
                            .as_str()
                            .unwrap_or("off");
                        if status == Some("running") {
                            (true, "running")
                        } else if input_status != "on" {
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
                let status = runtime.and_then(|runtime| runtime["status"].as_str());
                let input_status = health["pipelines"][pipeline_id]["input"]["status"]
                    .as_str()
                    .unwrap_or("off");
                if output.is_some_and(|output| output.desired_state == DesiredOutputState::Running)
                    && status == Some("running")
                {
                    (true, "running")
                } else if input_status != "on" {
                    (false, "pendingInput")
                } else {
                    (false, "notRunning")
                }
            }
            "stopOutput" => {
                let status = runtime.and_then(|runtime| runtime["status"].as_str());
                if output.is_some_and(|output| output.desired_state == DesiredOutputState::Stopped)
                    && status != Some("running")
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
    let pipelines = db::list_pipelines(&state.db).await.unwrap_or_default();
    let pipeline_ids: Vec<String> = pipelines
        .iter()
        .map(|pipeline| pipeline.id.clone())
        .collect();
    let recording_enabled = recording_enabled_map(state, &pipeline_ids).await;
    let health = crate::api_runtime_views::health_snapshot(
        &state.engine,
        &pipeline_ids,
        &recording_enabled,
        0,
    )
    .await;
    alerts::derive_alerts(&health).len()
}

#[cfg(feature = "agent-plane")]
async fn agent_media_inventory(state: &AppState) -> serde_json::Value {
    let mut files = Vec::new();
    if let Ok(mut entries) = tokio::fs::read_dir(&state.media_dir).await {
        while let Ok(Some(entry)) = entries.next_entry().await {
            let name = entry.file_name().to_string_lossy().to_string();
            if (name.ends_with(".ts")
                || name.ends_with(".mkv")
                || name.ends_with(".mp4")
                || name.ends_with(".mov"))
                && let Ok(metadata) = entry.metadata().await
            {
                let modified = metadata
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .and_then(|d| chrono::DateTime::from_timestamp_millis(d.as_millis() as i64))
                    .map(|dt| dt.to_rfc3339())
                    .unwrap_or_default();

                let ingests = db::list_ingests_for_filename(&state.db, &name)
                    .await
                    .unwrap_or_default();
                let lower_name = name.to_ascii_lowercase();
                let kind = if lower_name.ends_with(".ts") || lower_name.ends_with(".mkv") {
                    "recording"
                } else {
                    "source"
                };
                files.push(serde_json::json!({
                    "name": name,
                    "size": metadata.len(),
                    "modifiedAt": modified,
                    "ingestCount": ingests.len(),
                    "kind": kind
                }));
            }
        }
    }
    serde_json::json!({
        "mediaDir": state.media_dir,
        "files": files,
    })
}

#[cfg(feature = "agent-plane")]
fn agent_desired_vs_actual(
    pipelines: &[Pipeline],
    outputs: &[crate::types::Output],
    ingests: &[Ingest],
    jobs: &[crate::types::Job],
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
            let reason = if output.desired_state == DesiredOutputState::Running
                && input_status != "on"
            {
                pending_count += 1;
                "pendingInput"
            } else if (output.desired_state == DesiredOutputState::Running && actual == "running")
                || (output.desired_state == DesiredOutputState::Stopped && actual != "running")
            {
                converged_count += 1;
                "converged"
            } else {
                drift_count += 1;
                "desiredActualMismatch"
            };
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
fn agent_diagnostics_summary(
    pipelines: &[Pipeline],
    outputs: &[crate::types::Output],
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
            "activeProbeEndpoint": format!("/api/v1/pipelines/{}/diagnostics", pipeline.id),
            "supportedProbeQueryValues": ["rtmp", "srt"],
            "includedActiveProbeResults": false,
            "reason": "The context endpoint is read-only and does not open active SSE diagnostics probes.",
            "inactiveGraphNodes": inactive_nodes,
            "findings": findings,
        }));
    }

    serde_json::json!({
        "streamingEndpointTemplate": "/api/v1/pipelines/:pipeline_id/diagnostics?probe=:probe",
        "includedActiveProbeResults": false,
        "pipelines": pipeline_reports,
    })
}

#[cfg(feature = "agent-plane")]
async fn agent_dependency_summary(
    state: &AppState,
    pipelines: &[Pipeline],
    outputs: &[crate::types::Output],
    ingests: &[Ingest],
    recording_enabled: &std::collections::HashMap<String, bool>,
    health: &serde_json::Value,
) -> serde_json::Value {
    let hls_config = crate::media::hls::HlsConfig::from_app_config(&state.engine.config);
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
            "backend": if crate::media::file_ingest::use_internal_file_ingest() { "internal" } else { "ffmpeg-subprocess" },
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
                "minSegmentSecs": hls_config.min_segment_secs,
                "segmentCapacity": hls_config.segment_capacity,
                "maxSegments": hls_config.max_segments,
            },
            "outputCount": hls_output_count,
            "pipelines": hls,
        },
        "recording": {
            "pipelines": recordings,
        },
        "fileIngest": {
            "configured": file_ingest.len(),
            "backend": if crate::media::file_ingest::use_internal_file_ingest() { "internal" } else { "ffmpeg-subprocess" },
            "ingests": file_ingest,
        },
        "ingestSecurity": {
            "config": state.security.get_config(),
            "loopbackExempt": true,
            "trackedIpRuntimeStateRedacted": true,
        }
    })
}

#[cfg(feature = "agent-plane")]
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
async fn build_agent_plan(
    state: &AppState,
    request: crate::agent_plane::PlanRequest,
) -> crate::agent_plane::PlanResponse {
    let pipelines = db::list_pipelines(&state.db).await.unwrap_or_default();
    let outputs = db::list_outputs(&state.db).await.unwrap_or_default();
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
