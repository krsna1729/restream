//! `moq-relay-interop` execution: egress, isolation and ingest phases against
//! the pinned MediaMTX, with every early return tearing all processes down.

use super::*;

const MODE: &str = "moq-relay-interop";
/// B-frame free (decode order is presentation order, so the PTS check is
/// strict), 30 fps H.264 High with stereo AAC.
const FIXTURE: &str = "test/fixtures/transport/av-marker-h264-bf0.ts";
const EGRESS_STREAM_KEY: &str = "sk-moq-egress";
const INGEST_STREAM_KEY: &str = "sk-moq-ingest";

/// Media the egress FFprobe read must span (the contract's ≥ 20 s window).
const EGRESS_MIN_SPAN_SECS: f64 = 20.0;
/// FFprobe reads this much so the sampled span clears the minimum.
const EGRESS_READ_SECS: u64 = 22;
/// The isolation phase lets the stream run this long before the kill.
const OUTAGE_AFTER: Duration = Duration::from_secs(10);
/// MediaMTX stays down this long before the restart.
const OUTAGE_DOWN: Duration = Duration::from_secs(3);
/// The MoQ output must publish again within this budget after the restart.
const RECONNECT_BUDGET: Duration = Duration::from_secs(60);
/// Sibling sampling continues this long after the reconnect.
const POST_RECONNECT_HOLD: Duration = Duration::from_secs(5);
/// Longest wall-clock stretch the RTMP sibling may go without a new video
/// frame while the relay is down and back; 30 fps is 33 ms per frame.
pub(crate) const SIBLING_MAX_STALL: Duration = Duration::from_secs(1);
const SIBLING_SAMPLE_PERIOD: Duration = Duration::from_millis(100);
/// Post-reconnect and ingest reads: shorter, same checks.
const SHORT_MIN_SPAN_SECS: f64 = 8.0;
const SHORT_READ_SECS: u64 = 10;
const INPUT_TIMEOUT: Duration = Duration::from_secs(60);
const RELAY_PUBLISH_TIMEOUT: Duration = Duration::from_secs(60);

/// Restream's MoQ ingest listener settings (`src/media/config/moq.rs`).
const MOQ_INGEST_LISTEN_ENV: &str = "RESTREAM_MOQ_INGEST_LISTEN";
const MOQ_TLS_CERT_ENV: &str = "RESTREAM_MOQ_TLS_CERT";
const MOQ_TLS_KEY_ENV: &str = "RESTREAM_MOQ_TLS_KEY";

/// Accumulates named checks and the failure messages a red run reports.
pub(crate) struct Checks {
    mode: &'static str,
    pub(crate) rows: Vec<Value>,
    pub(crate) failures: Vec<String>,
}

impl Checks {
    pub(crate) fn new(mode: &'static str) -> Self {
        Self {
            mode,
            rows: Vec::new(),
            failures: Vec::new(),
        }
    }

    pub(crate) fn record(&mut self, id: &str, ok: bool, detail: Value) {
        if !ok {
            self.failures.push(format!("{id}: {detail}"));
        }
        println!("[{}] {}", self.mode, json!({"check": id, "ok": ok}));
        self.rows
            .push(json!({"id": id, "ok": ok, "detail": detail}));
    }

    /// Record an FFprobe read of a relay path against the relay contract.
    pub(crate) fn record_sample(
        &mut self,
        id: &str,
        probe: Result<Value, String>,
        input_fps: f64,
        min_span_secs: f64,
    ) {
        match probe {
            Ok(probe) => {
                let sample = RelaySample::from_probe(&probe);
                let failures = sample.failures(input_fps, min_span_secs);
                let mut detail = sample.to_json(input_fps);
                detail["minSpanSecs"] = json!(min_span_secs);
                detail["failures"] = json!(failures);
                self.record(id, failures.is_empty(), detail);
            }
            Err(error) => self.record(id, false, json!({"error": error})),
        }
    }
}

