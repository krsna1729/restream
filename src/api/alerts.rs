//! Alert HTTP handlers expose derived operator alerts from the runtime health
//! snapshot. This module stays at the transport boundary so request auth,
//! snapshot shaping, and alert-tracker state remain easy to audit together.

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use crate::alerts;

use super::state::{AppState, recording_enabled_map, require_authenticated};

// Alerts are always derived from the dashboard health view so the alert list
// reflects the same pipeline set and desired-recording state operators see.
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

// Keep the generated timestamp extraction in one place so alert responses use
// the same snapshot timestamp even if the JSON shape grows later.
fn snapshot_generated_at(snapshot: &serde_json::Value) -> String {
    snapshot["generatedAt"].as_str().unwrap_or("").to_string()
}

// Transport shaping for the alert payload stays local to this module so the
// tracker and derivation code do not need to know the HTTP response schema.
fn alerts_response(
    snapshot: &serde_json::Value,
    alerts: Vec<crate::alerts::Alert>,
    resolved: Option<crate::alerts::ResolvedWindow>,
) -> Response {
    let mut body = serde_json::json!({
        "generatedAt": snapshot_generated_at(snapshot),
        "alerts": alerts,
    });
    // History is opt-in: a reader of the open set does not pay for it.
    if let Some(resolved) = resolved {
        body["resolved"] = serde_json::json!(resolved.alerts);
        body["resolvedCompleteFrom"] = serde_json::json!(resolved.complete_from);
    }
    Json(body).into_response()
}

/// The open alerts now, stamped by the tracker. Alerts are derived from the
/// same health snapshot shape the dashboard uses, so transport-only alert
/// logic does not need to understand engine internals.
pub async fn current_alerts(state: &AppState) -> (serde_json::Value, Vec<crate::alerts::Alert>) {
    let snapshot = dashboard_alert_snapshot(state).await;
    let mut alert_list = alerts::derive_alerts(&snapshot);
    state.alert_tracker.track(&mut alert_list);
    (snapshot, alert_list)
}

/// How often the notifier evaluates alerts.
pub const NOTIFY_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);

/// Evaluates alerts every `interval` whether or not a dashboard is open,
/// and tells every channel and browser that wants each confirmed change
/// (`application::notify`). Deliveries run off the loop, so a slow
/// receiver never delays the next evaluation. It also keeps the alert
/// tracker's history current when nobody is polling `/alerts`.
pub async fn run_alert_notifier(
    state: Arc<AppState>,
    interval: std::time::Duration,
    shutdown: tokio_util::sync::CancellationToken,
) {
    let mut transitions = crate::application::notify::AlertTransitions::default();
    let mut ticks = tokio::time::interval(interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            _ = ticks.tick() => {}
        }
        let (_, open) = current_alerts(&state).await;
        let briefs: Vec<_> = open
            .iter()
            .map(crate::application::notify::AlertBrief::from)
            .collect();
        let events = transitions.observe(&briefs, chrono::Utc::now().timestamp_millis());
        for event in events {
            let notify = state.notify_service.clone();
            tokio::spawn(async move {
                notify.dispatch(&event).await;
            });
        }
    }
}

#[derive(Debug, serde::Deserialize)]
pub struct AlertsQuery {
    /// RFC 3339: only resolved alerts that closed at or after it.
    since: Option<chrono::DateTime<chrono::Utc>>,
}

/// Returns the authenticated dashboard alert feed derived from the current
/// health snapshot and then passed through the alert tracker.
pub async fn aggregate_alerts_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    query: Result<Query<AlertsQuery>, axum::extract::rejection::QueryRejection>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    // Validated after authentication: a caller without a session learns
    // nothing about the query, only that it needs one.
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => return rejection.into_response(),
    };

    let (snapshot, alert_list) = current_alerts(&state).await;
    let resolved = query
        .since
        .map(|since| state.alert_tracker.resolved_since(since));
    alerts_response(&snapshot, alert_list, resolved)
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
        let response = alerts_response(&serde_json::json!({}), Vec::new(), None);

        assert_eq!(response.status(), StatusCode::OK);
    }
}
