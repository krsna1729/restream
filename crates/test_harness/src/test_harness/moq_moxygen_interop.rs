//! `moq-moxygen-interop`: Restream's MoQ egress through moxygen, an
//! independent C++ MOQT implementation, on each IETF draft both speak.
//!
//! The peer is moxygen's `moqrelayserver` from a pinned commit, built by
//! `cargo xtask fetch moxygen` into
//! `.local/bin/moxygen-<commit>/moqrelayserver` (`MOQ_MOXYGEN_RELAY_BIN`
//! overrides the path; the `.commit` stamp beside it must name the pin).
//! For every draft in [`DRAFTS`] one relay runs offering only that draft:
//!
//! 1. An FFmpeg RTMP publisher (checked-in B-frame-free fixture) feeds one
//!    pipeline whose `moqt://` outputs each offer exactly one draft
//!    (`version=moqt-NN`) to that draft's relay.
//! 2. The relay log must show Restream's session negotiating `moqt-NN` and
//!    the broadcast's PUBLISH_NAMESPACE accepted.
//! 3. The pinned `moq` CLI subscribes through the moxygen relay on the same
//!    draft (`moq export --catalog-format msf ts`): MSF catalog, LOC media.
//!    FFprobe checks both codecs, monotonic timestamps and a video frame
//!    count within 5 % of the input frame rate over at least
//!    [`MIN_SPAN_SECS`] of media.
//!
//! moxygen is the relay here, so each read-back crosses moxygen's control
//! and object codecs twice: Restream publishing into it, the CLI reading
//! out of it.
//!
//! ```text
//! cargo xtask fetch moq-dev
//! cargo xtask fetch moxygen
//! cargo xtask harness moq-moxygen-interop -- --no-netns
//! ```

use crate::moq_dev_interop::{CliTarget, MOQ_CLI, capture_export, pinned_bin};
use crate::moq_relay_interop::{
    Checks, InteropTls, fixture_frame_rate, planned_harness_ports, round3,
};

use super::*;

const MODE: &str = "moq-moxygen-interop";
/// B-frame free (decode order is presentation order, so the PTS check is
/// strict), 30 fps H.264 High with stereo AAC.
const FIXTURE: &str = "test/fixtures/transport/av-marker-h264-bf0.ts";
const STREAM_KEY: &str = "sk-moq-moxygen";
/// The commit `cargo xtask fetch moxygen` builds.
const MOXYGEN_COMMIT: &str = "c392ebfc4d1dc39220fac3087b503416d06c47b9";
const MOXYGEN_RELAY_ENV: &str = "MOQ_MOXYGEN_RELAY_BIN";
/// The relay's raw-QUIC and WebTransport endpoint path.
const RELAY_ENDPOINT: &str = "/moq-relay";
/// The drafts moxygen supports (README: 16 stable, 18 in interop testing).
const DRAFTS: [u8; 2] = [16, 18];

const MIN_SPAN_SECS: f64 = 10.0;
const CAPTURE_SECS: u64 = 13;
const INPUT_TIMEOUT: Duration = Duration::from_secs(60);
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(60);
const RELAY_START_TIMEOUT: Duration = Duration::from_secs(10);

/// One moxygen relay offering exactly one draft.
struct MoxygenRelay {
    draft: u8,
    port: u16,
    log: PathBuf,
    child: Option<Child>,
}

impl MoxygenRelay {
    fn alpn(&self) -> String {
        format!("moqt-{}", self.draft)
    }

