//! The output pass's per-output panic boundary: which outputs panicked while
//! being planned, and how long each backs off.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use tracing::error;

use crate::config::RuntimeTuning;
use restream_domain::models::Output;

/// Outputs whose planning (the pass's decision, or the start task) panicked:
/// the panic count and when the output may be planned again. The output is
/// left alone until then, so a panic that recurs costs one attempt per
/// backoff, never the other outputs' pass.
#[derive(Clone, Default)]
pub(super) struct PlanningPanics(Arc<std::sync::Mutex<HashMap<String, (u32, Instant)>>>);

impl PlanningPanics {
    /// How long until the output may be planned again, if it is backing off.
    pub(super) fn remaining(&self, output_id: &str, now: Instant) -> Option<Duration> {
        restream_sync::lock::lock(&self.0)
            .get(output_id)
            .and_then(|(_, retry_at)| retry_at.checked_duration_since(now))
            .filter(|remaining| !remaining.is_zero())
    }

    /// Records a panic while planning the output; returns its backoff.
    pub(super) fn record(&self, output_id: &str, tuning: RuntimeTuning) -> Duration {
        let mut panics = restream_sync::lock::lock(&self.0);
        let count = panics.get(output_id).map_or(0, |(count, _)| *count) + 1;
        let backoff = Duration::from_millis(tuning.output_backoff_ms(count, output_id));
        panics.insert(output_id.to_string(), (count, Instant::now() + backoff));
        drop(panics);
        error!(
            output_id = %output_id,
            panics = count,
            retry_in_ms = backoff.as_millis() as u64,
            event_class = "lifecycle",
            event_type = "reconciler.output_planning_panicked",
            "panic while planning an output; the output is retried after its backoff and the pass goes on"
        );
        backoff
    }

    /// The output runs, or is no longer wanted: its past panics no longer
    /// grow its backoff.
    pub(super) fn clear(&self, output_id: &str) {
        restream_sync::lock::lock(&self.0).remove(output_id);
    }

    /// Forgets outputs whose rows are gone.
    pub(super) fn retain_outputs(&self, outputs: &[Output]) {
        let mut panics = restream_sync::lock::lock(&self.0);
        if !panics.is_empty() {
            panics.retain(|id, _| outputs.iter().any(|output| output.id == *id));
        }
    }
}
