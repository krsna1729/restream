//! `hls-pull-duty` execution: start a source pipeline, pull its own HLS
//! preview into a second pipeline, and check the gate.
//!
//! The run is one pass through the phases: bring up the source and its
//! publisher, wait for the source's preview, create the pull input (and the
//! failure-reporting input), wait for the `hls-pull` session, hold a rated
//! window, sample the gate, then drive the reconciler-owned lifecycle
//! (disable, re-enable). Every early return still tears the publisher, sink
//! and engine down.

use super::*;

/// The source preview must serve a master playlist before the pull can start.
const PREVIEW_TIMEOUT: Duration = Duration::from_secs(90);
/// The pull session must register with protocol `hls-pull`.
const SESSION_TIMEOUT: Duration = Duration::from_secs(90);
/// Shorter budget once the good pull points at an unresolvable URL: the
/// failing variant should not pay a full timeout on a check that cannot pass.
const FAILING_SESSION_TIMEOUT: Duration = Duration::from_secs(20);
/// A session that has been gone for this long counts as stopped: longer than
/// one pull refresh cycle, so a re-registration cannot be mistaken for it.
const SESSION_GONE_STABLE: Duration = Duration::from_secs(8);
/// One lifecycle transition must complete within this budget.
const LIFECYCLE_BUDGET: Duration = Duration::from_secs(45);

/// One API-created pull input under test.
#[derive(Clone)]
struct PullInput {
    pipeline_id: String,
    input_id: String,
    source_url: String,
}

/// Static paths and ports for one run.
struct Plan {
    ports: TestPorts,
    ffmpeg: PathBuf,
    fixture: PathBuf,
    work_dir: PathBuf,
    db_path: PathBuf,
    log_path: PathBuf,
    restream_bin: PathBuf,
    sink_port: u16,
}

impl Plan {
    /// The source pipeline's own HLS preview master: what the pull fetches.
    fn preview_master_url(&self, pipeline_id: &str) -> String {
        format!(
            "http://127.0.0.1:{}/hls/{pipeline_id}/master.m3u8",
            self.ports.http
        )
    }

    /// The same preview master as an API-relative path, for the harness's own
    /// readiness poll (the API client prepends its base URL).
    fn preview_master_path(pipeline_id: &str) -> String {
        format!("/hls/{pipeline_id}/master.m3u8")
    }

    fn rtmp_publish_url(&self, stream_key: &str) -> String {
        format!("rtmp://127.0.0.1:{}/live/{stream_key}", self.ports.rtmp)
    }
}

/// Accumulates named checks and the failure messages a red run reports.
#[derive(Default)]
struct Checks {
    rows: Vec<Value>,
    failures: Vec<String>,
}

impl Checks {
    fn record(&mut self, id: &str, ok: bool, detail: Value) {
        if !ok {
            self.failures.push(format!("{id}: {detail}"));
        }
        self.rows
            .push(json!({"id": id, "ok": ok, "detail": detail}));
    }
}

/// Live state owned across the run so every early return still tears down.
struct Session {
    restream: Child,
    api: RampApi,
    publisher: Option<Child>,
    sink: Option<GeneralizedSinkServer>,
    sink_metrics: Arc<GeneralizedSinkMetrics>,
    source_pipeline_id: String,
    good: Option<PullInput>,
    failure: Option<PullInput>,
    output_id: Option<String>,
}