    /// The `moq` CLI's spelling of the draft.
    fn cli_version(&self) -> &'static str {
        match self.draft {
            16 => "moq-transport-16",
            18 => "moq-transport-18",
            other => unreachable!("draft {other} is not in DRAFTS"),
        }
    }

    fn broadcast(&self) -> String {
        format!("live/egress-d{}.msf", self.draft)
    }

    fn connect_url(&self) -> String {
        format!("moqt://127.0.0.1:{}{RELAY_ENDPOINT}", self.port)
    }

    async fn start(&mut self, bin: &Path, tls: &InteropTls) -> Result<(), String> {
        let log = std::fs::File::create(&self.log)
            .map_err(|error| format!("create {}: {error}", self.log.display()))?;
        let stderr = log.try_clone().map_err(|error| error.to_string())?;
        let child = command_with_optional_cgroup(bin, &format!("moxygen-d{}", self.draft))
            .arg("--cert")
            .arg(&tls.leaf_pem)
            .arg("--key")
            .arg(&tls.leaf_key)
            .args([
                "--port",
                &self.port.to_string(),
                "--endpoint",
                RELAY_ENDPOINT,
                "--versions",
                &self.draft.to_string(),
                "--logging",
                "DBG1",
            ])
            .stdin(Stdio::null())
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", bin.display()))?;
        self.child = Some(child);
        let registered = format!("Registering ALPN handlers: {}", self.alpn());
        self.wait_log(RELAY_START_TIMEOUT, |log| log.contains(&registered))
            .await
            .map_err(|log| format!("moxygen relay d{} did not start: {log}", self.draft))
    }

    /// Wait until the relay log satisfies `ready`; on timeout, the log tail.
    async fn wait_log(
        &mut self,
        timeout: Duration,
        ready: impl Fn(&str) -> bool,
    ) -> Result<(), String> {
        let deadline = Instant::now() + timeout;
        loop {
            let log = std::fs::read_to_string(&self.log).unwrap_or_default();
            if ready(&log) {
                return Ok(());
            }
            if let Some(child) = self.child.as_mut()
                && let Ok(Some(status)) = child.try_wait()
            {
                return Err(format!("exited {status}; {}", log_tail(&log)));
            }
            if Instant::now() >= deadline {
                return Err(log_tail(&log));
            }
            tokio::time::sleep(Duration::from_millis(250)).await;
        }
    }

    async fn kill(&mut self) {
        if let Some(mut child) = self.child.take() {
            stop_child(&mut child).await;
        }
    }
}

fn log_tail(log: &str) -> String {
    let lines: Vec<&str> = log.lines().rev().take(5).collect();
    lines.into_iter().rev().collect::<Vec<_>>().join("\n")
}

/// Whether the relay received the broadcast's PUBLISH_NAMESPACE and then
/// answered it with PUBLISH_NAMESPACE_OK on the same session. moxygen logs
/// the request on arrival, before deciding, so arrival alone is not consent.
fn namespace_status(log: &str, broadcast: &str) -> (bool, bool) {
    let request = format!("onPublishNamespaceImpl ns={broadcast} sess=");
    let mut lines = log.lines();
    let Some(session) = lines.by_ref().find_map(|line| {
        line.split(&request)
            .nth(1)
            .and_then(|rest| rest.split_whitespace().next())
    }) else {
        return (false, false);
    };
    let ok = format!(" sess={session}");
    let accepted = lines.any(|line| line.contains("publishNamespaceOk ") && line.ends_with(&ok));
    (true, accepted)
}

/// What one draft's relay log says about Restream's publishing session:
/// every negotiated ALPN and whether the broadcast's namespace was accepted.
fn relay_verdict(log: &str, alpn: &str, broadcast: &str) -> (bool, Value) {
    let negotiated: Vec<&str> = log
        .lines()
        .filter_map(|line| line.split("Negotiated ALPN: ").nth(1))
        .map(str::trim)
        .collect();
    let (received, accepted) = namespace_status(log, broadcast);
    let ok = accepted && !negotiated.is_empty() && negotiated.iter().all(|seen| *seen == alpn);
    (
        ok,
        json!({"expectedAlpn": alpn, "negotiated": negotiated, "publishNamespaceReceived": received, "publishNamespaceAccepted": accepted}),
    )
}

/// Resolve the pinned relay: the binary and its `.commit` stamp.
fn moxygen_relay_bin() -> Result<PathBuf, String> {
    let bin = std::env::var_os(MOXYGEN_RELAY_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            PathBuf::from(format!(
                ".local/bin/moxygen-{}/moqrelayserver",
                &MOXYGEN_COMMIT[..8]
            ))
        });
    let bin = absolute_path(&bin)?;
    let hint = "run cargo xtask fetch moxygen";
    if !bin.is_file() {
        return Err(format!(
            "moxygen relay not found at {}; {hint}",
            bin.display()
        ));
    }
    let stamp = bin.with_extension("commit");
    let commit = std::fs::read_to_string(&stamp).unwrap_or_default();
    if commit.trim() != MOXYGEN_COMMIT {
        return Err(format!(
            "{} was built from {:?}, the interop run is pinned to {MOXYGEN_COMMIT}; {hint}",
            bin.display(),
            commit.trim()
        ));
    }
    Ok(bin)
}

