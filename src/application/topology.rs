//! Runtime convergence for topology mutations (Q-039).
//!
//! API and agent handlers only persist a change and then call
//! [`ReconcileRequests`]; the reconciler applies it with
//! [`converge_topology`]. A handler never tears down or switches runtime state
//! itself, so the persisted state stays authoritative: a crash or a failed
//! runtime step between the two halves is converged by the next pass, and a
//! restart starts from the same rows.

use std::collections::HashSet;
use std::sync::Mutex;

use sqlx::SqlitePool;
use tokio::sync::Notify;
use tracing::warn;

use crate::application::models::Output;
use crate::media::engine::MediaEngine;

/// Requests from mutation handlers to the reconciler.
#[derive(Default)]
pub struct ReconcileRequests {
    wake: Notify,
    dirty_selections: Mutex<HashSet<String>>,
}

impl ReconcileRequests {
    pub fn new() -> Self {
        Self::default()
    }

    /// Run a reconcile pass now instead of at the next interval tick. A wake
    /// sent while no pass is waiting is kept for the next wait.
    pub fn wake(&self) {
        self.wake.notify_one();
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

/// Converge runtime with the persisted topology: tear down deleted pipelines,
/// stop egresses whose output row is gone, and apply requested input
/// selections. `outputs` are the rows this reconcile pass read.
pub async fn converge_topology(
    pool: &SqlitePool,
    engine: &MediaEngine,
    requests: &ReconcileRequests,
    outputs: &[Output],
) {
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
    for pipeline_id in runtime_pipelines.difference(&pipelines) {
        engine.teardown_pipeline_runtime(pipeline_id).await;
    }

    // Only the reconciler registers egresses, always from its own output rows,
    // so a registration absent from these rows is for a deleted output.
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

    for pipeline_id in requests.take_dirty_selections() {
        if !pipelines.contains(&pipeline_id) {
            continue;
        }
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

#[cfg(test)]
#[path = "topology_tests.rs"]
mod tests;
