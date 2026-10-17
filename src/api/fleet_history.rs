//! `GET /api/v1/fleet/history`: the fleet's sampled signals
//! ([`crate::application::fleet_history`]).

use axum::{
    Json,
    extract::{Query, State},
    http::HeaderMap,
    response::{IntoResponse, Response},
};
use std::sync::Arc;

use super::state::{AppState, require_authenticated};

#[derive(Debug, serde::Deserialize)]
pub struct FleetHistoryQuery {
    /// RFC 3339: only samples taken at or after it; every kept one without.
    since: Option<chrono::DateTime<chrono::Utc>>,
    /// RFC 3339: only samples taken at or before it; up to now without.
    until: Option<chrono::DateTime<chrono::Utc>>,
}

/// Epoch ms, before 1970 read as 0.
fn epoch_ms(at: chrono::DateTime<chrono::Utc>) -> u64 {
    u64::try_from(at.timestamp_millis()).unwrap_or(0)
}

pub async fn v1_fleet_history_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    query: Result<Query<FleetHistoryQuery>, axum::extract::rejection::QueryRejection>,
) -> Response {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    // Validated after authentication: a caller without a session learns
    // nothing about the query, only that it needs one.
    let Query(query) = match query {
        Ok(query) => query,
        Err(rejection) => return rejection.into_response(),
    };
    let since_ms = query.since.map_or(0, epoch_ms);
    let until_ms = query.until.map_or(u64::MAX, epoch_ms);
    Json(state.fleet_history.window(since_ms, until_ms)).into_response()
}
