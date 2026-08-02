use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::application::models::{JobStatus, Output};
use crate::application::reconcile::{
    OutputFailureWindow, OutputStartAction, OutputStopAction, decide_output_start_action,
    decide_output_stop_action, load_output_runtime_snapshot,
};
use crate::config::RuntimeTuning;
use crate::domain::output_spec::OutputUrlScheme;
use crate::domain::state::DesiredOutputState;
use crate::media::engine::MediaEngine;

#[path = "egress_task.rs"]
mod egress_task;
use egress_task::{EgressTask, PipelineFabricTask, RtmpFabricTask, SinkFabricTask, SrtFabricTask};

pub(super) type FailureTracker = Arc<Mutex<HashMap<String, (Instant, u32)>>>;

pub(super) struct EgressReconciler {
    engine: Arc<MediaEngine>,
    pool: SqlitePool,
    tuning: RuntimeTuning,
    last_failed: FailureTracker,
}

impl EgressReconciler {
    pub fn new(engine: Arc<MediaEngine>, pool: SqlitePool, tuning: RuntimeTuning) -> Self {
        Self {
            engine,
            pool,
            tuning,
            last_failed: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn reconcile(&self, outputs: &[Output]) {
        for output in outputs {
            let snapshot = load_output_runtime_snapshot(
                &self.engine,
                output,
                self.tuning.ingest_disconnect_grace_ms,
            )
            .await;
            if snapshot.is_active || output.desired_state != DesiredOutputState::Running {
                self.engine.clear_egress_retry_state(&output.id).await;
            }
            let failure = {
                let last_failed = self.last_failed.lock().await;
                last_failed
                    .get(&output.id)
                    .map(|(failed_at, retries)| OutputFailureWindow {
                        retries: *retries,
                        elapsed_ms: failed_at.elapsed().as_millis().min(u128::from(u64::MAX))
                            as u64,
                    })
            };

            match decide_output_start_action(
                output.desired_state,
                snapshot.is_active,
                snapshot.effective_has_ingest,
                failure,
                self.tuning.output_retry_policy(),
            ) {
                OutputStartAction::NotApplicable => {}
                OutputStartAction::SkipNoIngest => {
                    self.engine.clear_egress_retry_state(&output.id).await;
                    continue;
                }
                OutputStartAction::MarkFailed => {
                    match crate::db::set_output_desired_state(
                        &self.pool,
                        &output.pipeline_id,
                        &output.id,
                        DesiredOutputState::Failed,
                    )
                    .await
                    {
                        Ok(_) => {
                            self.last_failed.lock().await.remove(&output.id);
                            self.engine.clear_egress_retry_state(&output.id).await;
                            warn!(
                                correlation_id = %crate::logging::next_correlation_id("out"),
                                output_id = %output.id,
                                output_name = %output.name,
                                max_retries = self.tuning.output_max_retries,
                                "output exceeded max retries — marking failed",
                            );
                        }
                        Err(err) => {
                            warn!(
                                correlation_id = %crate::logging::next_correlation_id("out"),
                                output_id = %output.id,
                                error = %err,
                                "failed to set output desired state to failed in db; will retry on next tick",
                            );
                        }
                    }
                    continue;
                }
                OutputStartAction::WaitRetry {
                    retries,
                    backoff_ms,
                    remaining_ms,
                } => {
                    self.engine
                        .update_egress_retry_state(&output.id, retries, backoff_ms, remaining_ms)
                        .await;
                    continue;
                }
                OutputStartAction::StartNow => self.start(output).await,
            }

            match decide_output_stop_action(
                output.desired_state,
                snapshot.is_active,
                snapshot.effective_has_ingest,
            ) {
                OutputStopAction::KeepRunning => {}
                OutputStopAction::StopBecauseIngestLost => {
                    info!(
                        correlation_id = %crate::logging::next_correlation_id("out"),
                        output_id = %output.id,
                        output_name = %output.name,
                        pipeline_id = %output.pipeline_id,
                        event_class = "lifecycle",
                        event_type = "lifecycle.stop",
                        "output job stopped because ingest is no longer active",
                    );
                    self.engine.unregister_egress(&output.id).await;
                    self.engine.clear_egress_retry_state(&output.id).await;
                }
                OutputStopAction::StopRequested => {
                    info!(
                        correlation_id = %crate::logging::next_correlation_id("out"),
                        output_id = %output.id,
                        output_name = %output.name,
                        pipeline_id = %output.pipeline_id,
                        event_class = "lifecycle",
                        event_type = "lifecycle.stop",
                        "output job stopped",
                    );
                    self.engine.unregister_egress(&output.id).await;
                    self.engine.clear_egress_retry_state(&output.id).await;
                }
            }
        }
    }

    async fn start(&self, output: &Output) {
        let correlation_id = crate::logging::next_correlation_id("out");
        info!(
            correlation_id = %correlation_id,
            output_id = %output.id,
            output_name = %output.name,
            pipeline_id = %output.pipeline_id,
            event_class = "lifecycle",
            event_type = "lifecycle.start",
            "output job started",
        );

        let url_scheme = OutputUrlScheme::from_url(&output.url);
        let prepared = crate::application::egress::prepare_output_ring(&self.engine, output).await;
        let use_srt_fabric = matches!(url_scheme, OutputUrlScheme::Srt);
        let encoding = if use_srt_fabric {
            prepared.media_stage_key.kind.to_string()
        } else {
            output.stage_encoding_label()
        };
        let registration = self
            .engine
            .register_egress_attempt_with_meta(
                &output.id,
                &output.pipeline_id,
                &output.url,
                Some(&output.name),
                Some(&encoding),
                Some(prepared.terminal_stage_key.clone()),
            )
            .await;

        let job_id = next_output_job_id(&output.id);
        let now = chrono::Utc::now().to_rfc3339();
        let _ = crate::db::create_job(
            &self.pool,
            &job_id,
            &output.pipeline_id,
            &output.id,
            None,
            JobStatus::Running,
            &now,
        )
        .await;

        let use_rtmp_fabric = matches!(url_scheme, OutputUrlScheme::Rtmp | OutputUrlScheme::Rtmps);
        let rtmp_fabric = if use_rtmp_fabric {
            match crate::application::egress_rtmp_fabric::prepare_rtmp_fabric_startup(
                &self.engine,
                output,
                &prepared,
            )
            .await
            {
                Ok(startup) => {
                    let feed =
                        crate::application::egress_rtmp_fabric::prepare_rtmp_fabric_feed(&prepared);
                    let mut spec = crate::application::egress_rtmp_fabric::rtmp_fabric_output_spec(
                        output,
                        registration.attempt_id,
                        feed.feed_id.clone(),
                    );
                    let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    if let Some(sink) = self
                        .engine
                        .with_active_egress(&output.id, |egress| {
                            crate::media::egress::leaf::EgressProgressSink {
                                bytes_sent: Some(egress.bytes_sent.clone()),
                                metrics: Some(egress.metrics.clone()),
                                last_progress_ms: Some(egress.last_progress_ms.clone()),
                                resync_count: Some(egress.resync_count.clone()),
                                feed_lag_units: Some(egress.feed_lag_units.clone()),
                                backpressure_reason: Some(egress.backpressure_reason.clone()),
                                quality: Some(egress.quality.clone()),
                                terminated_unexpectedly: Some(terminated.clone()),
                            }
                        })
                        .await
                    {
                        spec.progress = sink;
                    }
                    Some(RtmpFabricTask {
                        spec,
                        feed_id: feed.feed_id,
                        feed: feed.feed,
                        startup: startup.into(),
                        terminated,
                    })
                }
                Err(error) => {
                    self.engine
                        .record_egress_error_if_current(
                            &output.id,
                            &registration,
                            "rtmp_fabric_startup",
                            error,
                        )
                        .await;
                    None
                }
            }
        } else {
            None
        };

        let srt_fabric = if use_srt_fabric {
            let feed = crate::application::egress::prepare_srt_fabric_feed(
                &self.engine,
                output,
                &prepared,
                registration.attempt_id,
            )
            .await;
            let mut spec = crate::application::egress::srt_fabric_output_spec(
                output,
                registration.attempt_id,
                feed.feed_id.clone(),
                std::time::Duration::from_millis(self.engine.config.srt_connect_timeout_ms),
            );
            let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
            if let Some(sink) = self
                .engine
                .with_active_egress(&output.id, |egress| {
                    crate::media::egress::leaf::EgressProgressSink {
                        bytes_sent: Some(egress.bytes_sent.clone()),
                        metrics: Some(egress.metrics.clone()),
                        last_progress_ms: Some(egress.last_progress_ms.clone()),
                        resync_count: Some(egress.resync_count.clone()),
                        feed_lag_units: Some(egress.feed_lag_units.clone()),
                        backpressure_reason: Some(egress.backpressure_reason.clone()),
                        quality: Some(egress.quality.clone()),
                        terminated_unexpectedly: Some(terminated.clone()),
                    }
                })
                .await
            {
                spec.progress = sink;
            }
            Some(SrtFabricTask {
                spec,
                feed_id: feed.feed_id,
                feed: feed.feed,
                terminated,
            })
        } else {
            None
        };

        let use_sink_fabric = matches!(url_scheme, OutputUrlScheme::Sink);
        let sink_fabric = if use_sink_fabric {
            let feed = crate::application::egress::prepare_sink_fabric_feed(output, &prepared);
            let mut spec = crate::application::egress::sink_fabric_output_spec(
                output,
                registration.attempt_id,
                feed.feed_id.clone(),
            );
            let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
            if let Some(sink) = self
                .engine
                .with_active_egress(&output.id, |egress| {
                    crate::media::egress::leaf::EgressProgressSink {
                        bytes_sent: Some(egress.bytes_sent.clone()),
                        metrics: Some(egress.metrics.clone()),
                        last_progress_ms: Some(egress.last_progress_ms.clone()),
                        resync_count: Some(egress.resync_count.clone()),
                        feed_lag_units: Some(egress.feed_lag_units.clone()),
                        backpressure_reason: Some(egress.backpressure_reason.clone()),
                        terminated_unexpectedly: Some(terminated.clone()),
                        ..Default::default()
                    }
                })
                .await
            {
                spec.progress = sink;
            }
            Some(SinkFabricTask {
                spec,
                feed_id: feed.feed_id,
                feed: feed.feed,
                terminated,
            })
        } else {
            None
        };

        let use_pipeline_fabric = matches!(url_scheme, OutputUrlScheme::Pipeline);
        let pipeline_fabric = if use_pipeline_fabric {
            // A parse failure here just leaves `pipeline_fabric` `None` —
            // the legacy fallback path re-parses the URL itself and
            // records the same error, so nothing is lost by not
            // duplicating that here.
            match crate::domain::output_spec::RecirculationTarget::parse(&output.url) {
                Ok(target) => {
                    let feed = crate::application::egress::prepare_recirculation_fabric_feed(
                        output, &prepared,
                    );
                    let mut spec = crate::application::egress::recirculation_fabric_output_spec(
                        output,
                        registration.attempt_id,
                        feed.feed_id.clone(),
                        &target,
                    );
                    let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
                    if let Some(sink) = self
                        .engine
                        .with_active_egress(&output.id, |egress| {
                            crate::media::egress::leaf::EgressProgressSink {
                                bytes_sent: Some(egress.bytes_sent.clone()),
                                metrics: Some(egress.metrics.clone()),
                                last_progress_ms: Some(egress.last_progress_ms.clone()),
                                resync_count: Some(egress.resync_count.clone()),
                                feed_lag_units: Some(egress.feed_lag_units.clone()),
                                backpressure_reason: Some(egress.backpressure_reason.clone()),
                                terminated_unexpectedly: Some(terminated.clone()),
                                ..Default::default()
                            }
                        })
                        .await
                    {
                        spec.progress = sink;
                    }
                    Some(PipelineFabricTask {
                        spec,
                        feed_id: feed.feed_id,
                        feed: feed.feed,
                        target_pipeline_id: target.pipeline_id().to_string(),
                        target_input_id: target.input_id().to_string(),
                        terminated,
                    })
                }
                Err(_) => None,
            }
        } else {
            None
        };

        let is_fabric = rtmp_fabric.is_some()
            || srt_fabric.is_some()
            || sink_fabric.is_some()
            || pipeline_fabric.is_some();
        let shard_id = is_fabric.then(|| {
            let shard_count = std::num::NonZeroU32::new(self.engine.config.egress_fabric.shards)
                .unwrap_or(std::num::NonZeroU32::new(1).expect("1 is nonzero"));
            crate::media::egress::manager::assign_output_to_shard(
                &crate::media::egress::OutputId::new(output.id.clone()),
                shard_count,
            )
            .index()
        });
        self.engine
            .set_egress_fabric_attribution(&output.id, is_fabric, shard_id)
            .await;

        let task = EgressTask {
            output_id: output.id.clone(),
            pipeline_id: output.pipeline_id.clone(),
            url: output.url.clone(),
            ring: prepared.ring,
            terminal_stage_key: prepared.terminal_stage_key,
            registration,
            engine: self.engine.clone(),
            pool: self.pool.clone(),
            last_failed: self.last_failed.clone(),
            tuning: self.tuning,
            correlation_id,
            job_id,
            srt_fabric,
            rtmp_fabric,
            sink_fabric,
            pipeline_fabric,
        };
        tokio::spawn(task.run());
    }
}

pub(super) fn next_output_job_id(output_id: &str) -> String {
    format!(
        "job_{output_id}_{}",
        crate::logging::next_correlation_id("attempt")
    )
}