/// Everything the run owns, so teardown reaches all of it from any phase.
struct Session {
    work_dir: PathBuf,
    ffmpeg: PathBuf,
    fixture: PathBuf,
    input_fps: f64,
    tls: InteropTls,
    relay: Relay,
    restream: Child,
    api: RampApi,
    sink: Option<GeneralizedSinkServer>,
    sink_metrics: Arc<GeneralizedSinkMetrics>,
    sink_port: u16,
    restream_rtmp: u16,
    publishers: Vec<Child>,
    outputs: Vec<(String, String)>,
    checks: Checks,
    report: serde_json::Map<String, Value>,
}

pub(crate) async fn run_moq_relay_interop() -> Result<Value, String> {
    let started = Instant::now();
    let work_dir = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".local/artifacts/moq-relay-interop"));
    std::fs::create_dir_all(&work_dir).map_err(|error| error.to_string())?;
    let work_dir = absolute_path(&work_dir)?;

    let mediamtx = pinned_mediamtx_bin().await?;
    let fixture = restream::test_fixtures::checked_in_fixture(FIXTURE)?;
    let input_fps = fixture_frame_rate(&fixture).await?;
    let ffmpeg = restream::media::ffmpeg_binary::ffmpeg_bin_path().to_path_buf();
    let tls = InteropTls::generate(&work_dir.join("tls")).await?;
    let ports = TestPorts::from_env();
    let relay_ports = RelayPorts::allocate();
    let sink_port = harness_port_defaults().sink;

    let config_path = work_dir.join("mediamtx.yml");
    std::fs::write(
        &config_path,
        render_mediamtx_config(
            relay_ports,
            &tls,
            &RelayForward {
                stream_key: INGEST_STREAM_KEY,
                fingerprint: &tls.leaf_sha256,
            },
        ),
    )
    .map_err(|error| error.to_string())?;
    println!(
        "[{MODE}] {}",
        json!({
            "phase": "config",
            "mediamtx": mediamtx.display().to_string(),
            "fixture": fixture.display().to_string(),
            "inputFps": input_fps,
            "ports": {"http": ports.http, "rtmp": ports.rtmp, "srt": ports.srt, "sink": sink_port},
            "relayPorts": relay_ports.to_json(),
        })
    );

    let mut relay = Relay::new(
        mediamtx.clone(),
        config_path,
        work_dir.join("mediamtx.log"),
        relay_ports,
    );
    relay.start().await?;

    let env_overrides = [
        (
            MOQ_INGEST_LISTEN_ENV,
            format!("127.0.0.1:{}", relay_ports.restream_moq),
        ),
        (MOQ_TLS_CERT_ENV, tls.leaf_pem.display().to_string()),
        (MOQ_TLS_KEY_ENV, tls.leaf_key.display().to_string()),
        (
            "RESTREAM_MOQ_CA_DIR",
            tls.ca_pem
                .parent()
                .expect("CA directory")
                .display()
                .to_string(),
        ),
        // The outage is seconds long; the production 5 s base backoff would
        // only stretch the reconnect check, not change what it proves.
        ("RESTREAM_OUTPUT_RETRY_BASE_MS", "1000".to_string()),
        ("RESTREAM_OUTPUT_RETRY_MAX_MS", "4000".to_string()),
    ];
    let restream = match start_restream_child_with_env(
        &default_restream_bin(),
        &ports,
        &default_work_db_path(&work_dir, "moq-relay-interop.sqlite"),
        &work_dir.join("restream.log"),
        &env_overrides,
    )
    .await
    {
        Ok(child) => child,
        Err(error) => {
            relay.kill().await;
            return Err(error);
        }
    };
    let api = match login_api(&ports).await {
        Ok(api) => api,
        Err(error) => {
            let mut restream = restream;
            stop_child(&mut restream).await;
            relay.kill().await;
            return Err(error);
        }
    };

    let mut session = Session {
        work_dir: work_dir.clone(),
        ffmpeg,
        fixture,
        input_fps,
        tls,
        relay,
        restream,
        api,
        sink: None,
        sink_metrics: Arc::new(GeneralizedSinkMetrics::default()),
        sink_port,
        restream_rtmp: ports.rtmp,
        publishers: Vec::new(),
        outputs: Vec::new(),
        checks: Checks::new(MODE),
        report: serde_json::Map::new(),
    };
    let outcome = session.execute().await;
    session.shutdown().await;

    let passed = outcome.is_ok() && session.checks.failures.is_empty();
    let mut failures = std::mem::take(&mut session.checks.failures);
    if let Err(error) = &outcome {
        failures.push(format!("aborted: {error}"));
    }
    let report = json!({
        "passed": passed,
        "mode": MODE,
        "wireProfile": "MOQT draft-19 subset, interop-pinned to MediaMTX v1.21.2",
        "environment": {
            "mediamtx": mediamtx.display().to_string(),
            "mediamtxVersion": PINNED_MEDIAMTX_VERSION,
            "restreamBin": default_restream_bin().display().to_string(),
            "fixture": session.fixture.display().to_string(),
            "inputFps": session.input_fps,
            "ports": {"http": ports.http, "rtmp": ports.rtmp, "sink": sink_port},
            "relayPorts": relay_ports.to_json(),
        },
        "phases": Value::Object(std::mem::take(&mut session.report)),
        "checks": std::mem::take(&mut session.checks.rows),
        "failedChecks": failures,
        "wallClockSecs": round3(started.elapsed().as_secs_f64()),
    });
    let report_path = work_dir.join("report.json");
    let _ = std::fs::write(
        &report_path,
        serde_json::to_vec_pretty(&report).unwrap_or_default(),
    );
    println!(
        "[{MODE}] {}",
        json!({"phase": "done", "passed": passed, "failedChecks": report["failedChecks"]})
    );
    if passed {
        Ok(report)
    } else {
        Err(format!(
            "{MODE} failed: {}; report={}",
            report["failedChecks"],
            report_path.display()
        ))
    }
}

