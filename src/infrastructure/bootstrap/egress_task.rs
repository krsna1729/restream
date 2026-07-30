//! The per-output egress task: `EgressTask::run` dispatches to the
//! fabric-backed path for each protocol when available (falling back to
//! the legacy per-output task otherwise), and owns the shared
//! retry/backoff/status bookkeeping every path funnels back through.
//! Split out of `egress.rs` (which owns `EgressReconciler` and output
//! start-up preparation) purely to stay under the source-audit line cap —
//! not a module-boundary change.

use std::sync::Arc;
use std::time::Instant;

use futures_util::FutureExt as _;
use sqlx::SqlitePool;
use tracing::{error, warn};

use crate::application::models::JobStatus;
use crate::application::reconcile::next_output_retry_count;
use crate::config::RuntimeTuning;
use crate::domain::output_spec::OutputUrlScheme;
use crate::domain::state::EgressPhase;
use crate::media::egress::journal::{RingFeed, TsFeed};
use crate::media::egress::{EgressCommand, FeedId, OutputSpec};
use crate::media::engine::MediaEngine;
use crate::secret_display::redact_url;

use super::FailureTracker;

#[derive(Clone)]
pub(super) struct SrtFabricTask {
    pub(super) feed_id: FeedId,
    pub(super) feed: Arc<TsFeed>,
    pub(super) spec: OutputSpec,
    /// Set by the shard when the fabric closes this leaf for a reason the
    /// application did not request (peer closed, protocol failure, or
    /// stall recovery) — see `EgressProgressSink::terminated_unexpectedly`.
    /// `run_srt_fabric` polls this so it can return and let the shared
    /// retry/backoff bookkeeping in `EgressTask::run` run, exactly as it
    /// does when the legacy per-output task's own I/O loop returns.
    pub(super) terminated: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
pub(super) struct RtmpFabricTask {
    pub(super) feed_id: FeedId,
    pub(super) feed: Arc<RingFeed>,
    pub(super) spec: OutputSpec,
    pub(super) startup: crate::media::egress::backends::rtmp::RtmpPublishStartup,
    /// See `SrtFabricTask::terminated`.
    pub(super) terminated: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
pub(super) struct SinkFabricTask {
    pub(super) feed_id: FeedId,
    pub(super) feed: Arc<RingFeed>,
    pub(super) spec: OutputSpec,
    /// See `SrtFabricTask::terminated`.
    pub(super) terminated: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
pub(super) struct PipelineFabricTask {
    pub(super) feed_id: FeedId,
    pub(super) feed: Arc<RingFeed>,
    pub(super) spec: OutputSpec,
    pub(super) target_pipeline_id: String,
    pub(super) target_input_id: String,
    /// See `SrtFabricTask::terminated`.
    pub(super) terminated: Arc<std::sync::atomic::AtomicBool>,
}

pub(super) struct EgressTask {
    pub(super) output_id: String,
    pub(super) pipeline_id: String,
    pub(super) encoding: String,
    pub(super) url: String,
    pub(super) rtmp_mode: crate::domain::output_spec::RtmpOutputMode,
    pub(super) ring: Arc<crate::media::ring_buffer::RingBuffer>,
    pub(super) terminal_stage_key: crate::domain::stage::StageKey,
    pub(super) registration: crate::media::engine::EgressRegistration,
    pub(super) engine: Arc<MediaEngine>,
    pub(super) pool: SqlitePool,
    pub(super) last_failed: FailureTracker,
    pub(super) tuning: RuntimeTuning,
    pub(super) correlation_id: String,
    pub(super) job_id: String,
    pub(super) srt_fabric: Option<SrtFabricTask>,
    pub(super) rtmp_fabric: Option<RtmpFabricTask>,
    pub(super) sink_fabric: Option<SinkFabricTask>,
    pub(super) pipeline_fabric: Option<PipelineFabricTask>,
}

impl EgressTask {
    pub(super) async fn run(self) {
        let url_scheme = OutputUrlScheme::from_url(&self.url);
        if !url_scheme.is_supported_output() {
            self.reject_unsupported_url().await;
            return;
        }

        let mut hls_persistent_registered = false;
        let panicked = std::panic::AssertUnwindSafe(async {
            match url_scheme {
                OutputUrlScheme::Rtmp | OutputUrlScheme::Rtmps => {
                    if let Some(fabric) = self.rtmp_fabric.as_ref().cloned() {
                        self.run_rtmp_fabric(fabric).await;
                    } else {
                        crate::media::rtmp::start_rtmp_egress(
                            self.output_id.clone(),
                            self.pipeline_id.clone(),
                            self.url.clone(),
                            self.ring.clone(),
                            self.engine.clone(),
                            self.registration.clone(),
                            self.rtmp_mode,
                        )
                        .await;
                    }
                }
                OutputUrlScheme::Srt => {
                    if let Some(fabric) = self.srt_fabric.as_ref().cloned() {
                        self.run_srt_fabric(fabric).await;
                    } else {
                        crate::media::srt::start_srt_egress(
                            self.output_id.clone(),
                            self.pipeline_id.clone(),
                            self.encoding.clone(),
                            self.url.clone(),
                            self.ring.clone(),
                            self.engine.clone(),
                            self.registration.clone(),
                        )
                        .await;
                    }
                }
                OutputUrlScheme::Sink => {
                    if let Some(fabric) = self.sink_fabric.as_ref().cloned() {
                        self.run_sink_fabric(fabric).await;
                    } else {
                        crate::media::egress::backends::sink::start_sink_egress(
                            self.output_id.clone(),
                            self.ring.clone(),
                            self.engine.clone(),
                            self.registration.clone(),
                        )
                        .await;
                    }
                }
                OutputUrlScheme::Hls | OutputUrlScheme::Http | OutputUrlScheme::Https => {
                    let (store, already_running) =
                        self.engine.ensure_hls_segmenter(&self.pipeline_id).await;
                    if !already_running {
                        let Some(hls_cancel) =
                            self.engine.get_hls_cancel_token(&self.pipeline_id).await
                        else {
                            warn!(
                                correlation_id = %self.correlation_id,
                                pipeline_id = %self.pipeline_id,
                                output_id = %self.output_id,
                                "HLS segmenter token missing — skipping"
                            );
                            return;
                        };
                        let engine = self.engine.clone();
                        let pipeline_id = self.pipeline_id.clone();
                        let ring = self.ring.clone();
                        let segmenter_store = store.clone();
                        let stage_key = self.terminal_stage_key.clone();
                        tokio::spawn(async move {
                            crate::media::hls::start_hls_segmenter(
                                pipeline_id.clone(),
                                segmenter_store,
                                ring,
                                None,
                                engine.clone(),
                                hls_cancel,
                                crate::media::hls::HlsSegmenterStart {
                                    video_meta_override: None,
                                    planned_stage_key: Some(stage_key),
                                },
                            )
                            .await;
                            engine.shutdown_hls_segmenter(&pipeline_id).await;
                        });
                    }
                    self.engine
                        .add_hls_persistent_consumer(&self.pipeline_id)
                        .await;
                    hls_persistent_registered = true;
                    if matches!(url_scheme, OutputUrlScheme::Http | OutputUrlScheme::Https) {
                        crate::media::hls_upload::start_hls_put_upload(
                            crate::media::hls_upload::HlsUploadStart {
                                output_id: self.output_id.clone(),
                                pipeline_id: self.pipeline_id.clone(),
                                target_url: self.url.clone(),
                                terminal_stage_key: self.terminal_stage_key.clone(),
                            },
                            store,
                            self.engine.clone(),
                            self.registration.clone(),
                        )
                        .await;
                    } else {
                        self.engine
                            .update_egress_phase_if_current(
                                &self.output_id,
                                &self.registration,
                                EgressPhase::Segmenting,
                            )
                            .await;
                        self.registration.cancel_token.cancelled().await;
                    }
                }
                OutputUrlScheme::Pipeline => {
                    if let Some(fabric) = self.pipeline_fabric.as_ref().cloned() {
                        self.run_pipeline_fabric(fabric).await;
                    } else {
                        match crate::domain::output_spec::RecirculationTarget::parse(&self.url) {
                            Ok(target) => {
                                crate::media::recirculation::start_pipeline_recirculation(
                                    self.output_id.clone(),
                                    self.ring.clone(),
                                    target.pipeline_id().to_string(),
                                    target.input_id().to_string(),
                                    self.engine.clone(),
                                    self.registration.clone(),
                                )
                                .await;
                            }
                            Err(error) => {
                                self.engine
                                    .record_egress_error_if_current(
                                        &self.output_id,
                                        &self.registration,
                                        "recirculation_target_parse",
                                        error.message(),
                                    )
                                    .await;
                            }
                        }
                    }
                }
                OutputUrlScheme::Unknown => {}
            }
        })
        .catch_unwind()
        .await
        .is_err();

        if panicked {
            error!(
                correlation_id = %self.correlation_id,
                output_id = %self.output_id,
                pipeline_id = %self.pipeline_id,
                event_class = "lifecycle",
                event_type = "egress.failed",
                failure_reason = "panic",
                "panic in egress task"
            );
        }

        let is_cancelled = self.registration.cancel_token.is_cancelled();
        let had_progress = self
            .engine
            .egress_has_recorded_progress_if_current(&self.output_id, &self.registration)
            .await;

        let retry_backoff = {
            let mut last_failed = self.last_failed.lock().await;
            if is_cancelled {
                last_failed.remove(&self.output_id);
                None
            } else {
                let retries = next_output_retry_count(
                    last_failed
                        .get(&self.output_id)
                        .map(|(_, retries)| *retries),
                    had_progress,
                );
                last_failed.insert(self.output_id.clone(), (Instant::now(), retries));
                (retries < self.tuning.output_max_retries)
                    .then_some((retries, self.tuning.output_backoff_ms(retries)))
            }
        };

        if let Some((retries, backoff_ms)) = retry_backoff {
            self.engine
                .update_egress_retry_state(&self.output_id, retries, backoff_ms, backoff_ms)
                .await;
        } else {
            self.engine.clear_egress_retry_state(&self.output_id).await;
        }

        let _was_current = self
            .engine
            .unregister_egress_if_current(&self.output_id, &self.registration)
            .await;
        if hls_persistent_registered {
            self.engine
                .remove_hls_persistent_consumer(&self.pipeline_id)
                .await;
        }

        let ended_at = chrono::Utc::now().to_rfc3339();
        let job_status = if is_cancelled {
            JobStatus::Stopped
        } else {
            JobStatus::Failed
        };
        let _ = crate::db::update_job(
            &self.pool,
            &self.job_id,
            None,
            Some(job_status),
            Some(&ended_at),
            Some(0),
            None,
        )
        .await;
    }