pub(crate) async fn run_hls_pull_duty(
    options: HlsPullOptions,
    args: &[String],
) -> Result<Value, String> {
    let started = Instant::now();
    let work_dir = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".local/artifacts/hls-pull-duty"));
    std::fs::create_dir_all(&work_dir).map_err(|error| error.to_string())?;

    let ports = TestPorts::from_env();
    let plan = Plan {
        ffmpeg: restream_engine::ffmpeg_binary::ffmpeg_bin_path().to_path_buf(),
        fixture: restream_engine::test_fixtures::canonical_h264_ts_fixture()?,
        db_path: default_work_db_path(&work_dir, "hls-pull-duty.sqlite"),
        log_path: work_dir.join("restream.log"),
        restream_bin: default_restream_bin(),
        sink_port: harness_port_defaults().sink,
        work_dir: work_dir.clone(),
        ports: TestPorts {
            http: ports.http,
            rtmp: ports.rtmp,
            srt: ports.srt,
        },
    };

    println!(
        "[hls-pull-duty] {}",
        json!({
            "phase": "config",
            "durationSecs": options.duration_secs,
            "reconcileIntervalMs": options.reconcile_interval_ms,
            "failUnresolvable": options.fail_unresolvable,
            "ports": {"http": plan.ports.http, "rtmp": plan.ports.rtmp, "srt": plan.ports.srt},
            "sinkPort": plan.sink_port,
            "ffmpeg": plan.ffmpeg.display().to_string(),
            "fixture": plan.fixture.display().to_string(),
        })
    );

    let env_overrides = [(
        "RESTREAM_RECONCILE_INTERVAL_MS",
        options.reconcile_interval_ms.to_string(),
    )];
    let restream = start_restream_child_with_env(
        &plan.restream_bin,
        &plan.ports,
        &plan.db_path,
        &plan.log_path,
        &env_overrides,
    )
    .await?;
    let api = login_api(&plan.ports).await?;

    let mut session = Session {
        restream,
        api,
        publisher: None,
        sink: None,
        sink_metrics: Arc::new(GeneralizedSinkMetrics::default()),
        source_pipeline_id: String::new(),
        good: None,
        failure: None,
        output_id: None,
    };
    let report = session.execute(&options, &plan, started, args).await;
    session.shutdown().await;
    let report = report?;
    if let Ok(bytes) = serde_json::to_vec_pretty(&report) {
        let _ = std::fs::write(work_dir.join("report.json"), bytes);
    }
    Ok(report)
}