impl Session {
    /// Ingest does not depend on egress, so it runs even when egress aborted:
    /// one run reports both directions.
    async fn execute(&mut self) -> Result<(), String> {
        let egress = match self.egress_phase().await {
            Ok((pipeline, moq_output)) => self.isolation_phase(&pipeline, &moq_output).await,
            Err(error) => Err(error),
        };
        let ingest = self.ingest_phase().await;
        egress.and(ingest)
    }

    /// RTMP publisher → pipeline → `moqt://` MediaMTX + RTMP sibling sink;
    /// MediaMTX must show a MoQ-published path that reads back correctly.
    /// Returns the pipeline and its MoQ output.
    async fn egress_phase(&mut self) -> Result<(String, String), String> {
        let pipeline = create_resource_pipeline(&self.api, "moq-egress", EGRESS_STREAM_KEY).await?;
        self.sink =
            Some(start_generalized_sink_server(self.sink_port, self.sink_metrics.clone()).await?);
        let moq_url = self.relay.moq_url(EGRESS_PATH, &self.tls.ca_pem);
        let moq_output = self.add_output(&pipeline, "moq-relay", &moq_url).await?;
        let sibling_url = format!("rtmp://127.0.0.1:{}/live/moq-sibling", self.sink_port);
        self.add_output(&pipeline, "rtmp-sibling", &sibling_url)
            .await?;
        let publish_url = format!(
            "rtmp://127.0.0.1:{}/live/{EGRESS_STREAM_KEY}",
            self.restream_rtmp
        );
        self.spawn_publisher(
            "egress-publisher",
            &crate::hls_push_duty::run::rtmp_publisher_args(&self.fixture, &publish_url),
        )?;
        wait_for_api_input_live(&self.api, &pipeline, INPUT_TIMEOUT).await?;
        wait_for_api_input_media_ready(&self.api, &pipeline, INPUT_TIMEOUT).await?;

        let published = self
            .relay
            .wait_path_published(EGRESS_PATH, "moqSession", RELAY_PUBLISH_TIMEOUT)
            .await;
        let path_ok = published.is_ok();
        self.checks.record(
            "egressRelayPublished",
            path_ok,
            published
                .clone()
                .unwrap_or_else(|error| json!({"error": error})),
        );
        if !path_ok {
            self.record_output_status("egressMoqOutput", &pipeline, &moq_output)
                .await;
            return Err("the MoQ output never published to MediaMTX".to_string());
        }

        let probe = ffprobe_live_sample(
            &self.relay.rtsp_url(EGRESS_PATH),
            &self.work_dir.join("egress-probe.json"),
            EGRESS_READ_SECS,
            "5M",
            "5M",
        )
        .await;
        self.checks.record_sample(
            "egressReadBack",
            probe,
            self.input_fps,
            EGRESS_MIN_SPAN_SECS,
        );
        self.record_output_status("egressMoqOutput", &pipeline, &moq_output)
            .await;
        Ok((pipeline, moq_output))
    }