    /// Waits for either explicit cancellation (normal stop/reconfigure) or
    /// the fabric marking this output's leaf as unexpectedly terminated
    /// (peer closed, protocol failure, or stall recovery — see
    /// `EgressProgressSink::terminated_unexpectedly`). Returns `true` only
    /// for the unexpected-termination case, so the caller can surface it as
    /// an error before falling through to the shared retry/backoff
    /// bookkeeping in `EgressTask::run`. Polls rather than a wake-based
    /// signal: this only runs once per output for as long as it's healthy,
    /// nowhere near the packet-level hot path.
    async fn wait_for_stop_or_leaf_failure(
        &self,
        terminated: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> bool {
        loop {
            if terminated.load(std::sync::atomic::Ordering::Relaxed) {
                return true;
            }
            tokio::select! {
                _ = self.registration.cancel_token.cancelled() => return false,
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            }
        }
    }

    async fn run_srt_fabric(&self, fabric: SrtFabricTask) {
        match self
            .engine
            .retain_srt_fabric_runtime(fabric.feed_id.clone(), fabric.feed.as_ref())
            .await
        {
            Ok(_) => {}
            Err(error) => {
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        "srt_fabric_ensure",
                        format!("{error:?}"),
                    )
                    .await;
                return;
            }
        }

        match self
            .engine
            .dispatch_srt_fabric_command(&fabric.feed_id, EgressCommand::Add(fabric.spec.clone()))
            .await
        {
            Ok(_) => {
                self.engine
                    .update_egress_phase_if_current(
                        &self.output_id,
                        &self.registration,
                        EgressPhase::Sending,
                    )
                    .await;
            }
            Err(error) => {
                let _ = self
                    .engine
                    .release_srt_fabric_runtime(&fabric.feed_id)
                    .await;
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        "srt_fabric_dispatch",
                        format!("{error:?}"),
                    )
                    .await;
                return;
            }
        }