impl Session {
    async fn execute(
        &mut self,
        options: &HlsPullOptions,
        plan: &Plan,
        started: Instant,
        args: &[String],
    ) -> Result<Value, String> {
        let mut checks = Checks::default();

        // ── Source pipeline, fed by a real RTMP publisher ─────────────
        let source_pipeline_id =
            create_resource_pipeline(&self.api, "hls-pull-source", "sk-hls-pull-source").await?;
        self.source_pipeline_id = source_pipeline_id.clone();
        let publisher_args = crate::hls_push_duty::run::rtmp_publisher_args(
            &plan.fixture,
            &plan.rtmp_publish_url("sk-hls-pull-source"),
        );
        // The source publisher: real-time copy of the checked-in fixture over RTMP.
        self.publisher = Some(spawn_logged_ffmpeg(
            &plan.ffmpeg,
            &publisher_args,
            &plan.work_dir.join("source-publisher.log"),
            "hls-pull-source",
        )?);
        wait_for_api_input_live(&self.api, &source_pipeline_id, Duration::from_secs(60)).await?;
        wait_for_api_input_media_ready(&self.api, &source_pipeline_id, Duration::from_secs(60))
            .await?;

        let preview_url = plan.preview_master_url(&source_pipeline_id);
        let preview_path = Plan::preview_master_path(&source_pipeline_id);
        wait_for_preview_master(&self.api, &preview_path, PREVIEW_TIMEOUT).await?;
        println!(
            "[hls-pull-duty] {}",
            json!({"phase": "source-ready", "pipelineId": source_pipeline_id, "url": preview_url})
        );

        // ── Pull pipeline: input + promoted selection + sink output ───
        let pull_pipeline_id =
            create_resource_pipeline(&self.api, "hls-pull-target", "sk-hls-pull-target").await?;
        let sink = start_generalized_sink_server(plan.sink_port, self.sink_metrics.clone()).await?;
        self.sink = Some(sink);
        let output_url = format!("rtmp://127.0.0.1:{}/live/hls-pull-target", plan.sink_port);
        let output_id = create_output(
            &self.api,
            &pull_pipeline_id,
            "sink-pull",
            &output_url,
            "source",
        )
        .await?;
        start_output(&self.api, &pull_pipeline_id, &output_id).await?;
        self.output_id = Some(output_id);

        let cookie = self
            .api
            .cookie
            .clone()
            .ok_or("harness logged in without a session cookie to present to the preview")?;
        let pull_source_url = options.pull_source_url(&preview_url);
        let pull_input_id = create_pull_input(
            &self.api,
            &pull_pipeline_id,
            "Pull in",
            &pull_source_url,
            &cookie,
        )
        .await?;
        // A later-created input is a backup and does not forward; the pull
        // graph only publishes through the selected input, so promote it
        // before the reconciler starts the pull. Promote wakes the reconciler
        // itself, so the first pass that sees this row sees it selected.
        promote_input(&self.api, &pull_pipeline_id, &pull_input_id).await?;
        self.good = Some(PullInput {
            pipeline_id: pull_pipeline_id.clone(),
            input_id: pull_input_id.clone(),
            source_url: pull_source_url.clone(),
        });

        // ── The failure-reporting input ───────────────────────────────
        // In the failing variant the good pull already carries the bad URL;
        // otherwise a second pipeline carries it, so the gate always covers
        // "reports a failure, does not stop the pipeline".
        if options.fail_unresolvable {
            self.failure = self.good.clone();
        } else {
            let bad_pipeline_id =
                create_resource_pipeline(&self.api, "hls-pull-bad", "sk-hls-pull-bad").await?;
            let bad_url = options.bad_pull_url();
            let bad_input_id =
                create_pull_input(&self.api, &bad_pipeline_id, "Bad pull", bad_url, &cookie)
                    .await?;
            self.failure = Some(PullInput {
                pipeline_id: bad_pipeline_id,
                input_id: bad_input_id,
                source_url: bad_url.to_string(),
            });
        }

        // ── Read the created input back ───────────────────────────────
        let inputs = self.pipeline_inputs(&pull_pipeline_id).await?;
        let row = find_input(&inputs, &pull_input_id);
        checks.record(
            "pullInputCreated",
            row.is_some(),
            json!({"inputId": pull_input_id, "found": row.is_some()}),
        );
        let row = row.cloned().unwrap_or(Value::Null);
        let read_back = json!({
            "kind": row.get("kind").cloned().unwrap_or(Value::Null),
            "sourceUrl": row.get("sourceUrl").cloned().unwrap_or(Value::Null),
            "variantRule": row.get("variantRule").cloned().unwrap_or(Value::Null),
            "authKind": row.get("authKind").cloned().unwrap_or(Value::Null),
            "enabled": row.get("enabled").cloned().unwrap_or(Value::Null),
        });
        checks.record(
            "pullInputSourceUrl",
            row.get("kind").and_then(Value::as_str) == Some("hlsPull")
                && row.get("sourceUrl").and_then(Value::as_str) == Some(pull_source_url.as_str())
                && row.get("variantRule").and_then(Value::as_str) == Some("highestBandwidth")
                && row.get("authKind").and_then(Value::as_str) == Some("headers"),
            read_back,
        );

        // ── Wait for the `hls-pull` session and hold the window ───────
        let session_timeout = if options.fail_unresolvable {
            FAILING_SESSION_TIMEOUT
        } else {
            SESSION_TIMEOUT
        };
        let protocol = self
            .wait_for_pull_session(&pull_pipeline_id, &pull_input_id, session_timeout)
            .await?;
        checks.record(
            "pullSessionProtocol",
            protocol.as_deref() == Some("hls-pull"),
            json!({"protocol": protocol, "expected": "hls-pull"}),
        );
        println!(
            "[hls-pull-duty] {}",
            json!({"phase": "pull-session", "protocol": protocol})
        );

        // A baseline before the window, so keep-up measures progress inside it
        // rather than lifetime totals (the probe's keyframe timeline is capped,
        // so its media seconds freeze while uptime keeps growing).
        let sink_before = self.sink_metrics.summary();
        let window_started = std::time::Instant::now();
        tokio::time::sleep(options.window()).await;
        let wall_secs = window_started.elapsed().as_secs_f64();

        // ── Sample the good path over the window ──────────────────────
        let probe = self
            .api
            .get_json(&format!("/api/v1/pipelines/{pull_pipeline_id}/probe"))
            .await
            .unwrap_or(Value::Null);
        let sink_summary = self.sink_metrics.summary();
        checks.record(
            "pullMediaPublished",
            pull_published_ok(&probe),
            pull_media_summary(&probe),
        );
        checks.record(
            "pullSinkDelivered",
            pull_delivered_ok(&sink_summary),
            sink_summary.clone(),
        );
        checks.record(
            "pullKeepsUp",
            pull_keeps_up_over_window(&sink_before, &sink_summary, wall_secs),
            json!({
                "wallSeconds": (wall_secs * 10.0).round() / 10.0,
                "mediaProgressMsBefore": sink_before.get("mediaProgressMs").cloned().unwrap_or(Value::Null),
                "mediaProgressMs": sink_summary.get("mediaProgressMs").cloned().unwrap_or(Value::Null),
                "uptimeSeconds": pull_media_summary(&probe).get("uptimeSeconds").cloned().unwrap_or(Value::Null),
            }),
        );

        // ── The failure the gate exists for ───────────────────────────
        let failure = self
            .failure
            .clone()
            .ok_or("failure-reporting input missing")?;
        let reported = self
            .wait_for_bad_pull_log(&plan.log_path, &failure.input_id, Duration::from_secs(60))
            .await?;
        checks.record(
            "badPullReported",
            reported,
            json!({
                "inputId": failure.input_id,
                "url": failure.source_url,
                "logLine": format!("HLS pull failed; retrying input={}", failure.input_id),
            }),
        );

        let bad_inputs = self.pipeline_inputs(&failure.pipeline_id).await?;
        let bad_row = find_input(&bad_inputs, &failure.input_id);
        let bad_connected = bad_row.and_then(session_protocol).is_some();
        let bad_enabled = bad_row
            .and_then(|row| row.get("enabled"))
            .and_then(Value::as_bool)
            == Some(true);
        let pipelines = self.api.get_json("/api/v1/pipelines").await?;
        let bad_present = pipeline_present(&pipelines, &failure.pipeline_id);
        checks.record(
            "badPullDoesNotStopPipeline",
            bad_present && bad_enabled && !bad_connected,
            json!({
                "pipelineId": failure.pipeline_id,
                "pipelinePresent": bad_present,
                "inputEnabled": bad_enabled,
                "inputConnected": bad_connected,
            }),
        );

        // ── Reconciler-owned lifecycle ────────────────────────────────
        let lifecycle = self.run_lifecycle(options, &mut checks).await?;

        let passed = checks.failures.is_empty();
        let report = json!({
            "passed": passed,
            "mode": "hls-pull-duty",
            "config": {
                "durationSecs": options.duration_secs,
                "reconcileIntervalMs": options.reconcile_interval_ms,
                "failUnresolvable": options.fail_unresolvable,
                "cliArgs": args,
            },
            "environment": {
                "cpuCount": std::thread::available_parallelism()
                    .map(std::num::NonZeroUsize::get)
                    .unwrap_or(0),
                "ffmpeg": plan.ffmpeg.display().to_string(),
                "fixture": plan.fixture.display().to_string(),
                "restreamBin": plan.restream_bin.display().to_string(),
                "ports": {"http": plan.ports.http, "rtmp": plan.ports.rtmp},
                "sinkPort": plan.sink_port,
            },
            "wallClockSecs": round2(started.elapsed().as_secs_f64()),
            "source": {
                "pipelineId": self.source_pipeline_id,
                "previewUrl": preview_url,
            },
            "pull": self.good.as_ref().map(|pull| json!({
                "pipelineId": pull.pipeline_id,
                "inputId": pull.input_id,
                "sourceUrl": pull.source_url,
            })),
            "failurePull": self.failure.as_ref().map(|pull| json!({
                "pipelineId": pull.pipeline_id,
                "inputId": pull.input_id,
                "sourceUrl": pull.source_url,
            })),
            "probe": probe,
            "sink": sink_summary,
            "lifecycle": lifecycle,
            "checks": checks.rows,
            "failedChecks": checks.failures,
        });
        println!(
            "[hls-pull-duty] {}",
            json!({"phase": "done", "passed": passed, "failedChecks": checks.failures})
        );
        if !passed {
            let report_path = plan.work_dir.join("report.json");
            let _ = std::fs::write(
                &report_path,
                serde_json::to_vec_pretty(&report).unwrap_or_default(),
            );
            return Err(format!(
                "hls-pull-duty failed: {:?}; report={}",
                checks.failures,
                report_path.display()
            ));
        }
        Ok(report)
    }

