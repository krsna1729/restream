//! `GET /api/v1/agent/incidents/explanation?alertId=…[&firstSeen=…]`: why an
//! incident happened, from the pipeline's flight record over the incident
//! (`application::incident_explain`), and the agent-plane plan for the fix
//! it proposes. Read-only: the plan is judged, nothing is created; an
//! operator turns it into an operation and approves each change.

use axum::{extract::State, http::HeaderMap, response::Response};
use std::sync::Arc;

use super::super::state::{AppState, require_authenticated};
#[cfg(not(feature = "agent-plane"))]
use super::agent_plane_unavailable;
#[cfg(feature = "agent-plane")]
use super::queries::build_agent_plan;
#[cfg(feature = "agent-plane")]
use axum::{Json, extract::Query, http::StatusCode, response::IntoResponse};

#[cfg(feature = "agent-plane")]
#[derive(Debug, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExplainQuery {
    alert_id: String,
    /// One episode of a recurring alert; without it the open one, else the
    /// latest resolved.
    #[serde(default)]
    first_seen: Option<String>,
}

#[cfg(feature = "agent-plane")]
pub async fn agent_incident_explanation_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    query: Result<Query<ExplainQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    use crate::application::flight_recorder::{FLIGHT_MAX_POINTS, trace_window};
    use crate::application::incident_explain::{EXPLAIN_LEAD_MS, explain};

    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => return rejection.into_response(),
    };
    let now_ms = epoch_ms(chrono::Utc::now());
    let episode = |alert: &restream_health::alerts::Alert| {
        alert.id == query.alert_id
            && query
                .first_seen
                .as_ref()
                .is_none_or(|first| alert.first_seen.as_ref() == Some(first))
    };
    let open = state
        .alert_tracker
        .latest()
        .unwrap_or_default()
        .alerts
        .into_iter()
        .find(|alert| episode(alert))
        .map(|alert| (alert, now_ms));
    let incident = open.or_else(|| {
        state
            .alert_tracker
            .resolved_since(chrono::DateTime::UNIX_EPOCH)
            .alerts
            .into_iter()
            .rev()
            .find(|resolved| episode(&resolved.alert))
            .map(|resolved| (resolved.alert, epoch_ms(resolved.resolved_at)))
    });
    let Some((alert, to_ms)) = incident else {
        return (
            StatusCode::NOT_FOUND,
            Json(serde_json::json!({
                "error": "No open or recently resolved alert has this id",
                "code": "incidentNotFound",
            })),
        )
            .into_response();
    };
    let from_ms = alert
        .first_seen
        .as_deref()
        .or(Some(alert.generated_at.as_str()))
        .and_then(|at| chrono::DateTime::parse_from_rfc3339(at).ok())
        .map_or(to_ms, |at| epoch_ms(at.with_timezone(&chrono::Utc)));

    let samples = match alert.pipeline_id.as_deref() {
        Some(pipeline_id) => trace_window(
            &state.flight_recorder,
            &state.db,
            pipeline_id,
            from_ms.saturating_sub(EXPLAIN_LEAD_MS),
            to_ms.saturating_add(1),
            FLIGHT_MAX_POINTS,
        )
        .await
        .map(|trace| trace.samples)
        .unwrap_or_default(),
        None => Vec::new(),
    };
    let explanation = explain(&alert, from_ms, to_ms, &samples);
    let plan = match explanation.proposal.clone() {
        Some(proposal) => Some(build_agent_plan(&state, proposal).await),
        None => None,
    };
    Json(serde_json::json!({
        "alert": alert,
        "explanation": explanation,
        "plan": plan,
    }))
    .into_response()
}

#[cfg(feature = "agent-plane")]
fn epoch_ms(at: chrono::DateTime<chrono::Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

#[cfg(not(feature = "agent-plane"))]
pub async fn agent_incident_explanation_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    agent_plane_unavailable()
}
