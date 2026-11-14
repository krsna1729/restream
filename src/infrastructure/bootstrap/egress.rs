//! Output start, stop and retry. A pass decides every output in memory and
//! hands each output's slow work to a task of its own: a start (rings,
//! stages, the MoQ CA file, the egress task) and a retries-exhausted mark.
//! The pass leaves an output alone while its task runs, and the task's end
//! wakes the next pass, so one output's slow start or write never holds the
//! pass for the others. The job row is written off the start path entirely
//! ([`AttemptJob`]). Each output is planned behind a panic boundary of its
//! own ([`PlanningPanics`]): one whose planning panics backs off, and the
//! pass goes on to the rest.

use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::FutureExt as _;
use tokio::sync::Mutex;
use tokio_util::task::TaskTracker;
use tracing::{info, warn};

use crate::application::reconcile::{
    OutputFailureWindow, OutputRetryPolicy, OutputStartAction, OutputStopAction,
    decide_output_start_action, decide_output_stop_action, load_output_runtime_snapshot,
};
use crate::application::runtime_state::hub::StateHub;
use crate::application::topology::ReconcileRequests;
use crate::config::RuntimeTuning;
use crate::db::DeadlineDb;
use restream_domain::domain::output_spec::OutputUrlScheme;
use restream_domain::domain::state::DesiredOutputState;
use restream_domain::models::{JobStatus, Output};
use restream_engine::engine::MediaEngine;

#[path = "egress_task.rs"]
mod egress_task;
use egress_task::{
    EgressTask, HlsPutFabricTask, PipelineFabricTask, RtmpFabricTask, SinkFabricTask,
    SrtFabricTask, prepare_moq_fabric_task,
};
mod planning_panics;
use planning_panics::PlanningPanics;

pub(super) type FailureTracker = Arc<Mutex<HashMap<String, (Instant, u32)>>>;

/// How a pass decides whether to start an output; production's is
/// [`decide_output_start_action`].
type DecideStart = fn(
    DesiredOutputState,
    bool,
    bool,
    bool,
    bool,
    Option<OutputFailureWindow>,
    OutputRetryPolicy,
    &str,
) -> OutputStartAction;

pub(super) struct EgressReconciler {
    starter: EgressStarter,
    /// Marked when this reconciler writes a row the projection reads
    /// (retries exhausted), as API writes and engine events are.
    runtime_state: Arc<StateHub>,
    requests: Arc<ReconcileRequests>,
    /// Outputs whose start or retries-exhausted mark runs in its own task.
    in_flight: InFlight,
    tasks: TaskTracker,
    planning_panics: PlanningPanics,
    decide_start: DecideStart,
}

/// What an output's start needs, cloned into its task.
#[derive(Clone)]
struct EgressStarter {
    engine: Arc<MediaEngine>,
    db: DeadlineDb,
    tuning: RuntimeTuning,
    last_failed: FailureTracker,
}

/// Output ids with a task running for them.
#[derive(Clone, Default)]
struct InFlight(Arc<std::sync::Mutex<HashSet<String>>>);

impl InFlight {
    fn contains(&self, output_id: &str) -> bool {
        restream_sync::lock::lock(&self.0).contains(output_id)
    }
}

/// An output's claim on its one task. Released however the task ends
/// (return, panic, abort at shutdown), and the release wakes a pass: the
/// output's state changed, and a pass that ran meanwhile left it alone.
struct Claim {
    in_flight: InFlight,
    output_id: String,
    requests: Arc<ReconcileRequests>,
}

impl Drop for Claim {
    fn drop(&mut self) {
        restream_sync::lock::lock(&self.in_flight.0).remove(&self.output_id);
        self.requests.wake();
    }
}

impl EgressReconciler {
    pub fn new(
        engine: Arc<MediaEngine>,
        db: DeadlineDb,
        tuning: RuntimeTuning,
        runtime_state: Arc<StateHub>,
        requests: Arc<ReconcileRequests>,
    ) -> Self {
        Self {
            starter: EgressStarter {
                engine,
                db,
                tuning,
                last_failed: Arc::new(Mutex::new(HashMap::new())),
            },
            runtime_state,
            requests,
            in_flight: InFlight::default(),
            tasks: TaskTracker::new(),
            planning_panics: PlanningPanics::default(),
            decide_start: decide_output_start_action,
        }
    }