    /// Kill MediaMTX mid-stream and restart it: the MoQ output must publish
    /// again and the RTMP sibling on the same pipeline must not notice.
    async fn isolation_phase(&mut self, pipeline: &str, moq_output: &str) -> Result<(), String> {
        let sampler_stop = CancellationToken::new();
        let sampler_started = Instant::now();
        let sampler = tokio::spawn(sample_sibling(
            self.sink_metrics.clone(),
            sampler_started,
            sampler_stop.clone(),
        ));
        let connections_before = self.sink_metrics.connections.load(Ordering::Relaxed);

        tokio::time::sleep(OUTAGE_AFTER).await;
        self.relay.kill().await;
        let killed_at = Instant::now();
        println!("[{MODE}] {}", json!({"phase": "relay-killed"}));
        tokio::time::sleep(OUTAGE_DOWN).await;
        let restart = self.relay.start().await;
        let restarted_at = Instant::now();
        println!(
            "[{MODE}] {}",
            json!({"phase": "relay-restarted", "ok": restart.is_ok()})
        );
        let republished = match restart {
            Ok(()) => {
                self.relay
                    .wait_path_published(EGRESS_PATH, "moqSession", RECONNECT_BUDGET)
                    .await
            }
            Err(error) => Err(error),
        };
        let reconnect_secs = restarted_at.elapsed().as_secs_f64();
        let reconnected = republished.is_ok();
        self.checks.record(
            "isolationMoqReconnected",
            reconnected,
            json!({
                "secsAfterRestart": round3(reconnect_secs),
                "budgetSecs": RECONNECT_BUDGET.as_secs(),
                "path": republished.as_ref().ok(),
                "error": republished.as_ref().err(),
            }),
        );
        tokio::time::sleep(POST_RECONNECT_HOLD).await;
        sampler_stop.cancel();
        let samples = sampler.await.unwrap_or_default();

        // The sibling is judged from the kill onwards: before it, nothing
        // about the relay has changed.
        let since_kill = killed_at.saturating_duration_since(sampler_started);
        let window: Vec<(Duration, u64)> = samples
            .iter()
            .copied()
            .filter(|(at, _)| *at >= since_kill)
            .collect();
        let stall = longest_counter_stall(&window);
        let connections_after = self.sink_metrics.connections.load(Ordering::Relaxed);
        let frames = match (window.first(), window.last()) {
            (Some(first), Some(last)) => last.1.saturating_sub(first.1),
            _ => 0,
        };
        self.checks.record(
            "isolationSiblingUninterrupted",
            window.len() > 1
                && frames > 0
                && stall <= SIBLING_MAX_STALL
                && connections_after == connections_before,
            json!({
                "longestStallMs": stall.as_millis() as u64,
                "maxStallMs": SIBLING_MAX_STALL.as_millis() as u64,
                "videoFramesSinceKill": frames,
                "samples": window.len(),
                "sinkConnectionsBefore": connections_before,
                "sinkConnectionsAfter": connections_after,
            }),
        );

        if reconnected {
            let probe = ffprobe_live_sample(
                &self.relay.rtsp_url(EGRESS_PATH),
                &self.work_dir.join("reconnect-probe.json"),
                SHORT_READ_SECS,
                "5M",
                "5M",
            )
            .await;
            self.checks.record_sample(
                "isolationReadBackAfterReconnect",
                probe,
                self.input_fps,
                SHORT_MIN_SPAN_SECS,
            );
        }
        self.record_output_status("isolationMoqOutput", pipeline, moq_output)
            .await;
        Ok(())
    }

