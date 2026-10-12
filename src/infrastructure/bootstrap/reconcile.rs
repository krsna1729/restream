use std::collections::HashSet;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sqlx::SqlitePool;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::application::ports::PipelineStore;
use crate::application::reconcile::build_recording_reconcile_plan;
use crate::application::topology::{
    ReconcileRequests, apply_requested_selections, converge_topology,
};
use crate::config::AppConfig;
use crate::infrastructure::sqlite_ports::SqliteMetaStore;
use crate::media::engine::MediaEngine;
use crate::media::recording::RecordingMetadataReporter;

use super::egress::EgressReconciler;
use super::runtime::RuntimeTasks;

/// Backstop output pass, in reconcile intervals, for state changes no
/// trigger reports.
const BACKSTOP_INTERVALS: u32 = 10;

pub(super) struct Reconciler {
    config: Arc<AppConfig>,
    pool: SqlitePool,
    engine: Arc<MediaEngine>,
    pipeline_catalog: Arc<dyn PipelineStore>,
    meta_store: SqliteMetaStore,
    sessions: Arc<RwLock<HashSet<String>>>,
    recording_metadata: RecordingMetadataReporter,
    egress: EgressReconciler,
    requests: Arc<ReconcileRequests>,
}

impl Reconciler {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: Arc<AppConfig>,
        pool: SqlitePool,
        engine: Arc<MediaEngine>,
        pipeline_catalog: Arc<dyn PipelineStore>,
        meta_store: SqliteMetaStore,
        sessions: Arc<RwLock<HashSet<String>>>,
        recording_metadata: RecordingMetadataReporter,
        requests: Arc<ReconcileRequests>,
    ) -> Self {
        let egress = EgressReconciler::new(engine.clone(), pool.clone(), config.tuning);
        Self {
            config,
            pool,
            engine,
            pipeline_catalog,
            meta_store,
            sessions,
            recording_metadata,
            egress,
            requests,
        }
    }

    /// Runs until shutdown. Two cadences (Q-040):
    /// - the output pass (rows, topology, egress start/stop/retry) runs only
    ///   when requested or due: a mutation or lifecycle event, a deadline it
    ///   scheduled itself (retry backoff, ingest-disconnect grace), and a
    ///   slow backstop pass every
    ///   [`BACKSTOP_INTERVALS`] intervals for anything no trigger covers.
    ///   Passes are at least one interval apart, so a storm of events or
    ///   deadlines (a destination outage) costs no more than the old timer;
    ///   a request after a quiet interval still runs at once, and one in the
    ///   gap waits for its end;
    /// - housekeeping that reads no outputs (sessions, fabric sizing,
    ///   recordings, idle HLS, file-ingest reaping) runs every interval.
    pub async fn run(&self, runtime: &mut RuntimeTasks, shutdown: &CancellationToken) {
        let tuning = self.config.tuning;
        let interval = Duration::from_millis(tuning.reconciler_interval_ms);
        let backstop = interval * BACKSTOP_INTERVALS;
        let session_prune_every_ticks = tuning.session_prune_every_ticks();
        let mut tick = 0_u64;
        // Both are due at once: converge right after startup.
        let mut next_tick = Instant::now();
        let mut next_backstop = next_tick;
        let mut next_pass_allowed = next_tick;

        loop {
            let gap = Instant::now() < next_pass_allowed;
            let pass_wake = if gap {
                // Requests stay pending; the Notify is not polled, so its
                // permit cannot spin the loop before the gap ends.
                Some(next_pass_allowed)
            } else {
                [Some(next_backstop), self.requests.next_deadline()]
                    .into_iter()
                    .flatten()
                    .min()
            };
            let wake = pass_wake.map_or(next_tick, |at| at.min(next_tick));
            let wait = wake.saturating_duration_since(Instant::now());
            let alive = if gap {
                runtime
                    .wait_for_reconcile_tick(shutdown, wait, std::future::pending())
                    .await
            } else {
                runtime
                    .wait_for_reconcile_tick(shutdown, wait, self.requests.woken())
                    .await
            };
            if !alive {
                break;
            }
            let now = Instant::now();
            if now >= next_pass_allowed && (self.requests.take_due(now) || now >= next_backstop) {
                next_backstop = now + backstop;
                self.output_pass(interval).await;
                next_pass_allowed = Instant::now() + interval;
            }
            if now >= next_tick {
                next_tick = now + interval;
                tick += 1;
                if tick.is_multiple_of(session_prune_every_ticks) {
                    self.prune_sessions_and_logs().await;
                }
                self.engine.resize_srt_fabrics().await;
                self.engine.resize_rtmp_fabrics().await;
                if self.reconcile_recordings(tick).await {
                    self.sweep_idle_hls().await;
                    self.engine.reap_file_ingests().await;
                }
            }
        }
    }

    async fn output_pass(&self, interval: Duration) {
        apply_requested_selections(&self.pool, &self.engine, &self.requests).await;
        let outputs = match crate::db::list_outputs(&self.pool).await {
            Ok(records) => records
                .into_iter()
                .map(crate::application::outputs::from_record)
                .collect::<Vec<_>>(),
            Err(error) => {
                warn!(err = %error, "DB error reading outputs; output pass retried");
                self.requests.wake_at(Instant::now() + interval);
                return;
            }
        };

        converge_topology(&self.pool, &self.engine, &outputs).await;
        if let Some(wait) = self.egress.reconcile(&outputs).await {
            self.requests.wake_at(Instant::now() + wait);
        }
    }

    async fn prune_sessions_and_logs(&self) {
        let _ = crate::db::prune_expired_sessions(&self.pool, 30 * 24 * 60 * 60 * 1000).await;
        let _ = crate::db::delete_app_logs_older_than(
            &self.pool,
            self.config.log_retention_days as i64,
        )
        .await;
        match crate::db::list_sessions(&self.pool).await {
            Ok(live_tokens) => {
                let live_set = live_tokens.into_iter().collect::<HashSet<_>>();
                self.sessions
                    .write()
                    .await
                    .retain(|token| live_set.contains(token));
            }
            Err(error) => warn!(err = %error, "failed to list sessions for prune"),
        }
    }

    async fn reconcile_recordings(&self, tick: u64) -> bool {
        let commands = match build_recording_reconcile_plan(
            &self.engine,
            self.pipeline_catalog.as_ref(),
            &self.meta_store,
            self.config.tuning.ingest_disconnect_grace_ms,
        )
        .await
        {
            Ok(commands) => commands,
            Err(error) => {
                warn!(
                    tick,
                    err = %error,
                    "pipeline catalog error while reconciling recordings"
                );
                return false;
            }
        };
        crate::application::recording::apply_recording_commands(
            self.engine.clone(),
            &self.meta_store,
            &self.config.media_dir,
            commands,
            Some(self.recording_metadata.clone()),
        )
        .await;
        true
    }

    async fn sweep_idle_hls(&self) {
        for pipeline_id in self.engine.hls_pipeline_ids().await {
            if self
                .engine
                .has_recent_ingest_disconnect(
                    &pipeline_id,
                    self.config.tuning.ingest_disconnect_grace_ms,
                )
                .await
            {
                continue;
            }
            if self
                .engine
                .should_shutdown_hls_segmenter(&pipeline_id, self.config.tuning.hls_idle_timeout_ms)
                .await
            {
                self.engine.shutdown_hls_segmenter(&pipeline_id).await;
            }
        }
        for pipeline_id in self.engine.hls_preview_pipeline_ids().await {
            if self
                .engine
                .has_recent_ingest_disconnect(
                    &pipeline_id,
                    self.config.tuning.ingest_disconnect_grace_ms,
                )
                .await
            {
                continue;
            }
            if self
                .engine
                .should_shutdown_hls_preview_segmenter(
                    &pipeline_id,
                    self.config.tuning.hls_idle_timeout_ms,
                )
                .await
            {
                self.engine
                    .shutdown_hls_preview_segmenter(&pipeline_id)
                    .await;
            }
        }
    }
}