        if self.wait_for_stop_or_leaf_failure(&fabric.terminated).await {
            self.engine
                .record_egress_error_if_current(
                    &self.output_id,
                    &self.registration,
                    "srt_fabric_leaf",
                    "SRT fabric leaf terminated unexpectedly (peer closed, protocol failure, or stall recovery)",
                )
                .await;
        }
        let _ = self
            .engine
            .dispatch_srt_fabric_command(&fabric.feed_id, EgressCommand::Remove(fabric.spec.id))
            .await;
        let _ = self
            .engine
            .release_srt_fabric_runtime(&fabric.feed_id)
            .await;
    }

    async fn run_rtmp_fabric(&self, fabric: RtmpFabricTask) {
        match self
            .engine
            .retain_rtmp_fabric_runtime(fabric.feed_id.clone(), fabric.feed.as_ref())
            .await
        {
            Ok(_) => {}
            Err(error) => {
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        "rtmp_fabric_ensure",
                        format!("{error:?}"),
                    )
                    .await;
                return;
            }
        }

        // Must land before the Add command: the shard thread reads this via
        // RtmpPublishStartupSource::take_startup and never queries anything
        // itself.
        self.engine
            .set_rtmp_publish_startup(
                &fabric.feed_id,
                fabric.spec.id.clone(),
                fabric.startup.clone(),
            )
            .await;

