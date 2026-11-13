//! The output pass: rows, topology, input convergence and egress start,
//! stop and retry, on the main future. Listener supervision
//! (`runtime.rs`) and housekeeping (`housekeeping.rs`) run in tasks of their
//! own, so nothing here delays them; every database call is bounded by the
//! pass's [`DeadlineDb`], and each output's slow work runs in its own task
//! (`egress.rs`).

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt as _;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

use crate::application::topology::{
    ReconcileRequests, apply_requested_selections, converge_ingest_key_state, converge_pull_inputs,
    converge_srt_call_inputs, converge_topology,
};
use crate::config::AppConfig;
use crate::db::DeadlineDb;
use restream_engine::engine::MediaEngine;

use super::egress::EgressReconciler;

/// Backstop output pass, in reconcile intervals, for state changes no
/// trigger reports.
const BACKSTOP_INTERVALS: u32 = 10;
/// How long shutdown waits for output tasks still starting.
const SHUTDOWN_START_WAIT: Duration = Duration::from_secs(2);

pub(super) struct Reconciler {
    config: Arc<AppConfig>,
    db: DeadlineDb,
    engine: Arc<MediaEngine>,
    egress: EgressReconciler,
    requests: Arc<ReconcileRequests>,
    hls_ingest: Arc<restream_engine::hls_ingest::HlsIngest>,
}

impl Reconciler {
    pub fn new(
        config: Arc<AppConfig>,
        db: DeadlineDb,
        engine: Arc<MediaEngine>,
        requests: Arc<ReconcileRequests>,
        runtime_state: Arc<crate::application::runtime_state::hub::StateHub>,
        hls_ingest: Arc<restream_engine::hls_ingest::HlsIngest>,
    ) -> Self {
        let egress = EgressReconciler::new(
            engine.clone(),
            db.clone(),
            config.tuning,
            runtime_state,
            requests.clone(),
        );
        Self {
            config,
            db,
            engine,
            egress,
            requests,
            hls_ingest,
        }
    }

    /// Runs output passes until shutdown. A pass runs only when requested
    /// or due: a mutation or lifecycle event, a deadline it scheduled itself
    /// (retry backoff, ingest-disconnect grace, a step that could not read
    /// its rows, an output task's end), and a slow backstop pass every
    /// [`BACKSTOP_INTERVALS`] intervals for anything no trigger covers.
    /// Passes are at least one interval apart, so a storm of events or
    /// deadlines (a destination outage) costs no more than a timer; a
    /// request after a quiet interval still runs at once, and one in the gap
    /// waits for its end.
    pub async fn run(&self, shutdown: &CancellationToken) {
        let interval = Duration::from_millis(self.config.tuning.reconciler_interval_ms);
        let backstop = interval * BACKSTOP_INTERVALS;
        // Due at once: converge right after startup.
        let mut next_backstop = Instant::now();
        let mut next_pass_allowed = next_backstop;

        loop {
            let gap = Instant::now() < next_pass_allowed;
            let wake = if gap {
                next_pass_allowed
            } else {
                self.requests
                    .next_deadline()
                    .map_or(next_backstop, |at| at.min(next_backstop))
            };
            // In the gap requests stay pending: the Notify is not polled, so
            // its permit cannot spin the loop before the gap ends.
            let alive = tokio::select! {
                () = shutdown.cancelled() => false,
                () = tokio::time::sleep_until(wake.into()) => true,
                () = self.requests.woken(), if !gap => true,
            };
            if !alive {
                break;
            }
            let now = Instant::now();
            if now >= next_pass_allowed && (self.requests.take_due(now) || now >= next_backstop) {
                next_backstop = now + backstop;
                self.contained_output_pass(interval).await;
                next_pass_allowed = Instant::now() + interval;
            }
        }
        self.egress.finish(SHUTDOWN_START_WAIT).await;
        // Pull tasks fetch from origins that are not ours: stop them with the
        // runtime rather than leaving requests in flight at shutdown.
        self.hls_ingest.cancel_all_pulls();
    }

    /// One output pass behind the panic boundary; after a panic the pass is
    /// retried after the usual gap rather than waiting for a trigger.
    async fn contained_output_pass(&self, interval: Duration) -> bool {
        let completed = contained("output_pass", self.output_pass(interval)).await;
        if !completed {
            self.requests.wake_at(Instant::now() + interval);
        }
        completed
    }

    /// Each step reads its own rows under the deadline. A step that cannot
    /// read skips its convergence and the pass is retried after the gap;
    /// the other steps still run.
    async fn output_pass(&self, interval: Duration) {
        let mut converged =
            apply_requested_selections(&self.db, &self.engine, &self.requests).await;
        let outputs = match self.db.read(crate::db::list_outputs).await {
            Ok(records) => Some(
                records
                    .into_iter()
                    .map(crate::application::outputs::from_record)
                    .collect::<Vec<_>>(),
            ),
            Err(error) => {
                warn!(err = %error, "DB error reading outputs; output pass retried");
                None
            }
        };

        converged &= converge_pull_inputs(&self.db, &self.hls_ingest)
            .await
            .is_some();
        converged &= converge_srt_call_inputs(&self.db, &self.engine)
            .await
            .is_some();
        match converge_ingest_key_state(&self.db, &self.hls_ingest).await {
            Some(released) => {
                for input_id in released {
                    info!(input_id = %input_id, "ingest key state released for a deleted input");
                }
            }
            None => converged = false,
        }
        match outputs {
            Some(outputs) => {
                converged &= converge_topology(&self.db, &self.engine, &outputs).await;
                if let Some(wait) = self.egress.reconcile(&outputs).await {
                    self.requests.wake_at(Instant::now() + wait);
                }
            }
            None => converged = false,
        }
        if !converged {
            self.requests.wake_at(Instant::now() + interval);
        }
    }
}

/// Runs one reconciler or housekeeping step behind a panic boundary: the
/// output pass runs on the main future, so a panic in a step would
/// otherwise end the process, and a housekeeping panic would wait out its
/// supervisor's restart backoff. Returns whether the step completed.
pub(super) async fn contained(step: &'static str, work: impl Future<Output = ()>) -> bool {
    let completed = std::panic::AssertUnwindSafe(work)
        .catch_unwind()
        .await
        .is_ok();
    if !completed {
        error!(
            step,
            event_class = "lifecycle",
            event_type = "reconciler.step_panicked",
            "panic in reconciler step; continuing with the next tick"
        );
    }
    completed
}

#[cfg(test)]
mod tests;
