use axum::{
    Json,
    extract::{Path, State},
    http::HeaderMap,
    response::IntoResponse,
};
use std::sync::Arc;

use super::super::state::{AppState, require_authenticated};
#[cfg(feature = "agent-execution")]
use axum::http::StatusCode;

#[cfg(feature = "agent-execution")]
use super::super::{
    outputs::{
        CUSTOM_OUTPUT_ENCODING_ERROR, MONITORING_URL_SCHEME_ERROR, OUTPUT_URL_SCHEME_ERROR,
        is_supported_monitoring_url, is_supported_output_url, normalize_monitoring_url,
    },
    state::{MAX_NAME_LEN, MAX_OUTPUT_CONFIG_LEN, MAX_URL_LEN, to_hex},
};
#[cfg(not(feature = "agent-execution"))]
use super::agent_execution_unavailable;
#[cfg(feature = "agent-execution")]
use super::{agent_health_snapshot, pipeline_input_is_on, queries::build_agent_plan};
#[cfg(feature = "agent-execution")]
use crate::{
    alerts,
    application::services::agent_service::{AgentOutputMutation, AgentOutputMutationOutcome},
    domain::{
        output_spec::{OutputConfig, ProtocolCapabilities},
        state::DesiredOutputState,
    },
};

#[cfg(feature = "agent-execution")]
pub async fn agent_operation_create_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_core::OperationCreateRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    let plan = build_agent_plan(&state, request.plan_request()).await;
    let pre_alert_count = current_agent_alert_count(&state).await;
    let result = match state.agent_execution.create(request, plan, pre_alert_count) {
        Ok(result) => result,
        Err(err) => return operation_store_error(err),
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
    Json(request): Json<crate::agent_core::ApprovalRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    match state.agent_execution.approve(&operation_id, request) {
        Ok(record) => Json(crate::agent_execution::public_record(&record)).into_response(),
        Err(err) => operation_store_error(err),
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
        Err(err) => return operation_store_error(err),
    };
    match execute_operation(&state, &record).await {
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
    verify_operation_by_id(&state, &operation_id).await
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
    Json(request): Json<crate::agent_core::VerifyRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    verify_operation_by_id(&state, &request.operation_id).await
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

#[cfg(feature = "agent-execution")]
struct OperationApplyOutcome {
    state_transitions: Vec<serde_json::Value>,
    progress_snapshots: Vec<serde_json::Value>,
    execution_result: serde_json::Value,
}

#[cfg(feature = "agent-execution")]
async fn execute_operation(
    state: &AppState,
    record: &crate::agent_execution::OperationRecord,
) -> Result<OperationApplyOutcome, String> {
    let request = record.request.plan_request();
    let catalog = state
        .agent_service
        .try_load_pipeline_output_catalog()
        .await?;
    let validation =
        crate::agent_plane::validate_plan(&request, &catalog.pipelines, &catalog.outputs);
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
    for (index, change) in request.proposed_changes.iter().enumerate() {
        let pipeline_id = change
            .pipeline_id
            .as_deref()
            .or(request.pipeline_id.as_deref())
            .ok_or_else(|| "change is missing pipelineId".to_string())?;
        let result = apply_change(state, pipeline_id, change).await?;
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
            "completed": index + 1,
            "total": total,
            "currentChange": change.kind,
            "pipelineId": pipeline_id,
            "outputId": result["outputId"],
        }));
        change_results.push(result);
    }

    Ok(OperationApplyOutcome {
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
async fn apply_change(
    state: &AppState,
    pipeline_id: &str,
    change: &crate::agent_core::ProposedChange,
) -> Result<serde_json::Value, String> {
    let mutation = match change.kind.as_str() {
        "addOutput" => prepare_add_output(change)?,
        "updateOutput" => prepare_update_output(state, pipeline_id, change).await?,
        "removeOutput" => AgentOutputMutation::Remove {
            output_id: required_change_field(change.output_id.as_deref(), "outputId")?.to_string(),
        },
        "startOutput" => AgentOutputMutation::SetDesiredState {
            output_id: required_change_field(change.output_id.as_deref(), "outputId")?.to_string(),
            desired_state: DesiredOutputState::Running,
        },
        "stopOutput" => AgentOutputMutation::SetDesiredState {
            output_id: required_change_field(change.output_id.as_deref(), "outputId")?.to_string(),
            desired_state: DesiredOutputState::Stopped,
        },
        other => return Err(format!("unsupported change kind '{other}'")),
    };
    let outcome = state
        .agent_service
        .apply_output_mutation(&state.engine, pipeline_id, mutation)
        .await?;
    Ok(mutation_json(&change.kind, pipeline_id, outcome))
}

#[cfg(feature = "agent-execution")]
fn prepare_add_output(
    change: &crate::agent_core::ProposedChange,
) -> Result<AgentOutputMutation, String> {
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
    Ok(AgentOutputMutation::Create {
        output_id: change
            .output_id
            .clone()
            .unwrap_or_else(|| format!("output_agent_{}", to_hex(&rand::random::<[u8; 8]>()))),
        name: name.trim().to_string(),
        url: url.to_string(),
        monitoring_url,
        desired_state: DesiredOutputState::from(desired_state),
        config: config.clone(),
    })
}

#[cfg(feature = "agent-execution")]
async fn prepare_update_output(
    state: &AppState,
    pipeline_id: &str,
    change: &crate::agent_core::ProposedChange,
) -> Result<AgentOutputMutation, String> {
    let output_id = required_change_field(change.output_id.as_deref(), "outputId")?;
    let existing = state
        .agent_service
        .load_output_for_mutation(pipeline_id, output_id)
        .await?;
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
    Ok(AgentOutputMutation::Update {
        output_id: output_id.to_string(),
        name: name.trim().to_string(),
        url: url.to_string(),
        monitoring_url,
        desired_state: DesiredOutputState::from(desired_state),
        config: config.clone(),
    })
}

#[cfg(feature = "agent-execution")]
fn mutation_json(
    kind: &str,
    pipeline_id: &str,
    outcome: AgentOutputMutationOutcome,
) -> serde_json::Value {
    match outcome {
        AgentOutputMutationOutcome::Created(output) => {
            let output_id = output.id.clone();
            serde_json::json!({
                "kind": kind,
                "pipelineId": pipeline_id,
                "outputId": output_id,
                "status": "created",
                "from": null,
                "to": output,
            })
        }
        AgentOutputMutationOutcome::Updated { previous, current } => {
            let output_id = current.id.clone();
            serde_json::json!({
                "kind": kind,
                "pipelineId": pipeline_id,
                "outputId": output_id,
                "status": "updated",
                "from": previous,
                "to": current,
            })
        }
        AgentOutputMutationOutcome::Removed(previous) => {
            let output_id = previous.id.clone();
            serde_json::json!({
                "kind": kind,
                "pipelineId": pipeline_id,
                "outputId": output_id,
                "status": "deleted",
                "from": previous,
                "to": null,
            })
        }
        AgentOutputMutationOutcome::DesiredStateUpdated { previous, current } => {
            let output_id = current.id.clone();
            serde_json::json!({
                "kind": kind,
                "pipelineId": pipeline_id,
                "outputId": output_id,
                "status": "desiredStateUpdated",
                "from": previous,
                "to": current,
            })
        }
    }
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
    config
        .validate_capabilities(ProtocolCapabilities::from_output(url, config))
        .map_err(|error| error.message().to_string())?;
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
fn output_runtime_is_running(runtime: &serde_json::Value) -> bool {
    runtime["status"].as_str() == Some("running")
}

#[cfg(feature = "agent-execution")]
async fn verify_operation_by_id(state: &AppState, operation_id: &str) -> axum::response::Response {
    let record = match state.agent_execution.get(operation_id) {
        Some(record) => record,
        None => return (StatusCode::NOT_FOUND, "Operation not found").into_response(),
    };
    let verification = verify_operation(state, &record).await;
    match state
        .agent_execution
        .complete_verify(operation_id, verification)
    {
        Some(record) => Json(crate::agent_execution::public_record(&record)).into_response(),
        None => (StatusCode::NOT_FOUND, "Operation not found").into_response(),
    }
}

#[cfg(feature = "agent-execution")]
async fn verify_operation(
    state: &AppState,
    record: &crate::agent_execution::OperationRecord,
) -> serde_json::Value {
    let catalog = state.agent_service.load_pipeline_output_catalog().await;
    let pipelines = catalog.pipelines;
    let pipeline_ids = pipelines
        .iter()
        .map(|pipeline| pipeline.id.clone())
        .collect::<Vec<_>>();
    let outputs = catalog.outputs;
    let (_, health) = agent_health_snapshot(state, &pipeline_ids).await;
    let alerts = alerts::derive_alerts(&health);
    let mut checks = Vec::new();
    let mut success = true;

    for change in &record.request.proposed_changes {
        let pipeline_id = change
            .pipeline_id
            .as_deref()
            .or(record.request.pipeline_id.as_deref())
            .unwrap_or_default();
        let output_id = change_output_id(record, change);
        let output = output_id.as_deref().and_then(|output_id| {
            outputs
                .iter()
                .find(|output| output.pipeline_id == pipeline_id && output.id == output_id)
        });
        let runtime = output_id
            .as_deref()
            .map(|output_id| &health["pipelines"][pipeline_id]["outputs"][output_id]);
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
                        if runtime.and_then(|runtime| runtime["status"].as_str()) != Some("running")
                        {
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
fn change_output_id(
    record: &crate::agent_execution::OperationRecord,
    change: &crate::agent_core::ProposedChange,
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
    let pipeline_ids = catalog
        .pipelines
        .iter()
        .map(|pipeline| pipeline.id.clone())
        .collect::<Vec<_>>();
    let (_, health) = agent_health_snapshot(state, &pipeline_ids).await;
    alerts::derive_alerts(&health).len()
}

#[cfg(feature = "agent-execution")]
fn operation_store_error(
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
    #[cfg(feature = "agent-execution")]
    use super::validate_output_fields;
    #[cfg(feature = "agent-execution")]
    use crate::domain::output_spec::{OutputConfig, OutputVideoCodec};

    #[cfg(feature = "agent-execution")]
    #[test]
    fn agent_execution_rejects_unsupported_output_codec_for_legacy_rtmp() {
        let config = OutputConfig::preset("720p").with_video_codec(OutputVideoCodec::Hevc);
        let error = validate_output_fields(
            "Legacy RTMP",
            "rtmp://example/live/key",
            None,
            &config,
            "running",
        )
        .expect_err("legacy RTMP must reject explicit H.265");
        assert_eq!(
            error,
            "Output video codec is not supported by the selected protocol mode"
        );
    }
}
