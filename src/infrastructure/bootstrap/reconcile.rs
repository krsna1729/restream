use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use sqlx::SqlitePool;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::application::ports::PipelineStore;
use crate::application::reconcile::{
    build_recording_reconcile_plan, collect_needed_stage_keys, load_output_runtime_snapshot,
    output_stage_sweep_input,
};
use crate::config::AppConfig;
use crate::domain::stage::StageKey;
use crate::infrastructure::sqlite_ports::SqliteMetaStore;
use crate::media::engine::MediaEngine;
use crate::media::recording::RecordingMetadataReporter;

use super::egress::EgressReconciler;
use super::runtime::RuntimeTasks;

pub(super) struct Reconciler {
    config: Arc<AppConfig>,
    pool: SqlitePool,
    engine: Arc<MediaEngine>,
    pipeline_catalog: Arc<dyn PipelineStore>,
    meta_store: SqliteMetaStore,
    sessions: Arc<RwLock<HashSet<String>>>,
    recording_metadata: RecordingMetadataReporter,
    egress: EgressReconciler,
}

impl Reconciler {
    pub fn new(
        config: Arc<AppConfig>,
        pool: SqlitePool,
        engine: Arc<MediaEngine>,
        pipeline_catalog: Arc<dyn PipelineStore>,
        meta_store: SqliteMetaStore,
        sessions: Arc<RwLock<HashSet<String>>>,
        recording_metadata: RecordingMetadataReporter,
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
        }
    }

    pub async fn run(&self, runtime: &mut RuntimeTasks, shutdown: &CancellationToken) {
        let tuning = self.config.tuning;
        let mut tick = 0_u64;
        let session_prune_every_ticks = tuning.session_prune_every_ticks();

        while runtime
            .wait_for_reconcile_tick(
                shutdown,
                Duration::from_millis(tuning.reconciler_interval_ms),
            )
            .await
        {
            tick += 1;
            if tick.is_multiple_of(session_prune_every_ticks) {
                self.prune_sessions_and_logs().await;
            }

            let outputs = match crate::db::list_outputs(&self.pool).await {
                Ok(records) => records
                    .into_iter()
                    .map(crate::infrastructure::sqlite_ports::records::output_model)
                    .collect::<Vec<_>>(),
                Err(error) => {
                    warn!(tick, err = %error, "DB error reading outputs");
                    continue;
                }
            };

            self.egress.reconcile(&outputs).await;
            self.sweep_unused_stages(&outputs).await;
            if !self.reconcile_recordings(tick).await {
                continue;
            }
            self.sweep_idle_hls().await;
            self.engine.reap_file_ingests().await;
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

    async fn sweep_unused_stages(&self, outputs: &[crate::application::models::Output]) {
        let mut stage_inputs = Vec::with_capacity(outputs.len());
        for output in outputs {
            let snapshot = load_output_runtime_snapshot(
                &self.engine,
                output,
                self.config.tuning.ingest_disconnect_grace_ms,
            )
            .await;
            stage_inputs.push(output_stage_sweep_input(output, &snapshot));
        }
        let mut needed_stages: HashSet<StageKey> =
            collect_needed_stage_keys(stage_inputs, &self.engine.backend_policy());
        needed_stages.extend(self.engine.active_hls_preview_stage_keys().await);
        self.engine
            .sweep_unused_transcoder_stages(&needed_stages)
            .await;
        self.engine.sweep_unused_stages().await;
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