    /// Disable the pull input through the API and assert the pull stops, then
    /// re-enable it and assert it starts again. Reports the observed latency
    /// so a reconciler that only converges on its backstop is visible.
    ///
    /// The API refuses to disable a selected input (`primary or selected
    /// inputs cannot be disabled`), and the pull only forwards while selected,
    /// so the lifecycle first selects the pipeline's primary input: the pull
    /// input becomes a backup and the disable is accepted.
    async fn run_lifecycle(
        &self,
        options: &HlsPullOptions,
        checks: &mut Checks,
    ) -> Result<Value, String> {
        let Some(pull) = self.good.clone() else {
            return Ok(Value::Null);
        };
        let input_url = format!(
            "/api/v1/pipelines/{}/inputs/{}",
            pull.pipeline_id, pull.input_id
        );

        // Unselect the pull input before disabling it.
        let before = self.pipeline_inputs(&pull.pipeline_id).await?;
        let unselect = match primary_input_id(&before) {
            Some(primary_id) => {
                let promoted = promote_input(&self.api, &pull.pipeline_id, primary_id).await;
                json!({
                    "primaryInputId": primary_id,
                    "selectedPrimary": promoted.is_ok(),
                    "error": promoted.err(),
                })
            }
            None => json!({"primaryInputId": null}),
        };

        let disable_started = Instant::now();
        let disable = self
            .api
            .patch_json(&input_url, json!({"enabled": false}))
            .await;
        let disable_accepted = disable.is_ok();
        let stopped = if disable_accepted {
            self.wait_for_session_gone(&pull, LIFECYCLE_BUDGET).await?
        } else {
            false
        };
        let disable_elapsed = disable_started.elapsed().as_secs_f64();
        checks.record(
            "lifecycleDisableStopsPull",
            stopped,
            json!({
                "accepted": disable_accepted,
                "error": disable.err(),
                "elapsedSecs": round2(disable_elapsed),
            }),
        );

        let enable_started = Instant::now();
        let enable = self
            .api
            .patch_json(&input_url, json!({"enabled": true}))
            .await;
        let enable_accepted = enable.is_ok();
        let protocol = if enable_accepted {
            self.wait_for_pull_session(&pull.pipeline_id, &pull.input_id, LIFECYCLE_BUDGET)
                .await?
        } else {
            None
        };
        let enable_elapsed = enable_started.elapsed().as_secs_f64();
        checks.record(
            "lifecycleEnableRestartsPull",
            protocol.as_deref() == Some("hls-pull"),
            json!({
                "accepted": enable_accepted,
                "error": enable.err(),
                "elapsedSecs": round2(enable_elapsed),
                "protocol": protocol,
            }),
        );

        // Mutating API requests wake the reconciler through the
        // `wake_reconciler_after_mutation` middleware, so both the disable and
        // the enable converge within about one reconcile interval. The disable
        // also cancels the running session's token directly; the pull *task*
        // is stopped by the next pass.
        let backstop = options.backstop().as_secs_f64();
        let waited_for_backstop = enable_elapsed >= backstop * 0.6;
        println!(
            "[hls-pull-duty] {}",
            json!({
                "phase": "lifecycle",
                "disableSecs": round2(disable_elapsed),
                "enableSecs": round2(enable_elapsed),
                "backstopSecs": round2(backstop),
            })
        );
        Ok(json!({
            "unselect": unselect,
            "disableObservedSecs": round2(disable_elapsed),
            "enableObservedSecs": round2(enable_elapsed),
            "reconcileIntervalMs": options.reconcile_interval_ms,
            "backstopSecs": round2(backstop),
            "reEnableWaitedForBackstop": waited_for_backstop,
            "finding": if waited_for_backstop {
                "The re-enabled pull did not start until roughly one reconcile backstop after the PATCH, so the mutation wake did not (or could not) run a pass promptly on this run."
            } else {
                "The mutation-wake middleware ran a reconcile pass promptly after the PATCH, so the pull converged without waiting for the periodic backstop."
            },
            "notes": [
                "PATCH disable cancels the running session's token at once; the pull task itself is stopped by the next reconcile pass (converge_pull_inputs).",
                "PATCH enable (like input create/promote/update) wakes the reconciler through the wake_reconciler_after_mutation middleware, so the pull restarts within about one reconcile interval, not on the backstop.",
                "The API refuses to disable a selected input (\"primary or selected inputs cannot be disabled\"), and the pull only forwards while selected, so the lifecycle selects the pipeline's primary input before disabling the pull input.",
                "Re-enabling does not create the session directly; the puller registers the hls-pull session when its first fetched object arrives.",
            ],
        }))
    }