    /// The reconciler with another start decision: a test's, that panics
    /// for one output and defers to production's for the rest.
    #[cfg(test)]
    fn with_start_decision(mut self, decide_start: DecideStart) -> Self {
        self.decide_start = decide_start;
        self
    }

    /// Runs `work` for the output in its own task, unless one runs already.
    /// A panic in it is the output's planning panic: whatever it registered
    /// is unregistered, and the output backs off.
    fn spawn_for(&self, output_id: &str, work: impl Future<Output = ()> + Send + 'static) {
        if !restream_sync::lock::lock(&self.in_flight.0).insert(output_id.to_string()) {
            return;
        }
        let claim = Claim {
            in_flight: self.in_flight.clone(),
            output_id: output_id.to_string(),
            requests: self.requests.clone(),
        };
        let (engine, tuning, panics) = (
            self.starter.engine.clone(),
            self.starter.tuning,
            self.planning_panics.clone(),
        );
        self.tasks.spawn(async move {
            // Released last: the pass it wakes sees the backoff.
            let claim = claim;
            if AssertUnwindSafe(work).catch_unwind().await.is_err() {
                engine.unregister_egress(&claim.output_id).await;
                panics.record(&claim.output_id, tuning);
            }
        });
    }

    /// At shutdown: stops taking work and waits up to `within` for the
    /// output tasks still running. A start registers its egress before its
    /// slow steps (the MoQ `ca` read, the kTLS probe), so cleanup's cancel
    /// reaches almost every one; a start still before registration after
    /// `within` may register after cleanup and run until the process exits.
    pub async fn finish(&self, within: Duration) {
        self.tasks.close();
        let _ = tokio::time::timeout(within, self.tasks.wait()).await;
    }

    /// Starts, stops and retries outputs. Returns how long until the earliest
    /// output waiting out its retry backoff may start, so the reconciler can
    /// run then. Each output is planned behind its own panic boundary: one
    /// that panics backs off, and the pass goes on to the next.
    pub async fn reconcile(&self, outputs: &[Output]) -> Option<std::time::Duration> {
        self.planning_panics.retain_outputs(outputs);
        let mut next_retry: Option<Duration> = None;
        for output in outputs {
            // Its task decides; the task's end wakes the next pass.
            if self.in_flight.contains(&output.id) {
                continue;
            }
            let wait = match self.planning_panics.remaining(&output.id, Instant::now()) {
                Some(remaining) => Some(remaining),
                None => AssertUnwindSafe(self.reconcile_output(output))
                    .catch_unwind()
                    .await
                    .unwrap_or_else(|_| {
                        Some(self.planning_panics.record(&output.id, self.starter.tuning))
                    }),
            };
            if let Some(wait) = wait {
                next_retry = Some(next_retry.map_or(wait, |next| next.min(wait)));
            }
        }
        next_retry
    }

