//! Alert HTTP handlers expose derived operator alerts from the runtime health
//! snapshot. This module stays at the transport boundary so request auth,
//! snapshot shaping, and alert-tracker state remain easy to audit together.

use axum::{
    Json,
    extract::State,
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::alerts;

use super::state::{AppState, recording_enabled_map, require_authenticated};

async fn dashboard_alert_snapshot(state: &AppState) -> serde_json::Value {
    let pipeline_ids = state
        .pipeline_service
        .list_pipeline_ids()
        .await
        .unwrap_or_default();
    let recording_enabled = recording_enabled_map(state, &pipeline_ids).await;

    // Alerts should reflect the current dashboard view immediately, so they do
    // not preserve the disconnect grace window used by some operator snapshots.
    crate::api_runtime_views::health_snapshot(&state.engine, &pipeline_ids, &recording_enabled, 0)
        .await
}

fn snapshot_generated_at(snapshot: &serde_json::Value) -> String {
    snapshot["generatedAt"].as_str().unwrap_or("").to_string()
}

fn alerts_response(snapshot: &serde_json::Value, alerts: Vec<crate::alerts::Alert>) -> Response {
    Json(serde_json::json!({
        "generatedAt": snapshot_generated_at(snapshot),
        "alerts": alerts,
    }))
    .into_response()
}

pub async fn aggregate_alerts_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    // Alerts are derived from the same health snapshot shape the dashboard uses
    // so transport-only alert logic does not need to understand engine internals.
    let snapshot = dashboard_alert_snapshot(&state).await;
    let mut alert_list = alerts::derive_alerts(&snapshot);
    state.alert_tracker.track(&mut alert_list);
    alerts_response(&snapshot, alert_list)
}

#[cfg(test)]
mod tests {
    use super::{alerts_response, snapshot_generated_at};
    use axum::http::StatusCode;

    #[test]
    fn snapshot_generated_at_falls_back_to_empty_string() {
        assert_eq!(snapshot_generated_at(&serde_json::json!({})), "");
        assert_eq!(
            snapshot_generated_at(&serde_json::json!({"generatedAt": "2026-07-14T00:00:00Z"})),
            "2026-07-14T00:00:00Z"
        );
    }

    #[test]
    fn alerts_response_uses_ok_status() {
        let response = alerts_response(&serde_json::json!({}), Vec::new());

        assert_eq!(response.status(), StatusCode::OK);
    }
}