/// Entry point for the `moq-moxygen-interop` command; it takes no options
/// beyond the harness's own `--no-netns`.
pub(crate) async fn moq_moxygen_interop(args: &[String]) -> Result<Value, String> {
    let args = strip_netns_opt(args);
    if let Some(unknown) = args.first() {
        return Err(format!(
            "{MODE} takes no options (got {unknown:?}); usage: {MODE} [--no-netns]"
        ));
    }
    let started = Instant::now();
    let work_dir = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".local/artifacts/moq-moxygen-interop"));
    std::fs::create_dir_all(&work_dir).map_err(|error| error.to_string())?;
    let work_dir = absolute_path(&work_dir)?;

    let moq = pinned_bin(&MOQ_CLI).await?;
    let relay_bin = moxygen_relay_bin()?;
    let fixture = restream::media::test_fixtures::checked_in_fixture(FIXTURE)?;
    let input_fps = fixture_frame_rate(&fixture).await?;
    let ffmpeg = restream::media::ffmpeg_binary::ffmpeg_bin_path().to_path_buf();
    let tls = InteropTls::generate(&work_dir.join("tls")).await?;
    let ports = TestPorts::from_env();
    let mut reserved = planned_harness_ports();
    let mut relays: Vec<MoxygenRelay> = DRAFTS
        .iter()
        .zip(4446u16..)
        .map(|(&draft, default)| MoxygenRelay {
            draft,
            port: env_or_allocated_port(
                &format!("MOQ_MOXYGEN_INTEROP_D{draft}"),
                default,
                &mut reserved,
            ),
            log: work_dir.join(format!("moxygen-d{draft}.log")),
            child: None,
        })
        .collect();

    let mut checks = Checks::new(MODE);
    let mut children: Vec<Child> = Vec::new();
    let mut restream: Option<Child> = None;
    let outcome = async {
        for relay in &mut relays {
            relay.start(&relay_bin, &tls).await?;
        }
        let env_overrides = [(
            "RESTREAM_MOQ_CA_DIR",
            tls.ca_pem
                .parent()
                .expect("CA directory")
                .display()
                .to_string(),
        )];
        restream = Some(
            start_restream_child_with_env(
                &default_restream_bin(),
                &ports,
                &default_work_db_path(&work_dir, "moq-moxygen-interop.sqlite"),
                &work_dir.join("restream.log"),
                &env_overrides,
            )
            .await?,
        );
        let api = login_api(&ports).await?;
        let pipeline = create_resource_pipeline(&api, "moq-moxygen-egress", STREAM_KEY).await?;
        for relay in &relays {
            let url = format!(
                "moqt://127.0.0.1:{}{RELAY_ENDPOINT}?broadcast={}&version={}&ca=ca.pem",
                relay.port,
                relay.broadcast(),
                relay.alpn()
            );
            let output = create_output(
                &api,
                &pipeline,
                &format!("moxygen-d{}", relay.draft),
                &url,
                "source",
            )
            .await?;
            start_output(&api, &pipeline, &output).await?;
        }
        let publish_url = format!("rtmp://127.0.0.1:{}/live/{STREAM_KEY}", ports.rtmp);
        let log = std::fs::File::create(work_dir.join("publisher.log"))
            .map_err(|error| error.to_string())?;
        let stderr = log.try_clone().map_err(|error| error.to_string())?;
        children.push(
            command_with_optional_cgroup(&ffmpeg, "moq-moxygen-publisher")
                .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
                .args(crate::hls_push_duty::run::rtmp_publisher_args(
                    &fixture,
                    &publish_url,
                ))
                .stdout(Stdio::from(log))
                .stderr(Stdio::from(stderr))
                .kill_on_drop(true)
                .spawn()
                .map_err(|error| format!("spawn {}: {error}", ffmpeg.display()))?,
        );
        wait_for_api_input_live(&api, &pipeline, INPUT_TIMEOUT).await?;
        wait_for_api_input_media_ready(&api, &pipeline, INPUT_TIMEOUT).await?;

        for relay in &mut relays {
            let draft = relay.draft;
            let (alpn, broadcast) = (relay.alpn(), relay.broadcast());
            let published = relay
                .wait_log(PUBLISH_TIMEOUT, |log| namespace_status(log, &broadcast).1)
                .await;
            let log = std::fs::read_to_string(&relay.log).unwrap_or_default();
            let (ok, mut detail) = relay_verdict(&log, &alpn, &broadcast);
            if let Err(tail) = &published {
                detail["logTail"] = json!(tail);
            }
            checks.record(&format!("d{draft}RestreamPublish"), ok, detail);
            if !ok {
                continue;
            }
            let url = relay.connect_url();
            let capture = work_dir.join(format!("d{draft}-readback.ts"));
            let target = CliTarget {
                url: &url,
                ca: &tls.ca_pem,
                broadcast: &broadcast,
                version: relay.cli_version(),
            };
            let probe = match capture_export(&moq, &target, &capture, CAPTURE_SECS).await {
                Ok(_) => {
                    ffprobe_live_sample(
                        &capture.display().to_string(),
                        &capture.with_extension("probe.json"),
                        CAPTURE_SECS + 10,
                        "5M",
                        "5M",
                    )
                    .await
                }
                Err(error) => Err(error),
            };
            checks.record_sample(
                &format!("d{draft}ReadBack"),
                probe,
                input_fps,
                MIN_SPAN_SECS,
            );
        }
        Ok::<(), String>(())
    }
    .await;

    for mut child in children.drain(..) {
        stop_child(&mut child).await;
    }
    if let Some(mut child) = restream.take() {
        stop_child(&mut child).await;
    }
    for relay in &mut relays {
        relay.kill().await;
    }

    let passed = outcome.is_ok() && checks.failures.is_empty();
    let mut failures = std::mem::take(&mut checks.failures);
    if let Err(error) = &outcome {
        failures.push(format!("aborted: {error}"));
    }
    let report = json!({
        "passed": passed,
        "mode": MODE,
        "wireProfile": "MOQT drafts 16 and 18 + MSF/LOC through moxygen",
        "environment": {
            "moq": moq.display().to_string(),
            "moqVersion": MOQ_CLI.version,
            "moxygenRelay": relay_bin.display().to_string(),
            "moxygenCommit": MOXYGEN_COMMIT,
            "restreamBin": default_restream_bin().display().to_string(),
            "fixture": fixture.display().to_string(),
            "inputFps": input_fps,
            "relayPorts": relays.iter().map(|relay| json!({"draft": relay.draft, "port": relay.port})).collect::<Vec<_>>(),
        },
        "checks": std::mem::take(&mut checks.rows),
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

#[cfg(test)]
mod tests {
    use super::*;

    const LOG: &str = "\
V1023 MoQServer.cpp:199] Server: Negotiated ALPN: moqt-16
V1023 MoQRelaySession.cpp:1082] onPublishNamespaceImpl ns=live/egress-d16.msf sess=0x1
V1023 MoQServer.cpp:199] Server: Negotiated ALPN: moqt-16
V1023 MoQRelaySession.cpp:1173] publishNamespaceOk reqID=0 sess=0x1
";

    #[test]
    fn relay_verdict_needs_the_accepted_namespace_and_only_the_pinned_draft() {
        assert!(relay_verdict(LOG, "moqt-16", "live/egress-d16.msf").0);
        // Another broadcast, or a prefix of it, is not this one.
        assert!(!relay_verdict(LOG, "moqt-16", "live/egress-d16").0);
        assert!(!relay_verdict(LOG, "moqt-16", "live/egress-d18.msf").0);
        // A session on another draft fails the pin.
        let mixed = format!("{LOG}V1023 Server: Negotiated ALPN: moqt-18\n");
        assert!(!relay_verdict(&mixed, "moqt-16", "live/egress-d16.msf").0);
        // No session at all proves nothing.
        assert!(!relay_verdict("", "moqt-16", "live/egress-d16.msf").0);
        // Received but never answered, or answered on another session, is not accepted.
        let unanswered = LOG.replace("publishNamespaceOk reqID=0 sess=0x1\n", "");
        let (ok, detail) = relay_verdict(&unanswered, "moqt-16", "live/egress-d16.msf");
        assert!(!ok);
        assert_eq!(detail["publishNamespaceReceived"], true);
        let other = LOG.replace("reqID=0 sess=0x1", "reqID=0 sess=0x2");
        assert!(!relay_verdict(&other, "moqt-16", "live/egress-d16.msf").0);
    }
}
