//! Holds the current projection under a revision and publishes deltas.
//!
//! Change points mark the hub changed ([`StateHub::mark_changed`]): every
//! engine lifecycle event, every successful mutating request, and the
//! egress reconciler's retries-exhausted write. The projector
//! ([`run_projector`]) rebuilds on a change and, while a stream client is
//! connected, once per interval for state no event reports (flow, stalls,
//! retry countdowns). With no client connected nothing runs between changes;
//! `GET /api/v1/state` and a connecting stream refresh on demand, reusing a
//! projection whose rebuild completed less than [`ON_DEMAND_REUSE`] ago
//! when no change was marked since, so a client polling in a loop costs at
//! most one rebuild per window.

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::{Mutex, Notify, broadcast};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use super::{Projection, RuntimeState, StateDelta, diff, project};
use crate::media::engine::MediaEngine;

/// Deltas kept for clients that reconnect with `since`; older gaps resync
/// from a snapshot.
pub const DELTA_HISTORY: usize = 256;

/// How long an on-demand read reuses the last projection when no change was
/// marked since: the staleness bound for state no event reports (flow,
/// stalls, retry countdowns), and the bound on rebuilds a polling client
/// can cause.
pub const ON_DEMAND_REUSE: Duration = Duration::from_millis(500);

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
    epoch: String,
    revision: u64,
    projection: Projection,
    history: VecDeque<Arc<StateDelta>>,
}

pub struct StateHub {
    current: Mutex<Current>,
    /// Serializes refreshes, so an older projection never publishes after
    /// a newer one, and records the last one for on-demand reuse.
    refreshing: Mutex<LastRefresh>,
    deltas: broadcast::Sender<Arc<StateDelta>>,
    changed: Notify,
    /// Marked changes so far; a refresh records the count it read.
    changes: AtomicU64,
}

/// When the last rebuild completed, and the marked-change count it read
/// before reading the rows.
#[derive(Default)]
pub(super) struct LastRefresh {
    pub(super) at: Option<Instant>,
    pub(super) changes: u64,
}

impl LastRefresh {
    /// The last projection still answers a request at `now`: completed less
    /// than [`ON_DEMAND_REUSE`] ago, with no change marked since.
    pub(super) fn covers(&self, now: Instant, changes: u64) -> bool {
        self.at
            .is_some_and(|at| now.saturating_duration_since(at) < ON_DEMAND_REUSE)
            && self.changes == changes
    }
}

impl Default for StateHub {
    fn default() -> Self {
        Self::new()
    }
}

impl StateHub {
    pub fn new() -> Self {
        let started = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos());
        Self::with_epoch(format!("{started:x}-{:x}", std::process::id()))
    }

    pub fn with_epoch(epoch: String) -> Self {
        Self {
            refreshing: Mutex::new(LastRefresh::default()),
            current: Mutex::new(Current {
                epoch,
                revision: 0,
                projection: Projection::default(),
                history: VecDeque::with_capacity(DELTA_HISTORY),
            }),
            deltas: broadcast::channel(DELTA_HISTORY).0,
            changed: Notify::new(),
            changes: AtomicU64::new(0),
        }
    }

    /// Something the projection reads may have changed.
    pub fn mark_changed(&self) {
        self.changes.fetch_add(1, Ordering::AcqRel);
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
        delta.epoch = current.epoch.clone();
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
            epoch: current.epoch.clone(),
            revision: current.revision,
            pipelines: current.projection.pipelines.values().cloned().collect(),
            outputs: current.projection.outputs.values().cloned().collect(),
        }
    }

    /// The deltas after a resume token `<epoch>.<revision>`.
    pub async fn since(&self, token: &str) -> Since {
        let Some((epoch, revision)) = token
            .rsplit_once('.')
            .and_then(|(epoch, revision)| Some((epoch, revision.parse::<u64>().ok()?)))
        else {
            return Since::Resync;
        };
        let current = self.current.lock().await;
        if epoch != current.epoch || revision > current.revision {
            // From another server run: its revisions are not this run's.
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
        let mut last = self.refreshing.lock().await;
        self.rebuild(&mut last, pool, engine).await;
    }

    /// [`Self::refresh`] for a request: reuses the current projection when
    /// the last rebuild completed less than [`ON_DEMAND_REUSE`] ago and no
    /// change was marked since. Requests queued behind a rebuild find it
    /// fresh and return at once.
    pub async fn refresh_on_demand(&self, pool: &SqlitePool, engine: &MediaEngine) {
        let mut last = self.refreshing.lock().await;
        if !last.covers(Instant::now(), self.changes.load(Ordering::Acquire)) {
            self.rebuild(&mut last, pool, engine).await;
        }
    }

    async fn rebuild(&self, last: &mut LastRefresh, pool: &SqlitePool, engine: &MediaEngine) {
        // Read before the rows: a change marked during the rebuild is newer
        // than this projection and must not be taken as covered by it.
        let changes = self.changes.load(Ordering::Acquire);
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
        *last = LastRefresh {
            at: Some(Instant::now()),
            changes,
        };
    }

    fn has_clients(&self) -> bool {
        self.deltas.receiver_count() > 0
    }
}

/// While a stream client is connected, rebuilds every `interval` and soon
/// after a change, at most once per `interval` (the reconcile cadence), so
/// an event storm costs no more than one projection per interval. With no
/// client nothing is rebuilt: no one reads the deltas, and skipping
/// projections is safe because each delta is the difference of two whole
/// projections. `GET /api/v1/state` and a connecting stream refresh on
/// demand.
pub async fn run_projector(
    hub: Arc<StateHub>,
    pool: SqlitePool,
    engine: Arc<MediaEngine>,
    interval: Duration,
    shutdown: CancellationToken,
) {
    let mut next_allowed = tokio::time::Instant::now();
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
        if !hub.has_clients() {
            continue;
        }
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep_until(next_allowed) => {}
        }
        hub.refresh(&pool, &engine).await;
        next_allowed = tokio::time::Instant::now() + interval;
    }
}