    /// Plans one output: starts, stops or retries it. Returns how long until
    /// it should be planned again, when that is a deadline of its own.
    async fn reconcile_output(&self, output: &Output) -> Option<Duration> {
        let engine = &self.starter.engine;
        let tuning = self.starter.tuning;
        // Its failures end here, and only here: from the row this pass
        // read. A mark that landed after this pass read the rows leaves
        // them tracked, so the stale row decides `MarkFailed` again, not
        // a start; the next pass reads the mark and clears them, so the
        // operator's next Start is a fresh attempt.
        if output.retries_exhausted
            && self
                .starter
                .last_failed
                .lock()
                .await
                .remove(&output.id)
                .is_some()
        {
            engine.clear_egress_retry_state(&output.id).await;
        }
        let snapshot =
            load_output_runtime_snapshot(engine, output, tuning.ingest_disconnect_grace_ms).await;
        // Running, or no longer wanted: past panics no longer grow its backoff.
        if snapshot.is_active || !output.wants_running() {
            self.planning_panics.clear(&output.id);
        }
        // Restart backoff is the reconciler's; while an output is active,
        // its retry state is its uploader's, so it is cleared only here.
        if !output.wants_running() {
            engine.clear_egress_retry_state(&output.id).await;
        }
        let failure = {
            let last_failed = self.starter.last_failed.lock().await;
            last_failed
                .get(&output.id)
                .map(|(failed_at, retries)| OutputFailureWindow {
                    retries: *retries,
                    elapsed_ms: failed_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64,
                })
        };

        match (self.decide_start)(
            output.desired_state,
            output.retries_exhausted,
            snapshot.is_active,
            snapshot.effective_has_ingest,
            snapshot.ingest_tracks_ready,
            failure,
            tuning.output_retry_policy(),
            &output.id,
        ) {
            OutputStartAction::NotApplicable => {}
            OutputStartAction::SkipNoIngest => {
                engine.clear_egress_retry_state(&output.id).await;
                return None;
            }
            OutputStartAction::SkipProbePending => {
                // The probe landing emits `ingest.probed`, which wakes a
                // pass. Also re-check on the next interval the pass gap
                // allows: a source that never probes opens the gate only
                // when its probe grace from the first media byte runs
                // out, and nothing wakes a pass for that.
                return Some(Duration::from_millis(tuning.reconciler_interval_ms));
            }
            OutputStartAction::MarkFailed => {
                self.spawn_for(
                    &output.id,
                    mark_retries_exhausted(
                        self.starter.clone(),
                        self.runtime_state.clone(),
                        output.clone(),
                    ),
                );
                return None;
            }
            OutputStartAction::WaitRetry {
                retries,
                backoff_ms,
                remaining_ms,
            } => {
                engine
                    .update_egress_retry_state(&output.id, retries, backoff_ms, remaining_ms)
                    .await;
                return Some(Duration::from_millis(remaining_ms));
            }
            OutputStartAction::StartNow => {
                // A fresh attempt carries no restart backoff.
                engine.clear_egress_retry_state(&output.id).await;
                self.spawn_for(&output.id, self.starter.clone().start(output.clone()));
            }
        }

        match decide_output_stop_action(
            output.desired_state,
            snapshot.is_active,
            snapshot.effective_has_ingest,
        ) {
            OutputStopAction::KeepRunning => {}
            OutputStopAction::StopBecauseIngestLost => {
                info!(
                    correlation_id = %restream_engine::correlation_id::next_correlation_id("out"),
                    output_id = %output.id,
                    output_name = %output.name,
                    pipeline_id = %output.pipeline_id,
                    event_class = "lifecycle",
                    event_type = "lifecycle.stop",
                    "output job stopped because ingest is no longer active",
                );
                engine.unregister_egress(&output.id).await;
                engine.clear_egress_retry_state(&output.id).await;
            }
            OutputStopAction::StopRequested => {
                info!(
                    correlation_id = %restream_engine::correlation_id::next_correlation_id("out"),
                    output_id = %output.id,
                    output_name = %output.name,
                    pipeline_id = %output.pipeline_id,
                    event_class = "lifecycle",
                    event_type = "lifecycle.stop",
                    "output job stopped",
                );
                engine.unregister_egress(&output.id).await;
                engine.clear_egress_retry_state(&output.id).await;
            }
        }
        None
    }
}

/// Records that the output used up its retries; restarts stop until the
/// operator starts or stops it. Runs in the output's own task and waits for
/// the write's outcome, so the claim covers it: no second mark is issued
/// while one is pending. The pass that reads the marked row clears its
/// failures; one that fails is marked again by the pass the claim wakes.
async fn mark_retries_exhausted(
    starter: EgressStarter,
    runtime_state: Arc<StateHub>,
    output: Output,
) {
    let (pipeline_id, output_id) = (output.pipeline_id.clone(), output.id.clone());
    match starter
        .db
        .write(move |pool| async move {
            crate::db::mark_output_retries_exhausted(&pool, &pipeline_id, &output_id).await
        })
        .await
    {
        Ok(()) => {
            runtime_state.mark_changed();
            warn!(
                correlation_id = %restream_engine::correlation_id::next_correlation_id("out"),
                output_id = %output.id,
                output_name = %output.name,
                max_retries = starter.tuning.output_max_retries,
                "output exceeded max retries — retries exhausted; restarts stop until the operator starts or stops it",
            );
        }
        Err(err) => {
            warn!(
                correlation_id = %restream_engine::correlation_id::next_correlation_id("out"),
                output_id = %output.id,
                error = %err,
                "failed to record exhausted output retries in db; retried on the next pass",
            );
        }
    }
}

