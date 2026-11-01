//! The per-output egress task: `EgressTask::run` dispatches by URL scheme
//! to the fabric-backed path (RTMP/RTMPS, SRT, sink discard, pipeline
//! recirculation) or the output type's own dedicated task (HLS
//! segmenting/PUT upload), and owns the shared retry/backoff/status
//! bookkeeping every path funnels back through. Split out of `egress.rs`
//! (which owns `EgressReconciler` and output start-up preparation) purely
//! to stay under the source-audit line cap — not a module-boundary change.

use std::sync::Arc;
use std::time::Instant;

use futures_util::FutureExt as _;
use sqlx::SqlitePool;
use tracing::{error, warn};

use crate::application::reconcile::next_output_retry_count;
use crate::config::RuntimeTuning;
use restream_domain::domain::output_spec::OutputUrlScheme;
use restream_domain::domain::state::EgressPhase;
use restream_domain::models::JobStatus;
use restream_engine::egress::journal::{RingFeed, TsFeed};
use restream_engine::egress::{EgressCommand, FeedId, OutputSpec};
use restream_engine::engine::MediaEngine;
use restream_engine::secret_display::redact_url;
use restream_health::diagnosis::CAPACITY_REFUSED_PHASE;

use super::FailureTracker;

#[path = "egress_moq.rs"]
mod egress_moq;
pub(super) use egress_moq::{MoqFabricTask, prepare_moq_fabric_task};

#[derive(Clone)]
pub(super) struct SrtFabricTask {
    pub(super) feed_id: FeedId,
    pub(super) feed: Arc<TsFeed>,
    /// The ring the feed's muxer reads (`retain_srt_fabric_runtime`).
    pub(super) source: Arc<restream_sync::RingBuffer>,
    pub(super) spec: OutputSpec,
    /// Set by the shard when the fabric closes this leaf for a reason the
    /// application did not request (peer closed, protocol failure, or
    /// stall recovery) — see `EgressProgressSink::terminated_unexpectedly`.
    /// `run_srt_fabric` polls this so it can return and let the shared
    /// retry/backoff bookkeeping in `EgressTask::run` run.
    pub(super) terminated: Arc<std::sync::atomic::AtomicBool>,
}

#[derive(Clone)]
pub(super) struct RtmpFabricTask {
    pub(super) feed_id: FeedId,
    pub(super) feed: Arc<RingFeed>,
    pub(super) spec: OutputSpec,
    pub(super) startup: restream_engine::egress::backends::rtmp::RtmpPublishStartup,
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

/// An HTTP/HTTPS HLS output uploaded from the HLS PUT fabric. The store it
/// reads is known only once `EgressTask::run` has started the segmenter.
#[derive(Clone)]
pub(super) struct HlsPutFabricTask {
    pub(super) spec: OutputSpec,
    /// See `SrtFabricTask::terminated`.
    pub(super) terminated: Arc<std::sync::atomic::AtomicBool>,
    /// The leaf's per-request status, forwarded to the engine while the
    /// output runs (see `forward_upload_status`).
    pub(super) upload_status: Arc<std::sync::Mutex<restream_engine::egress::leaf::UploadStatus>>,
}

/// What `forward_upload_status` already passed on to the engine.
#[derive(Default)]
struct ForwardedUploadStatus {
    failures: u64,
    retry: Option<(u32, u64)>,
    /// A failure was reported and no recovery since.
    failing: bool,
}

pub(super) struct EgressTask {
    pub(super) output_id: String,
    pub(super) pipeline_id: String,
    pub(super) url: String,
    pub(super) ring: Arc<restream_sync::RingBuffer>,
    pub(super) terminal_stage_key: restream_domain::domain::stage::StageKey,
    pub(super) registration: restream_engine::engine::EgressRegistration,
    pub(super) engine: Arc<MediaEngine>,
    pub(super) pool: SqlitePool,
    pub(super) last_failed: FailureTracker,
    /// Leases on the shared stages this output reads through: they stop
    /// when the last egress (or other consumer) using them ends. A pipeline
    /// HLS segmenter this output starts holds clones while it reads the ring.
    pub(super) stages: Vec<restream_engine::stage_lease::StageLease>,
    pub(super) tuning: RuntimeTuning,
    pub(super) correlation_id: String,
    pub(super) job_id: String,
    pub(super) srt_fabric: Option<SrtFabricTask>,
    pub(super) rtmp_fabric: Option<RtmpFabricTask>,
    pub(super) sink_fabric: Option<SinkFabricTask>,
    pub(super) pipeline_fabric: Option<PipelineFabricTask>,
    pub(super) hls_put_fabric: Option<HlsPutFabricTask>,
    pub(super) moq_fabric: Option<MoqFabricTask>,
}

impl EgressTask {
    /// Attributes this attempt to the shard the fabric placed its `Add` on.
    async fn record_fabric_placement(
        &self,
        feed: &FeedId,
        outcome: &restream_engine::egress::manager::ManagerCommandOutcome,
    ) {
        if let Some(shard) = outcome.placed_shard() {
            self.engine
                .set_egress_fabric_attribution(
                    &self.output_id,
                    &self.registration,
                    restream_engine::engine::FabricSlot {
                        feed: feed.clone(),
                        shard: shard.index(),
                    },
                )
                .await;
        }
    }

