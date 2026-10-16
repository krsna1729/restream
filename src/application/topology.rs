//! Runtime convergence for topology mutations (Q-039).
//!
//! API and agent handlers only persist a change and then call
//! [`ReconcileRequests`]; the reconciler applies it with
//! [`converge_topology`]. A handler never tears down or switches runtime state
//! itself, so the persisted state stays authoritative: a crash or a failed
//! runtime step between the two halves is converged by the next pass, and a
//! restart starts from the same rows.

use std::collections::{BTreeSet, HashSet};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use sqlx::SqlitePool;
use tokio::sync::Notify;
use tracing::warn;

use crate::application::models::Output;
use crate::media::engine::MediaEngine;

/// Requests to the reconciler (Q-039, Q-040). The output pass runs only when
/// something asked for it: a mutation or engine lifecycle event
/// ([`Self::wake`]), or a deadline that came due ([`Self::wake_at`]: retry
/// backoff, ingest-disconnect grace). A deadline only
/// wakes the pass, which re-reads current state, so a stale deadline costs one
/// pass and can never act on old state.
#[derive(Default)]
pub struct ReconcileRequests {
    wake: Notify,
    dirty: AtomicBool,
    deadlines: Mutex<BTreeSet<Instant>>,
    dirty_selections: Mutex<HashSet<String>>,
}

impl ReconcileRequests {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run an output pass now. A wake sent while a pass runs is kept: the
    /// loop runs one more pass after it.
    pub fn wake(&self) {
        self.dirty.store(true, Ordering::Release);
        self.wake.notify_one();
    }

    /// Run an output pass at `at` (or right away if it is past).
    pub fn wake_at(&self, at: Instant) {
        crate::sync::lock(&self.deadlines).insert(at);
        // The loop recomputes its sleep with the new deadline.
        self.wake.notify_one();
    }

    /// Whether an output pass is due at `now`: consumes the dirty flag and
    /// every deadline at or before `now`.
    pub fn take_due(&self, now: Instant) -> bool {
        let dirty = self.dirty.swap(false, Ordering::AcqRel);
        let mut deadlines = crate::sync::lock(&self.deadlines);
        let before = deadlines.len();
        deadlines.retain(|at| *at > now);
        dirty || deadlines.len() != before
    }

    /// The earliest pending deadline.
    pub fn next_deadline(&self) -> Option<Instant> {
        crate::sync::lock(&self.deadlines).first().copied()
    }

    /// The pipeline's persisted input selection changed: the next pass makes
    /// the engine forward the selected input.
    ///
    /// Selection is applied only on request, never by comparing state each
    /// pass: legacy and file-ingest sessions register under their stream key,
    /// not an input row id, so the engine's selected id legitimately differs
    /// from the persisted one while they publish.
    pub fn input_selection_changed(&self, pipeline_id: &str) {
        crate::sync::lock(&self.dirty_selections).insert(pipeline_id.to_string());
        self.wake();
    }

    /// Resolves when a handler asked for a pass.
    pub async fn woken(&self) {
        self.wake.notified().await;
    }

    fn take_dirty_selections(&self) -> HashSet<String> {
        std::mem::take(&mut *crate::sync::lock(&self.dirty_selections))
    }

    fn keep_dirty_selection(&self, pipeline_id: String) {
        crate::sync::lock(&self.dirty_selections).insert(pipeline_id);
    }
}

/// Engine lifecycle events that change what the output pass decides: an
/// ingest arriving or leaving (outputs start or stop; a departure also once
/// its grace ends), an ingest's probe landing (outputs held for it start) and
/// an egress ending (retry it). Stages need no pass: their consumers' leases
/// stop them (Q-041).
/// `EgressFailed` is not one: it reports each error of a still-active egress
/// (every upload retry), and the egress ending emits `EgressStopped`.
pub fn request_pass_for_event(
    requests: &ReconcileRequests,
    event: &crate::events::EventKind,
    ingest_disconnect_grace: std::time::Duration,
) {
    use crate::events::EventKind;
    match event {
        EventKind::IngestDisconnected { .. } => {
            requests.wake();
            // The grace check is `elapsed < grace`; pass just after it ends.
            requests.wake_at(
                Instant::now() + ingest_disconnect_grace + std::time::Duration::from_millis(10),
            );
        }
        EventKind::IngestConnected { .. }
        | EventKind::IngestProbed { .. }
        | EventKind::EgressStopped { .. } => requests.wake(),
        _ => {}
    }
}

/// Applies requested input selections from their persisted rows. The
/// reconciler runs this first in a pass, so a manual failover waits only for
/// a pass already in progress. A deleted pipeline has no input rows: no-op.
pub async fn apply_requested_selections(
    pool: &SqlitePool,
    engine: &MediaEngine,
    requests: &ReconcileRequests,
) {
    for pipeline_id in requests.take_dirty_selections() {
        match crate::db::list_pipeline_inputs(pool, &pipeline_id).await {
            Ok(inputs) => {
                if let Some(selected) = inputs.iter().find(|input| input.selected) {
                    engine
                        .select_pipeline_input(&pipeline_id, &selected.id)
                        .await;
                }
            }
            Err(error) => {
                warn!(pipeline_id = %pipeline_id, err = %error, "DB error reading inputs; selection retried next pass");
                requests.keep_dirty_selection(pipeline_id);
            }
        }
    }
}

/// Converge runtime with the persisted topology: tear down deleted pipelines
/// and stop egresses whose output row is gone. `outputs` are the rows this
/// reconcile pass read.
pub async fn converge_topology(pool: &SqlitePool, engine: &MediaEngine, outputs: &[Output]) {
    // Snapshot runtime before reading rows. Runtime for a pipeline is created
    // only after its row commits, so an id seen here and missing from the rows
    // read below belongs to a deleted pipeline, never to a new one.
    let runtime_pipelines = engine.runtime_pipeline_ids().await;
    let pipelines = match crate::db::list_pipelines(pool).await {
        Ok(rows) => rows.into_iter().map(|row| row.id).collect::<HashSet<_>>(),
        Err(error) => {
            warn!(err = %error, "DB error reading pipelines; topology not converged this pass");
            return;
        }
    };
    // Only the reconciler registers egresses, always from its own output rows,
    // so a registration absent from these rows is for a deleted output.
    // Consumers stop before their producers: an egress whose stage the
    // teardown below cancels is then already stopped, not failed with
    // `stage_ended` and counted for a retry.
    let wanted = outputs
        .iter()
        .filter(|output| pipelines.contains(&output.pipeline_id))
        .map(|output| output.id.as_str())
        .collect::<HashSet<_>>();
    for output_id in engine.registered_egress_output_ids().await {
        if !wanted.contains(output_id.as_str()) {
            engine.unregister_egress(&output_id).await;
            engine.clear_egress_retry_state(&output_id).await;
        }
    }

    for pipeline_id in runtime_pipelines.difference(&pipelines) {
        engine.teardown_pipeline_runtime(pipeline_id).await;
    }
}

#[cfg(test)]
#[path = "topology_tests.rs"]
mod tests;