        match self
            .engine
            .dispatch_rtmp_fabric_command(&fabric.feed_id, EgressCommand::Add(fabric.spec.clone()))
            .await
        {
            Ok(_) => {
                self.engine
                    .update_egress_phase_if_current(
                        &self.output_id,
                        &self.registration,
                        EgressPhase::Sending,
                    )
                    .await;
            }
            Err(error) => {
                let _ = self
                    .engine
                    .release_rtmp_fabric_runtime(&fabric.feed_id)
                    .await;
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        "rtmp_fabric_dispatch",
                        format!("{error:?}"),
                    )
                    .await;
                return;
            }
        }

        if self.wait_for_stop_or_leaf_failure(&fabric.terminated).await {
            self.engine
                .record_egress_error_if_current(
                    &self.output_id,
                    &self.registration,
                    "rtmp_fabric_leaf",
                    "RTMP fabric leaf terminated unexpectedly (peer closed, protocol failure, or stall recovery)",
                )
                .await;
        }
        let _ = self
            .engine
            .dispatch_rtmp_fabric_command(&fabric.feed_id, EgressCommand::Remove(fabric.spec.id))
            .await;
        let _ = self
            .engine
            .release_rtmp_fabric_runtime(&fabric.feed_id)
            .await;
    }

    async fn run_sink_fabric(&self, fabric: SinkFabricTask) {
        match self
            .engine
            .retain_sink_fabric_runtime(fabric.feed_id.clone(), fabric.feed.as_ref())
            .await
        {
            Ok(_) => {}
            Err(error) => {
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        "sink_fabric_ensure",
                        format!("{error:?}"),
                    )
                    .await;
                return;
            }
        }

