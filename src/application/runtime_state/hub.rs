//! Holds the current projection under a revision and publishes deltas.
//!
//! Change points mark the hub changed ([`StateHub::mark_changed`]): every
//! engine lifecycle event and every successful mutating request. The
//! projector ([`run_projector`]) rebuilds on a change and, while a stream
//! client is connected, once per interval for state no event reports (flow,
//! stalls, retry countdowns). With no client connected nothing runs between
//! changes; `GET /api/v1/state` refreshes on demand.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::{Mutex, Notify, broadcast};
use tokio_util::sync::CancellationToken;

use super::{Projection, RuntimeState, StateDelta, diff, project};
use crate::media::engine::MediaEngine;

/// Deltas kept for clients that reconnect with `since`; older gaps resync
/// from a snapshot.
pub const DELTA_HISTORY: usize = 256;

/// Minimum gap between rebuilds, so an event burst costs one projection.
const MIN_REBUILD_GAP: Duration = Duration::from_millis(200);

/// Outcome of asking for the deltas after a revision.
#[derive(Debug, PartialEq)]
pub enum Since {
    /// Every delta after the revision, in order (possibly none).
    Deltas(Vec<Arc<StateDelta>>),
    /// The gap is no longer in history (or the revision is unknown): start
    /// over from a snapshot.
    Resync,
}

struct Current {
    revision: u64,
    projection: Projection,
    history: VecDeque<Arc<StateDelta>>,
}

pub struct StateHub {
    current: Mutex<Current>,
    deltas: broadcast::Sender<Arc<StateDelta>>,
    changed: Notify,
}

impl Default for StateHub {
    fn default() -> Self {
        Self::new()
    }
}

impl StateHub {
    pub fn new() -> Self {
        Self {
            current: Mutex::new(Current {
                revision: 0,
                projection: Projection::default(),
                history: VecDeque::with_capacity(DELTA_HISTORY),
            }),
            deltas: broadcast::channel(DELTA_HISTORY).0,
            changed: Notify::new(),
        }
    }

    /// Something the projection reads may have changed.
    pub fn mark_changed(&self) {
        self.changed.notify_one();
    }

    /// Live deltas. Subscribe before reading [`Self::since`] or
    /// [`Self::snapshot`], then skip deltas at or below what was sent.
    pub fn subscribe(&self) -> broadcast::Receiver<Arc<StateDelta>> {
        self.deltas.subscribe()
    }

    /// Installs `projection` as current. Returns the published delta, or
    /// `None` when nothing changed (the revision stays).
    pub async fn publish(&self, projection: Projection) -> Option<Arc<StateDelta>> {
        let mut current = self.current.lock().await;
        let mut delta = diff(&current.projection, &projection);
        if delta.is_empty() {
            return None;
        }
        current.revision += 1;
        delta.revision = current.revision;
        current.projection = projection;
        let delta = Arc::new(delta);
        if current.history.len() == DELTA_HISTORY {
            current.history.pop_front();
        }
        current.history.push_back(delta.clone());
        // Sent under the lock, so deltas reach subscribers in revision order.
        let _ = self.deltas.send(delta.clone());
        Some(delta)
    }

    pub async fn snapshot(&self) -> RuntimeState {
        let current = self.current.lock().await;
        RuntimeState {
            revision: current.revision,
            pipelines: current.projection.pipelines.values().cloned().collect(),
            outputs: current.projection.outputs.values().cloned().collect(),
        }
    }

    /// The deltas after `revision`.
    pub async fn since(&self, revision: u64) -> Since {
        let current = self.current.lock().await;
        if revision > current.revision {
            // From another process lifetime: revisions restart at 0.
            return Since::Resync;
        }
        let oldest_kept = current
            .history
            .front()
            .map_or(current.revision + 1, |delta| delta.revision);
        if revision + 1 < oldest_kept {
            return Since::Resync;
        }
        Since::Deltas(
            current
                .history
                .iter()
                .filter(|delta| delta.revision > revision)
                .cloned()
                .collect(),
        )
    }

    /// Rebuilds from the rows and the engine and publishes the change.
    pub async fn refresh(&self, pool: &SqlitePool, engine: &MediaEngine) {
        let pipelines = match crate::db::list_pipelines(pool).await {
            Ok(rows) => rows,
            Err(error) => {
                tracing::warn!(err = %error, "DB error reading pipelines for runtime state");
                return;
            }
        };
        let outputs = match crate::db::list_outputs(pool).await {
            Ok(rows) => rows
                .into_iter()
                .map(crate::application::outputs::from_record)
                .collect::<Vec<_>>(),
            Err(error) => {
                tracing::warn!(err = %error, "DB error reading outputs for runtime state");
                return;
            }
        };
        self.publish(project(engine, &pipelines, &outputs).await)
            .await;
    }

    fn has_clients(&self) -> bool {
        self.deltas.receiver_count() > 0
    }
}

/// Rebuilds the projection on change, and every `interval` while a stream
/// client is connected.
pub async fn run_projector(
    hub: Arc<StateHub>,
    pool: SqlitePool,
    engine: Arc<MediaEngine>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    loop {
        let periodic = async {
            if hub.has_clients() {
                tokio::time::sleep(interval).await;
            } else {
                std::future::pending::<()>().await;
            }
        };
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = hub.changed.notified() => {}
            () = periodic => {}
        }
        hub.refresh(&pool, &engine).await;
        tokio::time::sleep(MIN_REBUILD_GAP).await;
    }
}
