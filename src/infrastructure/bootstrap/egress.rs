use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use futures_util::FutureExt as _;
use sqlx::SqlitePool;
use tokio::sync::Mutex;
use tracing::{error, info, warn};

use crate::application::models::{JobStatus, Output};
use crate::application::reconcile::{
    OutputFailureWindow, OutputStartAction, OutputStopAction, decide_output_start_action,
    decide_output_stop_action, load_output_runtime_snapshot, next_output_retry_count,
};
use crate::config::RuntimeTuning;
use crate::domain::output_spec::OutputUrlScheme;
use crate::domain::state::{DesiredOutputState, EgressPhase};
use crate::media::engine::MediaEngine;
use crate::secret_display::redact_url;

type FailureTracker = Arc<Mutex<HashMap<String, (Instant, u32)>>>;

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
                    self.last_failed.lock().await.remove(&output.id);
                    self.engine.clear_egress_retry_state(&output.id).await;
                    warn!(
                        correlation_id = %crate::logging::next_correlation_id("out"),
                        output_id = %output.id,
                        output_name = %output.name,
                        max_retries = self.tuning.output_max_retries,
                        "output exceeded max retries — marking failed",
                    );
                    let _ = crate::db::set_output_desired_state(
                        &self.pool,
                        &output.pipeline_id,
                        &output.id,
                        DesiredOutputState::Failed,
                    )
                    .await;
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

        let prepared = crate::application::egress::prepare_output_ring(&self.engine, output).await;
        let encoding = output.stage_encoding_label();
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

        let task = EgressTask {
            output_id: output.id.clone(),
            pipeline_id: output.pipeline_id.clone(),
            encoding,
            url: output.url.clone(),
            rtmp_mode: output.config.rtmp_mode(),
            ring: prepared.ring,
            terminal_stage_key: prepared.terminal_stage_key,
            registration,
            engine: self.engine.clone(),
            pool: self.pool.clone(),
            last_failed: self.last_failed.clone(),
            tuning: self.tuning,
            correlation_id,
            job_id,
        };
        tokio::spawn(task.run());
    }
}

struct EgressTask {
    output_id: String,
    pipeline_id: String,
    encoding: String,
    url: String,
    rtmp_mode: crate::domain::output_spec::RtmpOutputMode,
    ring: Arc<crate::media::ring_buffer::RingBuffer>,
    terminal_stage_key: crate::domain::stage::StageKey,
    registration: crate::media::engine::EgressRegistration,
    engine: Arc<MediaEngine>,
    pool: SqlitePool,
    last_failed: FailureTracker,
    tuning: RuntimeTuning,
    correlation_id: String,
    job_id: String,
}

impl EgressTask {
    async fn run(self) {
        let url_scheme = OutputUrlScheme::from_url(&self.url);
        if !url_scheme.is_supported_output() {
            self.reject_unsupported_url().await;
            return;
        }

        let mut hls_persistent_registered = false;
        let panicked = std::panic::AssertUnwindSafe(async {
            match url_scheme {
                OutputUrlScheme::Rtmp | OutputUrlScheme::Rtmps => {
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
                OutputUrlScheme::Srt => {
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
        let was_current = self
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

        let retry_backoff = if was_current {
            let mut last_failed = self.last_failed.lock().await;
            if is_cancelled {
                last_failed.remove(&self.output_id);
            } else {
                let retries = next_output_retry_count(
                    last_failed
                        .get(&self.output_id)
                        .map(|(_, retries)| *retries),
                    had_progress,
                );
                last_failed.insert(self.output_id.clone(), (Instant::now(), retries));
            }
            if is_cancelled {
                None
            } else {
                last_failed
                    .get(&self.output_id)
                    .map(|(_, retries)| *retries)
                    .and_then(|retries| {
                        (retries < self.tuning.output_max_retries)
                            .then_some((retries, self.tuning.output_backoff_ms(retries)))
                    })
            }
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

pub(super) fn next_output_job_id(output_id: &str) -> String {
    format!(
        "job_{output_id}_{}",
        crate::logging::next_correlation_id("attempt")
    )
}
