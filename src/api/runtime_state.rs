//! `/api/v1/state` and `/api/v1/state/stream`.
//!
//! The stream sends `snapshot` (a whole `RuntimeState`) or `delta` (a
//! `StateDelta`) events, each with `id: <epoch>.<revision>`. A client
//! applies deltas in order to its last snapshot. It reconnects with that id
//! as `?since=` or `Last-Event-ID`; when the gap is no longer replayable,
//! the id is from another server run, or the client falls behind the live
//! stream, it receives a fresh `snapshot` instead.

use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Json};
use serde::Deserialize;
use std::sync::Arc;

use super::state::{AppState, require_authenticated};

pub async fn v1_state_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    Json(state.refresh_runtime_snapshot().await).into_response()
}

#[derive(Deserialize)]
pub struct StateStreamQuery {
    since: Option<String>,
}

pub async fn v1_state_stream_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<StateStreamQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    let since = headers
        .get("last-event-id")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
        .or(query.since);

    let (tx, rx) = tokio::sync::mpsc::channel::<String>(64);
    state.spawn_runtime_state_stream(since, tx);

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = axum::body::Body::from_stream(futures_util::StreamExt::map(stream, |frame| {
        Ok::<_, std::convert::Infallible>(frame)
    }));
    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        body,
    )
        .into_response()
}