/// The attempt's job row, written off the start path: created in a task of
/// its own when the attempt starts and finished, after the create, when the
/// attempt ends. Neither the pass nor the egress task waits on the database;
/// each statement is a [`DeadlineDb::write`], so it takes one of the
/// executor's write permits only while it runs.
pub(super) struct AttemptJob {
    finish: tokio::sync::oneshot::Sender<(JobStatus, String)>,
}

impl AttemptJob {
    fn begin(db: &DeadlineDb, job_id: String, pipeline_id: String, output_id: String) -> Self {
        let (finish, finished) = tokio::sync::oneshot::channel::<(JobStatus, String)>();
        let started_at = chrono::Utc::now().to_rfc3339();
        let db = db.clone();
        tokio::spawn(async move {
            let create = {
                let job_id = job_id.clone();
                db.write(move |pool| async move {
                    crate::db::create_job(
                        &pool,
                        &job_id,
                        &pipeline_id,
                        &output_id,
                        JobStatus::Running,
                        &started_at,
                    )
                    .await
                })
            };
            if let Err(err) = create.await {
                warn!(job_id = %job_id, error = %err, "failed to create the attempt's job row");
            }
            // An attempt dropped without an end (its start task panicked)
            // failed; at process exit this task ends with it and startup's
            // `reset_running_jobs` closes the row.
            let (status, ended_at) = finished
                .await
                .unwrap_or_else(|_| (JobStatus::Failed, chrono::Utc::now().to_rfc3339()));
            let finish = db.write(move |pool| async move {
                crate::db::update_job(&pool, &job_id, Some(status), Some(&ended_at)).await
            });
            if let Err(err) = finish.await {
                warn!(error = %err, "failed to finish the attempt's job row");
            }
        });
        Self { finish }
    }

    pub(super) fn finish(self, status: JobStatus) {
        let _ = self.finish.send((status, chrono::Utc::now().to_rfc3339()));
    }
}

