//! `moq-dev-interop` execution: egress, origin, browsers, isolation and
//! ingest against the pinned moq.dev tools, with every early return tearing
//! all processes down.

use super::*;

const MODE: &str = "moq-dev-interop";
/// B-frame free (decode order is presentation order, so the PTS check is
/// strict), 30 fps H.264 High with stereo AAC.
const FIXTURE: &str = "test/fixtures/transport/av-marker-h264-bf0.ts";
const EGRESS_STREAM_KEY: &str = "sk-moq-dev-egress";
const INGEST_STREAM_KEY: &str = "sk-moq-dev-ingest";
/// The broadcast Restream's egress announces on the relay.
const EGRESS_BROADCAST: &str = "live/egress.hang";

/// Media the egress read must span (the contract's ≥ 20 s window); the
/// export runs longer because it starts at the next group.
const EGRESS_MIN_SPAN_SECS: f64 = 20.0;
const EGRESS_CAPTURE_SECS: u64 = 24;
/// Origin, post-reconnect and ingest reads: shorter, same checks.
const SHORT_MIN_SPAN_SECS: f64 = 8.0;
const SHORT_CAPTURE_SECS: u64 = 11;
/// A refused subscription must end within this window, without media.
const REFUSAL_WATCH_SECS: u64 = 10;
/// IETF has no subscriber role in SETUP: a no-token viewer enters publisher
/// admission and reaches its 10 s announcement timeout after setup (also
/// bounded to 10 s). Leave another 10 s for startup and closing the CLI.
const IETF_REFUSAL_WATCH_SECS: u64 = 30;
/// The isolation phase lets the stream run this long before the kill.
const OUTAGE_AFTER: Duration = Duration::from_secs(10);
/// The relay stays down this long before the restart.
const OUTAGE_DOWN: Duration = Duration::from_secs(3);
/// The MoQ output must publish again within this budget after the restart.
const RECONNECT_BUDGET: Duration = Duration::from_secs(60);
/// Sibling sampling continues this long after the reconnect.
const POST_RECONNECT_HOLD: Duration = Duration::from_secs(5);
const INPUT_TIMEOUT: Duration = Duration::from_secs(60);
const RELAY_PUBLISH_TIMEOUT: Duration = Duration::from_secs(60);
/// Two browsers × two endpoints × (startup + 10 s watch), with headroom.
const BROWSER_TIMEOUT: Duration = Duration::from_secs(360);
/// The browser specs' own package, relative to the repository root.
const BROWSER_SPEC_DIR: &str = "test/e2e/moq";

/// Restream's MoQ listener settings (`src/config/moq.rs`).
const MOQ_INGEST_LISTEN_ENV: &str = "RESTREAM_MOQ_INGEST_LISTEN";
const MOQ_TLS_CERT_ENV: &str = "RESTREAM_MOQ_TLS_CERT";
const MOQ_TLS_KEY_ENV: &str = "RESTREAM_MOQ_TLS_KEY";

/// Everything the run owns, so teardown reaches all of it from any phase.
struct Session {
    options: DevOptions,
    work_dir: PathBuf,
    ffmpeg: PathBuf,
    moq: PathBuf,
    fixture: PathBuf,
    input_fps: f64,
    tls: InteropTls,
    ports: DevPorts,
    relay: DevRelay,
    ingest_relay: DevRelay,
    restream: Child,
    api: RampApi,
    sink: Option<GeneralizedSinkServer>,
    sink_metrics: Arc<GeneralizedSinkMetrics>,
    sink_port: u16,
    restream_rtmp: u16,
    children: Vec<Child>,
    outputs: Vec<(String, String)>,
    checks: Checks,
    report: serde_json::Map<String, Value>,
}

