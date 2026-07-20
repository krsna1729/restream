use axum::{Json, extract::State, http::HeaderMap, response::IntoResponse};
use std::sync::Arc;

use super::super::state::{AppState, require_authenticated};

#[cfg(feature = "agent-plane")]
use super::super::telemetry::process_resource_snapshot;
#[cfg(not(feature = "agent-plane"))]
use super::agent_plane_unavailable;
#[cfg(feature = "agent-plane")]
use super::{agent_health_snapshot, context::build_agent_context};
#[cfg(feature = "agent-plane")]
use crate::api_runtime_views::{ResourceMapOptions, ResourceMapView};
#[cfg(feature = "agent-plane")]
use crate::{alerts, events};
#[cfg(feature = "agent-plane")]
use sysinfo::System;

#[cfg(feature = "agent-plane")]
const AGENT_PROCESSING_GRAPH_OUTPUT_LIMIT: usize = 50;

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
    Json(build_agent_context(&state).await).into_response()
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
    Json(request): Json<crate::agent_core::InvestigationRequest>,
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
        .is_none_or(|pid| pipelines.iter().any(|pipeline| pipeline.id == pid));
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
    let pipeline_ids = request
        .pipeline_id
        .clone()
        .map(|pid| vec![pid])
        .unwrap_or_else(|| {
            pipelines
                .iter()
                .map(|pipeline| pipeline.id.clone())
                .collect()
        });
    let (_, health) = agent_health_snapshot(&state, &pipeline_ids).await;
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
    let system = System::new_all();
    let resource_map = crate::api_runtime_views::resource_map(
        &state.engine,
        process_resource_snapshot(&system),
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
pub async fn agent_plan_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_core::PlanRequest>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    Json(build_agent_plan(&state, request).await).into_response()
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
fn agent_plan_validation_json(response: &crate::agent_plane::PlanResponse) -> serde_json::Value {
    serde_json::json!({
        "generatedAt": response.generated_at,
        "planId": response.plan_id,
        "validation": response.validation,
    })
}

#[cfg(feature = "agent-plane")]
fn agent_plan_graph_preview_json(response: &crate::agent_plane::PlanResponse) -> serde_json::Value {
    serde_json::json!({
        "generatedAt": response.generated_at,
        "planId": response.plan_id,
        "graphPreview": response.graph_preview,
        "impact": response.impact,
    })
}

#[cfg(feature = "agent-plane")]
pub async fn agent_plan_validate_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_core::PlanRequest>,
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
pub async fn agent_graph_diff_preview_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    Json(request): Json<crate::agent_core::PlanRequest>,
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

#[cfg(feature = "agent-plane")]
pub(super) async fn build_agent_plan(
    state: &AppState,
    request: crate::agent_core::PlanRequest,
) -> crate::agent_plane::PlanResponse {
    let catalog = state.agent_service.load_pipeline_output_catalog().await;
    let pipelines = catalog.pipelines;
    let outputs = catalog.outputs;
    let current_graph = if let Some(pid) = request.pipeline_id.as_deref()
        && pipelines.iter().any(|pipeline| pipeline.id == pid)
    {
        Some(crate::api_runtime_views::processing_graph(&state.engine, pid, &outputs).await)
    } else {
        None
    };
    crate::agent_plane::plan_response(request, &pipelines, &outputs, current_graph.as_ref())
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "agent-plane")]
    use super::{agent_plan_graph_preview_json, agent_plan_validation_json};

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
}