    pub(super) async fn run(self) {
        let url_scheme = OutputUrlScheme::from_url(&self.url);
        if !url_scheme.is_supported_output() {
            self.reject_unsupported_url().await;
            return;
        }

        // Held for the whole run, panics included; released (dropped) after
        // the output is unregistered, as before.
        let mut hls_lease: Option<restream_engine::engine_hls::PersistentLease> = None;
        let panicked = std::panic::AssertUnwindSafe(async {
            match url_scheme {
                OutputUrlScheme::Rtmp | OutputUrlScheme::Rtmps => {
                    if let Some(fabric) = self.rtmp_fabric.as_ref().cloned() {
                        self.run_rtmp_fabric(fabric).await;
                    }
                }
                OutputUrlScheme::Srt => {
                    if let Some(fabric) = self.srt_fabric.as_ref().cloned() {
                        self.run_srt_fabric(fabric).await;
                    }
                }
                OutputUrlScheme::Sink => {
                    if let Some(fabric) = self.sink_fabric.as_ref().cloned() {
                        self.run_sink_fabric(fabric).await;
                    } else {
                        restream_engine::egress::backends::sink::start_sink_egress(
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
                        crate::infrastructure::bootstrap::hls_segmenter::spawn_pipeline_hls_segmenter(
                            self.engine.clone(),
                            self.pipeline_id.clone(),
                            store.clone(),
                            self.ring.clone(),
                            hls_cancel,
                            self.terminal_stage_key.clone(),
                            self.stages.clone(),
                        );
                    }
                    hls_lease = self
                        .engine
                        .lease_hls_persistent_consumer(&self.pipeline_id)
                        .await;
                    if let Some(fabric) = self.hls_put_fabric.clone() {
                        self.run_hls_put_fabric(fabric, store).await;
                    } else if matches!(url_scheme, OutputUrlScheme::Http | OutputUrlScheme::Https) {
                        restream_engine::hls_upload::start_hls_put_upload(
                            restream_engine::hls_upload::HlsUploadStart {
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
                        tokio::select! {
                            _ = self.registration.cancel_token.cancelled() => {}
                            () = self.own_stage_ended() => {}
                        }
                    }
                }
                OutputUrlScheme::Pipeline => {
                    if let Some(fabric) = self.pipeline_fabric.as_ref().cloned() {
                        self.run_pipeline_fabric(fabric).await;
                    } else {
                        match restream_domain::domain::output_spec::RecirculationTarget::parse(&self.url) {
                            Ok(target) => {
                                restream_engine::recirculation::start_pipeline_recirculation(
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
                OutputUrlScheme::Moqt => {
                    if let Some(fabric) = self.moq_fabric.as_ref().cloned() {
                        self.run_moq_fabric(fabric).await;
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
                (retries < self.tuning.output_max_retries).then_some((
                    retries,
                    self.tuning.output_backoff_ms(retries, &self.output_id),
                ))
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
        drop(hls_lease);

        let ended_at = chrono::Utc::now().to_rfc3339();
        let job_status = if is_cancelled {
            JobStatus::Stopped
        } else {
            JobStatus::Failed
        };
        let _ = crate::db::update_job(&self.pool, &self.job_id, Some(job_status), Some(&ended_at))
            .await;
    }

    /// Waits for explicit cancellation (normal stop/reconfigure), the fabric
    /// marking this output's leaf as unexpectedly terminated (peer closed,
    /// protocol failure, or stall recovery — see
    /// `EgressProgressSink::terminated_unexpectedly`), or a stage this output
    /// reads ending on its own. Returns `true` only for the leaf case, so
    /// the caller can surface it as an error before falling through to the
    /// shared retry/backoff bookkeeping in `EgressTask::run`; a stage end
    /// records its own error and also retries. Polls the leaf flag rather
    /// than a wake-based signal: this only runs once per output for as long
    /// as it's healthy, nowhere near the packet-level hot path.
    async fn wait_for_stop_or_leaf_failure(
        &self,
        terminated: &std::sync::Arc<std::sync::atomic::AtomicBool>,
    ) -> bool {
        let stage_ended = self.own_stage_ended();
        tokio::pin!(stage_ended);
        loop {
            if terminated.load(std::sync::atomic::Ordering::Relaxed) {
                return true;
            }
            tokio::select! {
                _ = self.registration.cancel_token.cancelled() => return false,
                () = &mut stage_ended => return false,
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            }
        }
    }

    /// Completes once a stage this output reads ends on its own, after
    /// recording that as the attempt's error. Such a stage is never fed or
    /// reused again, so the attempt ends and its retry starts a new one.
    async fn own_stage_ended(&self) {
        restream_engine::stage_lease::StageLease::any_ended(&self.stages).await;
        self.engine
            .record_egress_error_if_current(
                &self.output_id,
                &self.registration,
                "stage_ended",
                "a stage this output reads ended; the next attempt starts it again",
            )
            .await;
    }

    async fn run_srt_fabric(&self, fabric: SrtFabricTask) {
        match self
            .engine
            .retain_srt_fabric_runtime(
                fabric.feed_id.clone(),
                fabric.feed.as_ref(),
                fabric.source.as_ref(),
            )
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
            Ok(outcome) => {
                self.record_fabric_placement(&fabric.spec.feed, &outcome)
                    .await;
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
                let (phase, message) = match error.refused() {
                    Some(saturated) => (CAPACITY_REFUSED_PHASE, saturated.to_string()),
                    None => ("srt_fabric_dispatch", format!("{error:?}")),
                };
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        phase,
                        message,
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
            Ok(outcome) => {
                self.record_fabric_placement(&fabric.spec.feed, &outcome)
                    .await;
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
                let (phase, message) = match error.refused() {
                    Some(saturated) => (CAPACITY_REFUSED_PHASE, saturated.to_string()),
                    None => ("rtmp_fabric_dispatch", format!("{error:?}")),
                };
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        phase,
                        message,
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

    async fn run_hls_put_fabric(
        &self,
        fabric: HlsPutFabricTask,
        store: Arc<restream_engine::hls::HlsStore>,
    ) {
        let feed_id = restream_engine::engine_hls_egress_fabric::hls_put_feed_id(&self.pipeline_id);
        if let Err(error) = self
            .engine
            .retain_hls_put_fabric_runtime(feed_id.clone(), &store)
            .await
        {
            self.engine
                .record_egress_error_if_current(
                    &self.output_id,
                    &self.registration,
                    "hls_put_fabric_ensure",
                    format!("{error:?}"),
                )
                .await;
            return;
        }
        drop(store);
        match self
            .engine
            .dispatch_hls_put_fabric_command(&feed_id, EgressCommand::Add(fabric.spec.clone()))
            .await
        {
            Ok(outcome) => {
                self.record_fabric_placement(&fabric.spec.feed, &outcome)
                    .await;
                self.engine
                    .update_egress_phase_if_current(
                        &self.output_id,
                        &self.registration,
                        EgressPhase::Uploading,
                    )
                    .await;
            }
            Err(error) => {
                let _ = self.engine.release_hls_put_fabric_runtime(&feed_id).await;
                let (phase, message) = match error.refused() {
                    Some(saturated) => (CAPACITY_REFUSED_PHASE, saturated.to_string()),
                    None => ("hls_put_fabric_dispatch", format!("{error:?}")),
                };
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        phase,
                        message,
                    )
                    .await;
                return;
            }
        }
        let mut forwarded = ForwardedUploadStatus::default();
        let own_stage = self.own_stage_ended();
        tokio::pin!(own_stage);
        let (failed, stage_ended) = loop {
            self.forward_upload_status(&fabric.upload_status, &mut forwarded)
                .await;
            if fabric.terminated.load(std::sync::atomic::Ordering::Acquire) {
                self.forward_upload_status(&fabric.upload_status, &mut forwarded)
                    .await;
                break (true, false);
            }
            tokio::select! {
                _ = self.registration.cancel_token.cancelled() => break (false, false),
                () = &mut own_stage => break (false, true),
                _ = tokio::time::sleep(std::time::Duration::from_millis(250)) => {}
            }
        };
        // A rejection was already reported with its status and object.
        if failed && forwarded.failures == 0 {
            self.engine
                .record_egress_error_if_current(
                    &self.output_id,
                    &self.registration,
                    "hls_put_fabric_leaf",
                    "HLS ingest rejected the upload or the leaf failed",
                )
                .await;
        }
        // The leaf sends its end playlist after a remove. A stage that ended
        // under the output is no intentional stop: abandon the session, so
        // the destination keeps the stream open for the retry.
        let command = if stage_ended {
            EgressCommand::Abandon(fabric.spec.id)
        } else {
            EgressCommand::Remove(fabric.spec.id)
        };
        let _ = self
            .engine
            .dispatch_hls_put_fabric_command(&feed_id, command)
            .await;
        let _ = self.engine.release_hls_put_fabric_runtime(&feed_id).await;
    }

    /// Pass the leaf's new failures and retry changes to the engine, as the
    /// Tokio uploader reports them: each failure once (the latest, when
    /// several landed between polls), the retry state when it changes.
    async fn forward_upload_status(
        &self,
        status: &std::sync::Mutex<restream_engine::egress::leaf::UploadStatus>,
        forwarded: &mut ForwardedUploadStatus,
    ) {
        let (failure, retry, healthy) = {
            let status = restream_sync::lock::lock(status);
            let failure = (status.failures > forwarded.failures)
                .then(|| status.last_failure.clone())
                .flatten();
            forwarded.failures = status.failures;
            (failure, status.retry, status.healthy)
        };
        if let Some((phase, message)) = failure {
            self.engine
                .record_egress_error_if_current(&self.output_id, &self.registration, phase, message)
                .await;
            forwarded.failing = true;
        }
        if healthy && forwarded.failing {
            forwarded.failing = false;
            self.engine
                .mark_egress_recovered_if_current(&self.output_id, &self.registration)
                .await;
        }
        if retry != forwarded.retry {
            forwarded.retry = retry;
            match retry {
                Some((attempts, backoff_ms)) => {
                    self.engine
                        .update_egress_retry_state_if_current(
                            &self.output_id,
                            &self.registration,
                            attempts,
                            backoff_ms,
                            backoff_ms,
                        )
                        .await;
                }
                None => self.engine.clear_egress_retry_state(&self.output_id).await,
            }
        }
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
            Ok(outcome) => {
                self.record_fabric_placement(&fabric.spec.feed, &outcome)
                    .await;
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
                let (phase, message) = match error.refused() {
                    Some(saturated) => (CAPACITY_REFUSED_PHASE, saturated.to_string()),
                    None => ("sink_fabric_dispatch", format!("{error:?}")),
                };
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        phase,
                        message,
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
                restream_engine::egress::backends::pipeline::PipelineTarget {
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
            Ok(outcome) => {
                self.record_fabric_placement(&fabric.spec.feed, &outcome)
                    .await;
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
                let (phase, message) = match error.refused() {
                    Some(saturated) => (CAPACITY_REFUSED_PHASE, saturated.to_string()),
                    None => ("pipeline_fabric_dispatch", format!("{error:?}")),
                };
                self.engine
                    .record_egress_error_if_current(
                        &self.output_id,
                        &self.registration,
                        phase,
                        message,
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
            Some(JobStatus::Failed),
            Some(&ended_at),
        )
        .await;
        // Record the failure before the egress is gone: its stop event wakes
        // the reconciler, which must see the failure and wait out the backoff
        // (it reads `last_failed` under this lock).
        let mut last_failed = self.last_failed.lock().await;
        let was_current = self
            .engine
            .unregister_egress_if_current(&self.output_id, &self.registration)
            .await;
        let retry_backoff = if was_current {
            let retries = next_output_retry_count(
                last_failed
                    .get(&self.output_id)
                    .map(|(_, retries)| *retries),
                false,
            );
            last_failed.insert(self.output_id.clone(), (Instant::now(), retries));
            (retries < self.tuning.output_max_retries).then_some((
                retries,
                self.tuning.output_backoff_ms(retries, &self.output_id),
            ))
        } else {
            None
        };
        drop(last_failed);
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
