//! `/api/v1/state` and `/api/v1/state/stream` (Q-042).
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
use crate::application::runtime_state::hub::Since;
use crate::application::runtime_state::{RuntimeState, StateDelta};

pub async fn v1_state_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    state.runtime_state.refresh(&state.db, &state.engine).await;
    Json(state.runtime_state.snapshot().await).into_response()
}

#[derive(Deserialize)]
pub struct StateStreamQuery {
    since: Option<String>,
}

fn snapshot_frame(snapshot: &RuntimeState) -> String {
    format!(
        "id: {}.{}\nevent: snapshot\ndata: {}\n\n",
        snapshot.epoch,
        snapshot.revision,
        serde_json::to_string(snapshot).unwrap_or_default()
    )
}

fn delta_frame(delta: &StateDelta) -> String {
    format!(
        "id: {}.{}\nevent: delta\ndata: {}\n\n",
        delta.epoch,
        delta.revision,
        serde_json::to_string(delta).unwrap_or_default()
    )
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

    let hub = state.runtime_state.clone();
    // Subscribe before reading history, so no delta falls between them.
    let mut live = hub.subscribe();
    let (tx, rx) = tokio::sync::mpsc::channel::<String>(64);
    let (pool, engine) = (state.db.clone(), state.engine.clone());

    tokio::spawn(async move {
        // With no client the projector is idle, so the held state may be
        // old: bring it current first (still after subscribing). Connected
        // now, the projector keeps it so.
        hub.refresh(&pool, &engine).await;
        let mut sent_through = match since.as_deref() {
            Some(token) => match hub.since(token).await {
                Since::Deltas(deltas) => {
                    let mut through = token
                        .rsplit_once('.')
                        .and_then(|(_, revision)| revision.parse().ok())
                        .unwrap_or(0);
                    for delta in deltas {
                        through = delta.revision;
                        if tx.send(delta_frame(&delta)).await.is_err() {
                            return;
                        }
                    }
                    through
                }
                Since::Resync => send_snapshot(&hub, &tx).await,
            },
            None => send_snapshot(&hub, &tx).await,
        };
        if sent_through == u64::MAX {
            return;
        }

        let mut heartbeat = tokio::time::interval(std::time::Duration::from_secs(20));
        heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                delta = live.recv() => match delta {
                    Ok(delta) => {
                        if delta.revision <= sent_through {
                            continue;
                        }
                        sent_through = delta.revision;
                        if tx.send(delta_frame(&delta)).await.is_err() {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                        // Missed deltas: start the client over from now.
                        sent_through = send_snapshot(&hub, &tx).await;
                        if sent_through == u64::MAX {
                            return;
                        }
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                },
                _ = heartbeat.tick() => {
                    if tx.send(": ping\n\n".to_string()).await.is_err() {
                        return;
                    }
                }
                // Release the receiver (and the projector's cadence) as soon
                // as the client goes, not at the next send.
                () = tx.closed() => return,
            }
        }
    });

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

/// Sends the current snapshot; returns its revision, or `u64::MAX` when the
/// client is gone.
async fn send_snapshot(
    hub: &crate::application::runtime_state::hub::StateHub,
    tx: &tokio::sync::mpsc::Sender<String>,
) -> u64 {
    let snapshot = hub.snapshot().await;
    match tx.send(snapshot_frame(&snapshot)).await {
        Ok(()) => snapshot.revision,
        Err(_) => u64::MAX,
    }
}