pub(crate) async fn run_moq_dev_interop(options: DevOptions) -> Result<Value, String> {
    let started = Instant::now();
    let work_dir = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(if options.ietf {
                ".local/artifacts/moq-dev-interop-ietf"
            } else {
                ".local/artifacts/moq-dev-interop"
            })
        });
    std::fs::create_dir_all(&work_dir).map_err(|error| error.to_string())?;
    let work_dir = absolute_path(&work_dir)?;

    let moq = pinned_bin(&MOQ_CLI).await?;
    let relay_bin = pinned_bin(&MOQ_RELAY).await?;
    let fixture = restream::test_fixtures::checked_in_fixture(FIXTURE)?;
    let input_fps = fixture_frame_rate(&fixture).await?;
    let ffmpeg = restream::ffmpeg_binary::ffmpeg_bin_path().to_path_buf();
    let tls = InteropTls::generate(&work_dir.join("tls")).await?;
    let ports = TestPorts::from_env();
    let dev_ports = DevPorts::allocate();
    let sink_port = harness_port_defaults().sink;
    println!(
        "[{MODE}] {}",
        json!({
            "phase": "config",
            "moq": moq.display().to_string(),
            "moqRelay": relay_bin.display().to_string(),
            "fixture": fixture.display().to_string(),
            "inputFps": input_fps,
            "browsers": options.browsers,
            "alpn": options.alpn(),
            "ports": {"http": ports.http, "rtmp": ports.rtmp, "sink": sink_port},
            "devPorts": dev_ports.to_json(),
        })
    );

    let mut relay = DevRelay::new(
        relay_bin.clone(),
        dev_ports.relay,
        &tls,
        work_dir.join("moq-relay.log"),
    );
    relay.start().await?;

    let env_overrides = [
        (
            MOQ_INGEST_LISTEN_ENV,
            format!("127.0.0.1:{}", dev_ports.restream_moq),
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
        &default_work_db_path(&work_dir, "moq-dev-interop.sqlite"),
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

    let ingest_relay = DevRelay::new(
        relay_bin.clone(),
        dev_ports.ingest_relay,
        &tls,
        work_dir.join("moq-relay-ingest.log"),
    );
    let mut session = Session {
        options,
        work_dir: work_dir.clone(),
        ffmpeg,
        moq: moq.clone(),
        fixture,
        input_fps,
        tls,
        ports: dev_ports,
        relay,
        ingest_relay,
        restream,
        api,
        sink: None,
        sink_metrics: Arc::new(GeneralizedSinkMetrics::default()),
        sink_port,
        restream_rtmp: ports.rtmp,
        children: Vec::new(),
        outputs: Vec::new(),
        checks: Checks::new(MODE),
        report: serde_json::Map::new(),
    };
    let outcome = session.execute().await;
    session.shutdown().await;
    let relay_sessions = std::fs::read_to_string(session.relay.log_path())
        .map(|log| relay_sessions(&log))
        .unwrap_or_default();

    let passed = outcome.is_ok() && session.checks.failures.is_empty();
    let mut failures = std::mem::take(&mut session.checks.failures);
    if let Err(error) = &outcome {
        failures.push(format!("aborted: {error}"));
    }
    let report = json!({
        "passed": passed,
        "mode": MODE,
        "wireProfile": if options.ietf { "moqt-19 + MSF/LOC" } else { "moq-lite-06 + hang" },
        "environment": {
            "moq": moq.display().to_string(),
            "moqVersion": MOQ_CLI.version,
            "moqRelay": relay_bin.display().to_string(),
            "moqRelayVersion": MOQ_RELAY.version,
            "restreamBin": default_restream_bin().display().to_string(),
            "fixture": session.fixture.display().to_string(),
            "inputFps": session.input_fps,
            "leafSha256": session.tls.leaf_sha256,
            "ports": {"http": ports.http, "rtmp": ports.rtmp, "sink": sink_port},
            "devPorts": dev_ports.to_json(),
        },
        "relaySessions": relay_sessions.iter().map(RelaySession::to_json).collect::<Vec<_>>(),
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
    /// The origin and browser phases need the egress pipeline; ingest does
    /// not, so it runs even when egress aborted: one run reports both
    /// directions.
    async fn execute(&mut self) -> Result<(), String> {
        let egress = match self.egress_phase().await {
            Ok((pipeline, moq_output)) => {
                self.origin_phase(&pipeline).await;
                if self.options.browsers {
                    self.browser_phase(&pipeline).await;
                }
                self.isolation_phase(&pipeline, &moq_output).await
            }
            Err(error) => Err(error),
        };
        let ingest = self.ingest_phase().await;
        egress.and(ingest)
    }

    fn relay_target(&self) -> (String, &'static str) {
        (self.relay.connect_url(), EGRESS_BROADCAST)
    }

    /// Restream's own MoQ listener (IPv4 loopback, like the relay's
    /// [`DevRelay::connect_url`]); publishers authenticate by stream key.
    fn listener_url(&self) -> String {
        format!("https://127.0.0.1:{}/", self.ports.restream_moq)
    }

    /// Mint an origin subscribe grant for `pipeline` with the harness's API
    /// session. Its token admits for 60 s, so every subscription mints its
    /// own.
    async fn origin_grant(&self, pipeline: &str) -> Result<OriginGrant, String> {
        let grant = self.api.post_empty(&origin_token_path(pipeline)).await?;
        OriginGrant::parse(&grant, self.ports.restream_moq)
    }

    /// RTMP publisher → pipeline → `moqt://` moq-relay + RTMP sibling sink;
    /// the relay must serve the broadcast, and it reads back correctly.
    /// Returns the pipeline and its MoQ output.
    async fn egress_phase(&mut self) -> Result<(String, String), String> {
        let pipeline =
            create_resource_pipeline(&self.api, "moq-dev-egress", EGRESS_STREAM_KEY).await?;
        self.sink =
            Some(start_generalized_sink_server(self.sink_port, self.sink_metrics.clone()).await?);
        let moq_url = format!(
            "moqt://127.0.0.1:{}/?broadcast={EGRESS_BROADCAST}&version={}&ca=ca.pem",
            self.ports.relay,
            self.options.alpn(),
        );
        let moq_output = self.add_output(&pipeline, "moq-relay", &moq_url).await?;
        let sibling_url = format!("rtmp://127.0.0.1:{}/live/moq-sibling", self.sink_port);
        self.add_output(&pipeline, "rtmp-sibling", &sibling_url)
            .await?;
        let publish_url = format!(
            "rtmp://127.0.0.1:{}/live/{EGRESS_STREAM_KEY}",
            self.restream_rtmp
        );
        self.spawn_ffmpeg(
            "egress-publisher",
            &crate::hls_push_duty::run::rtmp_publisher_args(&self.fixture, &publish_url),
        )?;
        wait_for_api_input_live(&self.api, &pipeline, INPUT_TIMEOUT).await?;
        wait_for_api_input_media_ready(&self.api, &pipeline, INPUT_TIMEOUT).await?;

        let catalog = self
            .relay
            .wait_broadcast(EGRESS_BROADCAST, RELAY_PUBLISH_TIMEOUT, self.options.ietf)
            .await;
        let served = catalog.is_ok();
        self.checks.record(
            "egressRelayCatalog",
            served,
            catalog.unwrap_or_else(|error| json!({"error": error})),
        );
        self.record_relay_sessions("egressRelayVersion", 1);
        if !served {
            self.record_output_status("egressMoqOutput", &pipeline, &moq_output)
                .await;
            return Err("the MoQ output never published to moq-relay".to_string());
        }

        let (url, broadcast) = self.relay_target();
        self.record_export(
            "egressReadBack",
            &url,
            broadcast,
            EGRESS_CAPTURE_SECS,
            EGRESS_MIN_SPAN_SECS,
        )
        .await;
        self.record_output_status("egressMoqOutput", &pipeline, &moq_output)
            .await;
        Ok((pipeline, moq_output))
    }

    /// The egress pipeline read straight from Restream's origin with a minted
    /// token; the same subscription without a token, or with a tampered
    /// one, must be refused.
    async fn origin_phase(&mut self, pipeline: &str) {
        let grant = match self.origin_grant(pipeline).await {
            Ok(grant) => grant,
            Err(error) => {
                self.checks
                    .record("originToken", false, json!({"error": error}));
                return;
            }
        };
        self.record_export(
            "originReadBack",
            &grant.url,
            &grant.broadcast,
            SHORT_CAPTURE_SECS,
            SHORT_MIN_SPAN_SECS,
        )
        .await;
        let refusals = [
            ("originRefusesNoToken", with_token(&grant.url, None)),
            (
                "originRefusesBadToken",
                with_token(&grant.url, Some(&tampered(&grant.token))),
            ),
        ];
        for (id, variant) in refusals {
            match variant {
                Ok(variant) => self.record_refusal(id, &variant, &grant.broadcast).await,
                Err(error) => self.checks.record(id, false, json!({"error": error})),
            }
        }
    }

    /// One `moq export` that `url` must refuse without media, allowing the
    /// profile's admission/close bounds; the CLI's reason is recorded.
    async fn record_refusal(&mut self, id: &str, url: &str, broadcast: &str) {
        let out = self.work_dir.join(format!("{id}.ts"));
        let target = CliTarget {
            url,
            ca: &self.tls.ca_pem,
            broadcast,
            version: self.options.version(),
        };
        let timeout = if self.options.ietf {
            IETF_REFUSAL_WATCH_SECS
        } else {
            REFUSAL_WATCH_SECS
        };
        match attempt_export(&self.moq, &target, &out, timeout).await {
            Ok(refusal) => self.checks.record(id, refusal.refused(), refusal.to_json()),
            Err(error) => self.checks.record(id, false, json!({"error": error})),
        }
    }

    /// Playwright Chromium + Firefox play the origin and the relay, and the
    /// origin must refuse the page without a token; every (browser, test)
    /// pair becomes a check, a skipped one a fallback row. A token admits
    /// for 60 s, less than a browser run takes to reach its last test, so
    /// the spec mints one per test with the harness's API session.
    async fn browser_phase(&mut self, pipeline: &str) {
        let (relay_url, relay_broadcast) = self.relay_target();
        let targets = json!([
            {
                "id": "origin",
                "url": self.listener_url(),
                "name": pipeline,
                "tokenPath": origin_token_path(pipeline),
            },
            {"id": "relay", "url": relay_url, "name": relay_broadcast},
        ]);
        let outcomes = match self.run_playwright(&targets).await {
            Ok(outcomes) => outcomes,
            Err(error) => {
                self.checks
                    .record("browserRun", false, json!({"error": error}));
                return;
            }
        };
        for (id, verdict, detail) in browser_rows(&outcomes, &[("origin", true), ("relay", false)])
        {
            match verdict {
                BrowserVerdict::Pass => self.checks.record(&id, true, detail),
                BrowserVerdict::Fail => self.checks.record(&id, false, detail),
                BrowserVerdict::Fallback => {
                    println!("[{MODE}] {}", json!({"check": id, "fallback": true}));
                    self.checks
                        .rows
                        .push(json!({"id": id, "ok": null, "fallback": true, "detail": detail}));
                }
            }
        }
    }

    async fn run_playwright(&self, targets: &Value) -> Result<Vec<BrowserOutcome>, String> {
        let spec_dir = absolute_path(Path::new(BROWSER_SPEC_DIR))?;
        if !spec_dir.join("node_modules/@moq/watch").is_dir() {
            return Err(format!(
                "{} has no node_modules; run `(cd {BROWSER_SPEC_DIR} && npm ci && \
                 npx playwright install chromium firefox)`",
                spec_dir.display()
            ));
        }
        let out_dir = self.work_dir.join("browser");
        std::fs::create_dir_all(&out_dir).map_err(|error| error.to_string())?;
        let report_path = out_dir.join("playwright-report.json");
        let _ = std::fs::remove_file(&report_path);
        let log = std::fs::File::create(out_dir.join("playwright.log"))
            .map_err(|error| error.to_string())?;
        let stderr = log.try_clone().map_err(|error| error.to_string())?;
        let child = Command::new("npx")
            .args(["--no-install", "playwright", "test"])
            .current_dir(&spec_dir)
            .env("MOQ_BROWSER_TARGETS", targets.to_string())
            .env("MOQ_BROWSER_API", &self.api.base_url)
            .env(
                "MOQ_BROWSER_API_COOKIE",
                self.api.cookie.as_deref().unwrap_or_default(),
            )
            .env("MOQ_BROWSER_CERT_SHA256", &self.tls.leaf_sha256)
            .env("MOQ_BROWSER_PAGE_PORT", self.ports.page.to_string())
            .env("MOQ_BROWSER_REPORT", &report_path)
            .env("MOQ_BROWSER_OUTPUT_DIR", out_dir.join("results"))
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("spawn npx playwright: {error}"))?;
        let status = tokio::time::timeout(BROWSER_TIMEOUT, child.wait_with_output())
            .await
            .map_err(|_| format!("playwright exceeded {}s", BROWSER_TIMEOUT.as_secs()))?
            .map_err(|error| format!("wait for playwright: {error}"))?
            .status;
        // A failing spec exits non-zero but still writes the report, which
        // is what the checks are built from.
        let report = std::fs::read(&report_path).map_err(|error| {
            format!(
                "playwright ({status}) wrote no report at {}: {error}",
                report_path.display()
            )
        })?;
        let report: Value = serde_json::from_slice(&report).map_err(|error| error.to_string())?;
        Ok(browser_outcomes(&report))
    }

    /// Kill the relay mid-stream and restart it: the MoQ output must publish
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
                    .wait_broadcast(EGRESS_BROADCAST, RECONNECT_BUDGET, self.options.ietf)
                    .await
            }
            Err(error) => Err(error),
        };
        let reconnected = republished.is_ok();
        self.checks.record(
            "isolationMoqReconnected",
            reconnected,
            json!({
                "secsAfterRestart": round3(restarted_at.elapsed().as_secs_f64()),
                "budgetSecs": RECONNECT_BUDGET.as_secs(),
                "error": republished.as_ref().err(),
            }),
        );
        // Browsers/exports have dialled this relay life; only Restream has
        // dialled the new life yet, so this remains a publisher-only verdict.
        self.record_relay_sessions("isolationRelayVersion", 2);
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
            let (url, broadcast) = self.relay_target();
            self.record_export(
                "isolationReadBackAfterReconnect",
                &url,
                broadcast,
                SHORT_CAPTURE_SECS,
                SHORT_MIN_SPAN_SECS,
            )
            .await;
        }
        self.record_output_status("isolationMoqOutput", pipeline, moq_output)
            .await;
        Ok(())
    }

    /// Publisher → Restream ingest → RTMP read-back. IETF uses a dedicated
    /// real relay forwarding LOC from a separate source pipeline.
    async fn ingest_phase(&mut self) -> Result<(), String> {
        let pipeline =
            create_resource_pipeline(&self.api, "moq-dev-ingest", INGEST_STREAM_KEY).await?;
        let readback = self.work_dir.join("ingest-readback.ts");
        let listener = spawn_rtmp_readback(
            &self.ffmpeg,
            self.ports.readback_rtmp,
            &readback,
            SHORT_CAPTURE_SECS,
        )?;
        let listener_index = self.children.len();
        self.children.push(listener);
        wait_for_tcp_listener_ready(self.ports.readback_rtmp, Duration::from_secs(10)).await?;
        let return_url = format!(
            "rtmp://127.0.0.1:{}/{READBACK_RTMP_PATH}",
            self.ports.readback_rtmp
        );
        self.add_output(&pipeline, "rtmp-readback", &return_url)
            .await?;

        let broadcast = if self.options.ietf {
            format!("live/{INGEST_STREAM_KEY}.msf")
        } else {
            format!("live/{INGEST_STREAM_KEY}.hang")
        };
        if self.options.ietf {
            self.start_ietf_ingest_source(&broadcast).await?;
        } else {
            let url = self.listener_url();
            let target = CliTarget {
                url: &url,
                ca: &self.tls.ca_pem,
                broadcast: &broadcast,
                version: LITE_VERSION,
            };
            let publisher = spawn_ts_import(
                &self.ffmpeg,
                &self.moq,
                &self.fixture,
                &target,
                &self.work_dir,
                "ingest-publisher",
            )?;
            self.children.extend(publisher);
        }

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
                "broadcast": broadcast,
                "protocol": protocol,
                "videoCodec": video,
                "audioTracks": audio_tracks,
                "bytesReceived": probe.pointer("/ingest/bytesReceived"),
            }),
        );
        if live.is_err() {
            return Err("the MoQ ingest never went live".to_string());
        }
        if self.options.ietf {
            let log = std::fs::read_to_string(self.ingest_relay.log_path()).unwrap_or_default();
            let (ok, sessions) = sessions_on_version(&relay_sessions(&log), 1, IETF_VERSION);
            self.checks.record(
                "ingestRelayVersion",
                ok,
                json!({"expectedAlpn": "moqt-19", "sessions": sessions}),
            );
            // The relay offers exactly one version on its outbound cluster
            // dial; media admitted by Restream proves that dial completed.
            self.report.insert(
                "ingestRelayForward".to_string(),
                json!({
                    "url": format!("{}{INGEST_STREAM_KEY}", self.listener_url()),
                    "version": IETF_VERSION,
                    "broadcast": broadcast,
                    "log": self.ingest_relay.log_path(),
                }),
            );
        }

        // The listener exits once it has written SHORT_CAPTURE_SECS of media.
        let finished = match self.children.get_mut(listener_index) {
            Some(listener) => tokio::time::timeout(
                INPUT_TIMEOUT + Duration::from_secs(SHORT_CAPTURE_SECS),
                listener.wait(),
            )
            .await
            .map_err(|_| "the RTMP read-back listener never finished".to_string())
            .and_then(|status| status.map_err(|error| error.to_string())),
            None => Err("the RTMP read-back listener is gone".to_string()),
        };
        let probe = match finished.and_then(|_| captured_bytes(&readback)) {
            Ok(_) => self.probe_capture(&readback, SHORT_CAPTURE_SECS).await,
            Err(error) => Err(error),
        };
        self.checks
            .record_sample("ingestReadBack", probe, self.input_fps, SHORT_MIN_SPAN_SECS);
        Ok(())
    }

    async fn start_ietf_ingest_source(&mut self, broadcast: &str) -> Result<(), String> {
        self.ingest_relay.forward_to(
            format!("{}{INGEST_STREAM_KEY}", self.listener_url()),
            &self.tls.ca_pem,
        );
        self.ingest_relay.start().await?;
        let key = "sk-moq-ietf-source";
        let source = create_resource_pipeline(&self.api, "moq-ietf-source", key).await?;
        let output = format!(
            "moqt://127.0.0.1:{}/?broadcast={broadcast}&version=moqt-19&ca=ca.pem",
            self.ports.ingest_relay,
        );
        self.add_output(&source, "ietf-ingest-source", &output)
            .await?;
        let publish_url = format!("rtmp://127.0.0.1:{}/live/{key}", self.restream_rtmp);
        self.spawn_ffmpeg(
            "ietf-ingest-source",
            &crate::hls_push_duty::run::rtmp_publisher_args(&self.fixture, &publish_url),
        )?;
        let catalog = self
            .ingest_relay
            .wait_broadcast(broadcast, RELAY_PUBLISH_TIMEOUT, true)
            .await;
        let served = catalog.is_ok();
        self.checks.record(
            "ingestRelayMsfLocCatalog",
            served,
            catalog.unwrap_or_else(|error| json!({"error": error})),
        );
        if !served {
            return Err("the ingest relay never served the MSF/LOC source".to_string());
        }
        Ok(())
    }

    /// `moq export ts` from `url`/`broadcast` for `secs`, then the FFprobe
    /// read-back checks over what it delivered.
    async fn record_export(
        &mut self,
        id: &str,
        url: &str,
        broadcast: &str,
        secs: u64,
        min_span_secs: f64,
    ) {
        let capture = self.work_dir.join(format!("{id}.ts"));
        let target = CliTarget {
            url,
            ca: &self.tls.ca_pem,
            broadcast,
            version: self.options.version(),
        };
        let probe = match capture_export(&self.moq, &target, &capture, secs).await {
            Ok(_) => self.probe_capture(&capture, secs).await,
            Err(error) => Err(error),
        };
        self.checks
            .record_sample(id, probe, self.input_fps, min_span_secs);
    }

    async fn probe_capture(&self, capture: &Path, secs: u64) -> Result<Value, String> {
        ffprobe_live_sample(
            &capture.display().to_string(),
            &capture.with_extension("probe.json"),
            secs + 10,
            "5M",
            "5M",
        )
        .await
    }

    /// Before harness subscribers dial a relay life, all sessions must
    /// negotiate the selected version. Browser lite sessions from the old
    /// life do not contaminate the post-restart IETF verdict.
    fn record_relay_sessions(&mut self, id: &str, life: u32) {
        let log = std::fs::read_to_string(self.relay.log_path()).unwrap_or_default();
        let (ok, sessions) =
            sessions_on_version(&relay_sessions(&log), life, self.options.version());
        self.checks.record(
            id,
            ok,
            json!({"expected": self.options.version(), "expectedAlpn": self.options.alpn(), "relayLife": life, "sessions": sessions}),
        );
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

    fn spawn_ffmpeg(&mut self, label: &str, argv: &[String]) -> Result<(), String> {
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
        self.children.push(child);
        Ok(())
    }

    async fn shutdown(&mut self) {
        for mut child in self.children.drain(..) {
            stop_child(&mut child).await;
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
        self.ingest_relay.kill().await;
    }
}
