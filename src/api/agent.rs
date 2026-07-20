//! Authenticated HTTP edge for optional agent planning and execution features.

#[cfg(feature = "agent-plane")]
mod context;
mod operations;
mod queries;

pub use operations::{
    agent_operation_apply_handler, agent_operation_approve_handler, agent_operation_create_handler,
    agent_operation_get_handler, agent_operation_verify_handler, agent_verify_handler,
};
pub use queries::{
    agent_capabilities_handler, agent_context_handler, agent_graph_diff_preview_handler,
    agent_investigation_handler, agent_plan_handler, agent_plan_validate_handler,
};

#[cfg(any(not(feature = "agent-plane"), not(feature = "agent-execution")))]
use axum::{
    Json,
    http::StatusCode,
    response::{IntoResponse, Response},
};

#[cfg(feature = "agent-plane")]
use super::state::{AppState, recording_enabled_map};

#[cfg(any(not(feature = "agent-plane"), not(feature = "agent-execution")))]
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
fn agent_plane_unavailable() -> Response {
    feature_unavailable_response("agent-plane")
}

#[cfg(not(feature = "agent-execution"))]
fn agent_execution_unavailable() -> Response {
    feature_unavailable_response("agent-execution")
}

#[cfg(feature = "agent-plane")]
async fn agent_health_snapshot(
    state: &AppState,
    pipeline_ids: &[String],
) -> (std::collections::HashMap<String, bool>, serde_json::Value) {
    let recording_enabled = recording_enabled_map(state, pipeline_ids).await;
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
fn pipeline_input_is_on(pipeline_health: &serde_json::Value) -> bool {
    pipeline_health["input"]["status"].as_str() == Some("on")
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "agent-plane")]
    use super::pipeline_input_is_on;

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
            serde_json::json!({ "status": "running" })["status"].as_str(),
            Some("running")
        );
        assert_ne!(
            serde_json::json!({ "status": "stopped" })["status"].as_str(),
            Some("running")
        );
    }
}