    async fn pipeline_inputs(&self, pipeline_id: &str) -> Result<Value, String> {
        self.api
            .get_json(&format!("/api/v1/pipelines/{pipeline_id}/inputs"))
            .await
    }

    /// Wait until the pull input's session registers; returns the protocol it
    /// registered with, or `None` if the budget expires first.
    async fn wait_for_pull_session(
        &self,
        pipeline_id: &str,
        input_id: &str,
        timeout: Duration,
    ) -> Result<Option<String>, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let inputs = self.pipeline_inputs(pipeline_id).await?;
            if let Some(protocol) = find_input(&inputs, input_id).and_then(session_protocol) {
                return Ok(Some(protocol.to_string()));
            }
            if Instant::now() >= deadline {
                return Ok(None);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Wait until the input has no connected session and has had none for
    /// [`SESSION_GONE_STABLE`], so a brief re-registration before the pull
    /// task is stopped is not read as "stopped".
    async fn wait_for_session_gone(
        &self,
        pull: &PullInput,
        budget: Duration,
    ) -> Result<bool, String> {
        let deadline = Instant::now() + budget;
        let mut gone_since: Option<Instant> = None;
        loop {
            let inputs = self.pipeline_inputs(&pull.pipeline_id).await?;
            let connected = find_input(&inputs, &pull.input_id)
                .and_then(session_protocol)
                .is_some();
            if connected {
                gone_since = None;
            } else {
                let since = gone_since.get_or_insert_with(Instant::now);
                if since.elapsed() >= SESSION_GONE_STABLE {
                    return Ok(true);
                }
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    /// Wait for the engine log to name the failing input on a retry line.
    async fn wait_for_bad_pull_log(
        &self,
        log_path: &Path,
        input_id: &str,
        timeout: Duration,
    ) -> Result<bool, String> {
        let deadline = Instant::now() + timeout;
        loop {
            let log = std::fs::read_to_string(log_path).unwrap_or_default();
            if log_reports_pull_failure(&log, input_id) {
                return Ok(true);
            }
            if Instant::now() >= deadline {
                return Ok(false);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
    }

    async fn shutdown(&mut self) {
        if let Some(mut publisher) = self.publisher.take() {
            stop_child(&mut publisher).await;
        }
        if let (Some(pull), Some(output_id)) = (self.good.as_ref(), self.output_id.as_ref()) {
            let _ = self
                .api
                .post_null(&format!(
                    "/api/v1/pipelines/{}/outputs/{}/stop",
                    pull.pipeline_id, output_id
                ))
                .await;
        }
        if let Some(sink) = self.sink.take() {
            stop_generalized_sink_server(sink);
        }
        stop_child(&mut self.restream).await;
    }
}

/// Create an `hlsPull` input that presents `cookie` through the `headers`
/// auth variant, returning its id.
pub(crate) async fn create_pull_input(
    api: &RampApi,
    pipeline_id: &str,
    label: &str,
    source_url: &str,
    cookie: &str,
) -> Result<String, String> {
    let body = json!({
        "label": label,
        "kind": "hlsPull",
        "sourceUrl": source_url,
        "variantRule": "highestBandwidth",
        // The preview routes are behind require_hls_access, which is the
        // dashboard session cookie; the puller presents it as a request header.
        "auth": {"kind": "headers", "headers": [{"name": "Cookie", "value": cookie}]},
    });
    let response = api
        .post_json(&format!("/api/v1/pipelines/{pipeline_id}/inputs"), body)
        .await?;
    response
        .pointer("/input/id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| format!("input create response missing input.id: {response}"))
}

/// Promote the input to the pipeline's selected input, so its session
/// forwards into the pipeline ring.
pub(crate) async fn promote_input(
    api: &RampApi,
    pipeline_id: &str,
    input_id: &str,
) -> Result<(), String> {
    api.post_null(&format!(
        "/api/v1/pipelines/{pipeline_id}/inputs/{input_id}/promote"
    ))
    .await
    .map(|_| ())
}

/// Poll a pipeline's preview until it serves a master playlist a pull can
/// follow. A preview with no segments yet answers 404, which is expected
/// until the preview graph's first segment. `path` is the API-relative path
/// (`get_text_response` prepends the base URL).
pub(crate) async fn wait_for_preview_master(
    api: &RampApi,
    path: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let mut next_log = Instant::now() + Duration::from_secs(5);
    let mut last;
    loop {
        last = match api.get_text_response(path).await {
            Ok((status, body)) => {
                if status.is_success()
                    && body.contains("#EXTM3U")
                    && body.contains("EXT-X-STREAM-INF")
                {
                    return Ok(());
                }
                Some((status.as_u16(), truncate_body(&body)))
            }
            Err(error) => Some((0, error)),
        };
        if Instant::now() >= next_log {
            println!(
                "[harness-progress] {}",
                json!({"phase": "preview-wait", "path": path, "last": last}),
            );
            next_log += Duration::from_secs(5);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "preview {path} did not serve a master playlist within {}s; last={last:?}",
                timeout.as_secs(),
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

fn round2(value: f64) -> f64 {
    (value * 100.0).round() / 100.0
}

/// A body snippet for progress lines and errors, bounded so a large body never
/// floods the harness log.
fn truncate_body(body: &str) -> String {
    const LIMIT: usize = 200;
    let trimmed = body.trim();
    if trimmed.len() <= LIMIT {
        trimmed.to_string()
    } else {
        format!("{}…", &trimmed[..LIMIT])
    }
}