impl EgressStarter {
    async fn start(self, output: Output) {
        let output = &output;
        let correlation_id = restream_engine::correlation_id::next_correlation_id("out");
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
        let mut prepared =
            crate::application::egress::prepare_output_ring(&self.engine, output).await;
        let mut stages = std::mem::take(&mut prepared.stages);
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

        let job = AttemptJob::begin(
            &self.db,
            next_output_job_id(&output.id),
            output.pipeline_id.clone(),
            output.id.clone(),
        );

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
                            restream_engine::egress::leaf::EgressProgressSink {
                                bytes_sent: Some(egress.bytes_sent.clone()),
                                metrics: Some(egress.metrics.clone()),
                                last_progress_ms: Some(egress.last_progress_ms.clone()),
                                resync_count: Some(egress.resync_count.clone()),
                                feed_lag_units: Some(egress.feed_lag_units.clone()),
                                backpressure_reason: Some(egress.backpressure_reason.clone()),
                                quality: Some(egress.quality.clone()),
                                terminated_unexpectedly: Some(terminated.clone()),
                                upload_status: None,
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
            stages.push(feed.muxer_lease);
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
                    restream_engine::egress::leaf::EgressProgressSink {
                        bytes_sent: Some(egress.bytes_sent.clone()),
                        metrics: Some(egress.metrics.clone()),
                        last_progress_ms: Some(egress.last_progress_ms.clone()),
                        resync_count: Some(egress.resync_count.clone()),
                        feed_lag_units: Some(egress.feed_lag_units.clone()),
                        backpressure_reason: Some(egress.backpressure_reason.clone()),
                        quality: Some(egress.quality.clone()),
                        terminated_unexpectedly: Some(terminated.clone()),
                        upload_status: None,
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
                source: feed.source,
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
                    restream_engine::egress::leaf::EgressProgressSink {
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
            // `EgressTask::run`'s `OutputUrlScheme::Pipeline` arm re-parses
            // the same `output.url` itself (pure function, identical input)
            // and records the same error there, so nothing is lost by not
            // duplicating that here. That arm's `Ok` branch is otherwise
            // unreachable given this parse already succeeded whenever
            // `pipeline_fabric` is `Some` — kept as a defensive fallback
            // rather than an `unwrap_err()`, so a future refactor that
            // breaks this coupling degrades to the working (if
            // non-fabric) recirculation path instead of a panic.
            match restream_domain::domain::output_spec::RecirculationTarget::parse(&output.url) {
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
                            restream_engine::egress::leaf::EgressProgressSink {
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

        // HTTPS on the fabric needs kernel TLS (no userspace fallback);
        // without it HTTPS outputs stay on the Reqwest uploader.
        let use_hls_put_fabric = self.engine.config.egress_fabric.hls_put_fabric
            && match url_scheme {
                OutputUrlScheme::Http => true,
                OutputUrlScheme::Https => {
                    // The first call probes the kernel (sockets, setsockopt):
                    // keep it off the Tokio worker. Later calls hit the cache.
                    let available = tokio::task::spawn_blocking(
                        restream_engine::egress::tls::ktls::egress_available,
                    )
                    .await
                    .unwrap_or(false);
                    if !available {
                        static LOGGED: std::sync::Once = std::sync::Once::new();
                        LOGGED.call_once(|| {
                            warn!(
                                "kernel TLS unavailable; HTTPS HLS outputs use the Reqwest uploader"
                            );
                        });
                    }
                    available
                }
                _ => false,
            };
        let hls_put_fabric = if use_hls_put_fabric {
            let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let upload_status = Arc::new(std::sync::Mutex::new(
                restream_engine::egress::leaf::UploadStatus::default(),
            ));
            let mut spec = restream_engine::egress::OutputSpec {
                id: restream_engine::egress::OutputId::new(output.id.clone()),
                generation: registration.attempt_id,
                feed: restream_engine::engine_hls_egress_fabric::hls_put_feed_id(
                    &output.pipeline_id,
                ),
                protocol: restream_engine::egress::ProtocolSpec::HlsPut {
                    url: output.url.clone(),
                },
                policy: restream_engine::egress::LeafPolicy::default(),
                progress: Default::default(),
            };
            if let Some(sink) = self
                .engine
                .with_active_egress(&output.id, |egress| {
                    restream_engine::egress::leaf::EgressProgressSink {
                        bytes_sent: Some(egress.bytes_sent.clone()),
                        metrics: Some(egress.metrics.clone()),
                        last_progress_ms: Some(egress.last_progress_ms.clone()),
                        terminated_unexpectedly: Some(terminated.clone()),
                        upload_status: Some(upload_status.clone()),
                        ..Default::default()
                    }
                })
                .await
            {
                spec.progress = sink;
            }
            Some(HlsPutFabricTask {
                spec,
                terminated,
                upload_status,
            })
        } else {
            None
        };

        let moq_fabric = if matches!(url_scheme, OutputUrlScheme::Moqt) {
            prepare_moq_fabric_task(&self.engine, output, &prepared, &registration).await
        } else {
            None
        };

        let task = EgressTask {
            output_id: output.id.clone(),
            pipeline_id: output.pipeline_id.clone(),
            url: output.url.clone(),
            ring: prepared.ring,
            terminal_stage_key: prepared.terminal_stage_key,
            stages,
            registration,
            engine: self.engine.clone(),
            last_failed: self.last_failed.clone(),
            tuning: self.tuning,
            correlation_id,
            job,
            srt_fabric,
            rtmp_fabric,
            sink_fabric,
            pipeline_fabric,
            hls_put_fabric,
            moq_fabric,
        };
        tokio::spawn(task.run());
    }
}

pub(super) fn next_output_job_id(output_id: &str) -> String {
    format!(
        "job_{output_id}_{}",
        restream_engine::correlation_id::next_correlation_id("attempt")
    )
}

#[cfg(test)]
mod tests;
