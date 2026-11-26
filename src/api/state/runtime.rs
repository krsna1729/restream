//! Runtime-state queries and the dashboard event-stream pump.

use std::sync::Arc;

use super::AppState;
use crate::application::runtime_state::hub::Since;
use crate::application::runtime_state::{RuntimeState, StateDelta};

impl AppState {
    /// The runtime-state hub used by the projector and stream tasks.
    pub(crate) fn runtime_state_handle(
        &self,
    ) -> Arc<crate::application::runtime_state::hub::StateHub> {
        Arc::clone(&self.runtime_state)
    }

    /// Returns the current runtime snapshot without refreshing it.
    pub(crate) async fn runtime_snapshot(&self) -> RuntimeState {
        self.runtime_state.snapshot().await
    }

    /// Returns the current runtime snapshot after bringing it up to date.
    pub(crate) async fn refresh_runtime_snapshot(&self) -> RuntimeState {
        self.runtime_state
            .refresh_on_demand(&self.db, &self.engine)
            .await;
        self.runtime_state.snapshot().await
    }

    /// Marks the runtime state as changed.
    pub(crate) fn mark_runtime_changed(&self) {
        self.runtime_state.mark_changed();
    }

    /// Spawns the SSE pump for one runtime-state client.
    pub(crate) fn spawn_runtime_state_stream(
        &self,
        since: Option<String>,
        tx: tokio::sync::mpsc::Sender<String>,
    ) {
        let hub = self.runtime_state.clone();
        // Subscribe before reading history, so no delta falls between them.
        let mut live = hub.subscribe();
        let (pool, engine) = (self.db.clone(), self.engine.clone());

        tokio::spawn(async move {
            // With no client the projector is idle, so the held state may be
            // old: bring it current first (still after subscribing). Connected
            // now, the projector keeps it so.
            hub.refresh_on_demand(&pool, &engine).await;
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
                        // A named event, not an SSE comment: the client sees it
                        // and can tell a quiet stream from a dead connection.
                        if tx.send("event: heartbeat\ndata: {}\n\n".to_string()).await.is_err() {
                            return;
                        }
                    }
                    // Release the receiver (and the projector's cadence) once
                    // hyper drops the response body: at once when the client
                    // goes, since only HLS upload connections are served with
                    // half-close (`api::http_server`).
                    () = tx.closed() => return,
                    // Shutdown: end the stream so the HTTP drain is not held
                    // open by a response that never completes on its own.
                    () = hub.streams_closed() => return,
                }
            }
        });
    }

    /// Closes every runtime-state stream; used by shutdown tests.
    #[doc(hidden)]
    pub fn close_runtime_state_streams(&self) {
        self.runtime_state.close_streams();
    }
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