    /// FFmpeg RTSP → MediaMTX path forwarded over MoQ → Restream ingest →
    /// RTMP output back into MediaMTX, read back with the same checks.
    async fn ingest_phase(&mut self) -> Result<(), String> {
        let pipeline = create_resource_pipeline(&self.api, "moq-ingest", INGEST_STREAM_KEY).await?;
        let return_url = self.relay.rtmp_url(INGEST_RETURN_PATH);
        self.add_output(&pipeline, "rtmp-return", &return_url)
            .await?;
        let source_url = self.relay.rtsp_url(INGEST_SOURCE_PATH);
        self.spawn_publisher(
            "ingest-publisher",
            &rtsp_publisher_args(&self.fixture, &source_url),
        )?;

        let live = async {
            wait_for_api_input_live(&self.api, &pipeline, INPUT_TIMEOUT).await?;
            wait_for_api_input_media_ready(&self.api, &pipeline, INPUT_TIMEOUT).await
        }
        .await;
        let probe = self
            .api
            .get_json(&format!("/api/v1/pipelines/{pipeline}/probe"))
            .await
            .unwrap_or(Value::Null);
        let protocol = probe.pointer("/ingest/protocol").and_then(Value::as_str);
        let video = probe.pointer("/video/codec").and_then(Value::as_str);
        let audio_tracks = probe
            .get("audioTracks")
            .and_then(Value::as_array)
            .map_or(0, Vec::len);
        self.checks.record(
            "ingestMoqSession",
            live.is_ok() && protocol == Some("moq") && video == Some("h264") && audio_tracks > 0,
            json!({
                "error": live.as_ref().err(),
                "protocol": protocol,
                "videoCodec": video,
                "audioTracks": audio_tracks,
                "bytesReceived": probe.pointer("/ingest/bytesReceived"),
            }),
        );
        if live.is_err() {
            return Err("the MoQ ingest never went live".to_string());
        }

        let returned = self
            .relay
            .wait_path_published(INGEST_RETURN_PATH, "rtmpConn", RELAY_PUBLISH_TIMEOUT)
            .await;
        if let Err(error) = returned {
            self.checks
                .record("ingestReadBack", false, json!({"error": error}));
            return Ok(());
        }
        let probe = ffprobe_live_sample(
            &self.relay.rtsp_url(INGEST_RETURN_PATH),
            &self.work_dir.join("ingest-probe.json"),
            SHORT_READ_SECS,
            "5M",
            "5M",
        )
        .await;
        self.checks
            .record_sample("ingestReadBack", probe, self.input_fps, SHORT_MIN_SPAN_SECS);
        Ok(())
    }

    async fn add_output(
        &mut self,
        pipeline: &str,
        name: &str,
        url: &str,
    ) -> Result<String, String> {
        let output = create_output(&self.api, pipeline, name, url, "source").await?;
        start_output(&self.api, pipeline, &output).await?;
        self.outputs.push((pipeline.to_string(), output.clone()));
        Ok(output)
    }