        match self
            .engine
            .dispatch_sink_fabric_command(&fabric.feed_id, EgressCommand::Add(fabric.spec.clone()))
            .await
        {
            Ok(_) => {
                self.engine
                    .update_egress_phase_if_current(
                        &self.output_id,
                        &self.registration,
                        EgressPhase::Discarding,
                    )
                    .await;
            }
            Err(error) => {
                let _ = self
                    .engine
                    .release_sink_fabric_runtime(&fabric.feed_id)
                    .await;
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        "sink_fabric_dispatch",
                        format!("{error:?}"),
                    )
                    .await;
                return;
            }
        }

        if self.wait_for_stop_or_leaf_failure(&fabric.terminated).await {
            self.engine
                .record_egress_error_if_current(
                    &self.output_id,
                    &self.registration,
                    "sink_fabric_leaf",
                    "sink fabric leaf terminated unexpectedly",
                )
                .await;
        }
        let _ = self
            .engine
            .dispatch_sink_fabric_command(&fabric.feed_id, EgressCommand::Remove(fabric.spec.id))
            .await;
        let _ = self
            .engine
            .release_sink_fabric_runtime(&fabric.feed_id)
            .await;
    }

    async fn run_pipeline_fabric(&self, fabric: PipelineFabricTask) {
        match self
            .engine
            .retain_pipeline_fabric_runtime(fabric.feed_id.clone(), fabric.feed.as_ref())
            .await
        {
            Ok(_) => {}
            Err(error) => {
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        "pipeline_fabric_ensure",
                        format!("{error:?}"),
                    )
                    .await;
                return;
            }
        }

        // Claiming the target input is async and fallible — must land
        // before `Add`, and cannot happen on a shard thread (see
        // `PipelineTargetSource`'s doc comment).
        let Some(input_registration) = self
            .engine
            .try_register_pipeline_input_attempt(
                &fabric.target_pipeline_id,
                &fabric.target_input_id,
                &format!("pipeline:{}", self.output_id),
                "pipeline",
                false,
            )
            .await
        else {
            let _ = self
                .engine
                .release_pipeline_fabric_runtime(&fabric.feed_id)
                .await;
            self.engine
                .record_egress_error_if_current(
                    &self.output_id,
                    &self.registration,
                    "recirculation_input_claim",
                    format!(
                        "target input {}/{} is already active",
                        fabric.target_pipeline_id, fabric.target_input_id
                    ),
                )
                .await;
            return;
        };
        let target_ring = self
            .engine
            .get_or_create_pipeline(&fabric.target_pipeline_id)
            .await;

        self.engine
            .update_egress_target_addr_if_current(
                &self.output_id,
                &self.registration,
                format!(
                    "pipeline://{}/{}",
                    fabric.target_pipeline_id, fabric.target_input_id
                ),
            )
            .await;

        self.engine
            .set_pipeline_target(
                &fabric.feed_id,
                fabric.spec.id.clone(),
                crate::media::egress::backends::pipeline::PipelineTarget {
                    target_ring,
                    input_registration: input_registration.clone(),
                },
            )
            .await;

        match self
            .engine
            .dispatch_pipeline_fabric_command(
                &fabric.feed_id,
                EgressCommand::Add(fabric.spec.clone()),
            )
            .await
        {
            Ok(_) => {
                self.engine
                    .update_egress_phase_if_current(
                        &self.output_id,
                        &self.registration,
                        EgressPhase::Sending,
                    )
                    .await;
            }
            Err(error) => {
                let _ = self
                    .engine
                    .release_pipeline_fabric_runtime(&fabric.feed_id)
                    .await;
                self.engine
                    .unregister_ingest_if_current(&fabric.target_pipeline_id, &input_registration)
                    .await;
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        "pipeline_fabric_dispatch",
                        format!("{error:?}"),
                    )
                    .await;
                return;
            }
        }

        if self.wait_for_stop_or_leaf_failure(&fabric.terminated).await {
            self.engine
                .record_egress_error_if_current(
                    &self.output_id,
                    &self.registration,
                    "pipeline_fabric_leaf",
                    "pipeline fabric leaf terminated unexpectedly",
                )
                .await;
        }
        let _ = self
            .engine
            .dispatch_pipeline_fabric_command(
                &fabric.feed_id,
                EgressCommand::Remove(fabric.spec.id),
            )
            .await;
        let _ = self
            .engine
            .release_pipeline_fabric_runtime(&fabric.feed_id)
            .await;
        self.engine
            .unregister_ingest_if_current(&fabric.target_pipeline_id, &input_registration)
            .await;
    }

    async fn reject_unsupported_url(&self) {
        let ended_at = chrono::Utc::now().to_rfc3339();
        let _ = crate::db::update_job(
            &self.pool,
            &self.job_id,
            None,
            Some(JobStatus::Failed),
            Some(&ended_at),
            Some(0),
            None,
        )
        .await;
        let was_current = self
            .engine
            .unregister_egress_if_current(&self.output_id, &self.registration)
            .await;
        let retry_backoff = if was_current {
            let mut last_failed = self.last_failed.lock().await;
            let retries = next_output_retry_count(
                last_failed
                    .get(&self.output_id)
                    .map(|(_, retries)| *retries),
                false,
            );
            last_failed.insert(self.output_id.clone(), (Instant::now(), retries));
            (retries < self.tuning.output_max_retries)
                .then_some((retries, self.tuning.output_backoff_ms(retries)))
        } else {
            None
        };
        if let Some((retries, backoff_ms)) = retry_backoff {
            self.engine
                .update_egress_retry_state(&self.output_id, retries, backoff_ms, backoff_ms)
                .await;
        } else {
            self.engine.clear_egress_retry_state(&self.output_id).await;
        }
        error!(
            correlation_id = %self.correlation_id,
            output_id = %self.output_id,
            pipeline_id = %self.pipeline_id,
            event_class = "lifecycle",
            event_type = "egress.failed",
            failure_reason = "unsupported_url_scheme",
            url = %redact_url(&self.url),
            "output rejected unsupported URL scheme",
        );
    }
}
