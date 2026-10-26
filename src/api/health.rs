//! Health HTTP handlers build runtime snapshots for dashboard and operator
//! views. They stay close to the transport layer because they choose which
//! pipeline set should be exposed for each request.

use axum::{Json, extract::State, http::HeaderMap, response::IntoResponse};
use std::sync::Arc;

use super::error::ApiError;
use super::state::{AppState, recording_enabled_map, require_authenticated};

/// Builds the full health snapshot for one caller-selected pipeline set,
/// with recording state derived alongside so the two cannot go stale apart.
pub async fn build_health_snapshot_for_pipeline_ids(
    state: &AppState,
    pipeline_ids: &[String],
) -> serde_json::Value {
    let recording_enabled = recording_enabled_map(state, pipeline_ids).await;
    crate::api_runtime_views::health_snapshot(
        &state.engine,
        pipeline_ids,
        &recording_enabled,
        state.ingest_disconnect_grace_ms,
    )
    .await
}

/// The full health view for one caller-selected pipeline set, ready to
/// serialize: no JSON tree is built, which is what the HTTP response needs.
pub async fn build_health_view_for_pipeline_ids(
    state: &AppState,
    pipeline_ids: &[String],
) -> impl serde::Serialize + use<> {
    let recording_enabled = recording_enabled_map(state, pipeline_ids).await;
    crate::api_runtime_views::health_snapshot_view(
        &state.engine,
        pipeline_ids,
        &recording_enabled,
        state.ingest_disconnect_grace_ms,
    )
    .await
}

pub async fn list_dashboard_runtime_pipeline_ids(
    state: &AppState,
) -> Result<Vec<String>, ApiError> {
    // Keep pipeline-store failures on an ApiError boundary so handlers decide
    // whether they surface JSON, plain status codes, or merged telemetry views.
    state
        .pipeline_service
        .list_pipeline_ids()
        .await
        .map_err(|err| ApiError::internal(format!("list dashboard pipeline ids: {err}")))
}

/// Returns the authenticated dashboard health snapshot. It is serialized
/// straight from its typed form; at hundreds of outputs a JSON tree in between
/// cost more than the rest of the request.
pub async fn v1_engine_health_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    let pipeline_ids = match list_dashboard_runtime_pipeline_ids(&state).await {
        Ok(pipeline_ids) => pipeline_ids,
        Err(error) => return error.into_response(),
    };
    Json(build_health_view_for_pipeline_ids(&state, &pipeline_ids).await).into_response()
}

/// Lightweight process health probe used by infrastructure and liveness checks.
pub async fn healthz_get_handler() -> impl IntoResponse {
    Json(serde_json::json!({ "status": "ok" }))
}