    async fn record_output_status(&mut self, key: &str, pipeline: &str, output: &str) {
        let status = self
            .api
            .get_output_status(pipeline, output)
            .await
            .map(|(_, raw)| raw)
            .unwrap_or_else(|error| json!({"error": error}));
        self.report.insert(key.to_string(), status);
    }

    fn spawn_publisher(&mut self, label: &str, argv: &[String]) -> Result<(), String> {
        let log_path = self.work_dir.join(format!("{label}.log"));
        let log = std::fs::File::create(&log_path).map_err(|error| error.to_string())?;
        let stderr = log.try_clone().map_err(|error| error.to_string())?;
        let child = command_with_optional_cgroup(&self.ffmpeg, &format!("moq-{label}"))
            .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
            .args(argv)
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", self.ffmpeg.display()))?;
        self.publishers.push(child);
        Ok(())
    }

    async fn shutdown(&mut self) {
        for mut publisher in self.publishers.drain(..) {
            stop_child(&mut publisher).await;
        }
        for (pipeline, output) in &self.outputs {
            let _ = self
                .api
                .post_null(&format!(
                    "/api/v1/pipelines/{pipeline}/outputs/{output}/stop"
                ))
                .await;
        }
        if let Some(sink) = self.sink.take() {
            stop_generalized_sink_server(sink);
        }
        stop_child(&mut self.restream).await;
        self.relay.kill().await;
    }
}

/// The RTMP sibling's video counter every [`SIBLING_SAMPLE_PERIOD`], as
/// `(elapsed since started, videoCount)`, until cancelled.
pub(crate) async fn sample_sibling(
    metrics: Arc<GeneralizedSinkMetrics>,
    started: Instant,
    stop: CancellationToken,
) -> Vec<(Duration, u64)> {
    let mut samples = Vec::new();
    let mut tick = tokio::time::interval(SIBLING_SAMPLE_PERIOD);
    loop {
        tokio::select! {
            _ = stop.cancelled() => break,
            _ = tick.tick() => {
                samples.push((started.elapsed(), metrics.video_count.load(Ordering::Relaxed)));
            }
        }
    }
    samples
}

/// FFmpeg publishing the fixture into a MediaMTX RTSP path in real time.
fn rtsp_publisher_args(fixture: &Path, url: &str) -> Vec<String> {
    let fixture = fixture.display().to_string();
    [
        "-re",
        "-stream_loop",
        "-1",
        "-i",
        fixture.as_str(),
        "-map",
        "0:v",
        "-map",
        "0:a:0",
        "-c",
        "copy",
        "-f",
        "rtsp",
        "-rtsp_transport",
        "tcp",
        url,
    ]
    .iter()
    .map(|arg| arg.to_string())
    .collect()
}

/// The fixture's video frame rate, the reference for frame-count checks.
pub(crate) async fn fixture_frame_rate(fixture: &Path) -> Result<f64, String> {
    let output = tokio::time::timeout(
        Duration::from_secs(15),
        Command::new("ffprobe")
            .args([
                "-v",
                "error",
                "-select_streams",
                "v:0",
                "-show_entries",
                "stream=avg_frame_rate",
                "-of",
                "json",
            ])
            .arg(fixture)
            .kill_on_drop(true)
            .output(),
    )
    .await
    .map_err(|_| format!("ffprobe timed out on {}", fixture.display()))?
    .map_err(|error| format!("spawn ffprobe: {error}"))?;
    let probe: Value = serde_json::from_slice(&output.stdout).map_err(|error| error.to_string())?;
    probe
        .pointer("/streams/0/avg_frame_rate")
        .and_then(Value::as_str)
        .and_then(parse_frame_rate)
        .ok_or_else(|| format!("no video frame rate in {}: {probe}", fixture.display()))
}
