//! Housekeeping: the periodic work that reads no outputs — session and log
//! pruning, fabric sizing, recordings, stream checks, idle HLS segmenters and
//! file-ingest reaping. It runs in a task of its own, so a stall here never
//! delays an output pass or a listener restart, and its database calls are
//! bounded by its [`DeadlineDb`]: each tick reads the pipelines and every
//! pipeline's switches in two statements, whatever the pipeline count.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::application::ports::PipelineStore;
use crate::application::reconcile::{HousekeepingSwitches, build_recording_reconcile_plan};
use crate::config::AppConfig;
use crate::db::DeadlineDb;
use restream_domain::models::Pipeline;
use restream_engine::engine::MediaEngine;
use restream_engine::recording::RecordingMetadataReporter;

use super::reconcile::contained;

/// Sessions older than this are pruned.
const SESSION_MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

pub(super) struct Housekeeping {
    config: Arc<AppConfig>,
    db: DeadlineDb,
    engine: Arc<MediaEngine>,
    /// Lists pipelines on the lifecycle pool, bounded by `db`'s deadline.
    pipeline_catalog: Arc<dyn PipelineStore>,
    sessions: Arc<RwLock<HashSet<String>>>,
    recording_metadata: RecordingMetadataReporter,
}

impl Housekeeping {
    pub fn new(
        config: Arc<AppConfig>,
        db: DeadlineDb,
        engine: Arc<MediaEngine>,
        pipeline_catalog: Arc<dyn PipelineStore>,
        sessions: Arc<RwLock<HashSet<String>>>,
        recording_metadata: RecordingMetadataReporter,
    ) -> Self {
        Self {
            config,
            db,
            engine,
            pipeline_catalog,
            sessions,
            recording_metadata,
        }
    }

    /// Ticks every reconciler interval until `shutdown`.
    pub async fn run(self: Arc<Self>, shutdown: CancellationToken) {
        let tuning = self.config.tuning;
        let mut ticks = tokio::time::interval(Duration::from_millis(tuning.reconciler_interval_ms));
        ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        let prune_every = tuning.session_prune_every_ticks();
        let mut tick = 0_u64;
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = ticks.tick() => {}
            }
            tick += 1;
            self.contained_tick(tick, prune_every).await;
        }
    }

    /// One tick behind the panic boundary: the next tick runs on time.
    async fn contained_tick(&self, tick: u64, prune_every: u64) -> bool {
        contained("housekeeping", self.tick(tick, prune_every)).await
    }

    async fn tick(&self, tick: u64, prune_every: u64) {
        if tick.is_multiple_of(prune_every) {
            self.prune_sessions_and_logs().await;
        }
        self.engine.resize_srt_fabrics().await;
        self.engine.resize_rtmp_fabrics().await;
        self.engine.resize_moq_fabrics().await;
        self.engine.resize_sink_fabrics().await;
        self.engine.resize_pipeline_fabrics().await;
        self.engine.resize_hls_put_fabrics().await;
        if let Some((pipelines, switches)) = self.read_switches(tick).await {
            self.reconcile_recordings(&pipelines, &switches).await;
            crate::application::stream_qa::reconcile_stream_qa(
                &self.engine,
                &pipelines,
                &switches,
                self.config.tuning.ingest_disconnect_grace_ms,
            )
            .await;
        }
        self.sweep_idle_hls().await;
        self.engine.reap_file_ingests().await;
    }

    /// The pipelines and their switches, or `None` (recordings and stream
    /// checks keep their state this tick) when either read fails.
    async fn read_switches(&self, tick: u64) -> Option<(Vec<Pipeline>, HousekeepingSwitches)> {
        let pipelines = match self.db.bound(self.pipeline_catalog.list_pipelines()).await {
            Ok(pipelines) => pipelines,
            Err(error) => {
                warn!(tick, err = %error, "pipeline catalog error while reconciling recordings");
                return None;
            }
        };
        match self
            .db
            .read(|pool| {
                crate::db::list_meta_with_prefixes(pool, &HousekeepingSwitches::META_PREFIXES)
            })
            .await
        {
            Ok(rows) => Some((pipelines, HousekeepingSwitches::from_meta(rows))),
            Err(error) => {
                warn!(tick, err = %error, "DB error reading recording and stream-check switches");
                None
            }
        }
    }

    async fn reconcile_recordings(&self, pipelines: &[Pipeline], switches: &HousekeepingSwitches) {
        let commands = build_recording_reconcile_plan(
            &self.engine,
            pipelines,
            switches,
            self.config.tuning.ingest_disconnect_grace_ms,
        )
        .await;
        crate::application::recording::apply_recording_commands(
            self.engine.clone(),
            &switches.recording_settings,
            &self.config.media_dir,
            commands,
            Some(self.recording_metadata.clone()),
        )
        .await;
    }

    async fn prune_sessions_and_logs(&self) {
        if let Err(error) = self
            .db
            .write(|pool| async move {
                crate::db::prune_expired_sessions(&pool, SESSION_MAX_AGE_MS).await
            })
            .await
        {
            warn!(err = %error, "failed to prune expired sessions");
        }
        if let Err(error) = crate::db::prune_app_logs(
            &self.db,
            self.config.log_retention_days as i64,
            crate::db::APP_LOG_RETENTION_BATCH,
        )
        .await
        {
            warn!(err = %error, "app log retention stopped early; the next prune continues");
        }
        match self.db.read(crate::db::list_sessions).await {
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

    async fn sweep_idle_hls(&self) {
        let grace_ms = self.config.tuning.ingest_disconnect_grace_ms;
        let idle_ms = self.config.tuning.hls_idle_timeout_ms;
        for pipeline_id in self.engine.hls_pipeline_ids().await {
            if self
                .engine
                .has_recent_ingest_disconnect(&pipeline_id, grace_ms)
                .await
            {
                continue;
            }
            if self
                .engine
                .should_shutdown_hls_segmenter(&pipeline_id, idle_ms)
                .await
            {
                self.engine.shutdown_hls_segmenter(&pipeline_id).await;
            }
        }
        for pipeline_id in self.engine.hls_preview_pipeline_ids().await {
            if self
                .engine
                .has_recent_ingest_disconnect(
                    restream_engine::engine_hls::preview_pipeline(&pipeline_id).0,
                    grace_ms,
                )
                .await
            {
                continue;
            }
            if self
                .engine
                .should_shutdown_hls_preview_segmenter(&pipeline_id, idle_ms)
                .await
            {
                self.engine
                    .shutdown_hls_preview_segmenter(&pipeline_id)
                    .await;
            }
        }
    }
}

#[cfg(test)]
mod tests;
