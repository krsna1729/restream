use axum::{Json, extract::State, http::HeaderMap, response::IntoResponse};
use std::sync::Arc;

use crate::alerts;

use super::state::{AppState, recording_enabled_map, require_authenticated};

pub async fn aggregate_alerts_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let pipeline_ids = state
        .pipeline_service
        .list_pipeline_ids()
        .await
        .unwrap_or_default();
    let recording_enabled = recording_enabled_map(&state, &pipeline_ids).await;
    let snapshot = crate::api_runtime_views::health_snapshot(
        &state.engine,
        &pipeline_ids,
        &recording_enabled,
        0,
    )
    .await;
    let generated_at = snapshot["generatedAt"].as_str().unwrap_or("").to_string();
    let mut alert_list = alerts::derive_alerts(&snapshot);
    state.alert_tracker.track(&mut alert_list);
    Json(serde_json::json!({
        "generatedAt": generated_at,
        "alerts": alert_list,
    }))
    .into_response()
}
