//! `moq-impairment`: a headless MoQ subscriber against a live Restream MoQ
//! output under packet loss and delay.
//!
//! One RTMP publisher feeds a pipeline whose `moqt://` output publishes
//! `live/impairment.hang` to the pinned moq-relay. Once the relay serves the
//! broadcast's catalog, the harness applies a `netem` qdisc on loopback
//! (2 % loss, 200 ms delay) to the relay's QUIC port only, then runs
//! `moq_subscriber` over the production moq-net/hang path:
//!
//! - startup delay: catalog-ready publishing to the first delivered frame;
//! - video frames/keyframes, audio frames/bytes;
//! - A/V arrival skew: the wall-clock gap between the two tracks delivering
//!   the same media instant (frames matched within 25 ms), after a 2 s join
//!   warmup. A retransmission stall is a delivery transient caught by the
//!   frame counts, not an arrival-skew claim.
//!
//! The browser E2E (`moq-dev-interop --browsers`) is unchanged; this is the
//! protocol-under-impairment evidence beside it. The mode needs the default
//! private network namespace with CAP_NET_ADMIN (`--no-netns` cannot apply
//! tc and fails loudly).
//!
//! ```text
//! scripts/dev/moq/fetch-moq-dev.sh
//! scripts/harness/run.sh moq-impairment
//! ```

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use crate::moq_dev_interop::{DevPorts, DevRelay, LITE_VERSION, MOQ_RELAY, pinned_bin};
use crate::moq_relay_interop::{InteropTls, round3};

use super::*;

const MODE: &str = "moq-impairment";
const FIXTURE: &str = "test/fixtures/transport/av-marker-h264-bf0.ts";
const STREAM_KEY: &str = "sk-moq-impairment";
const BROADCAST: &str = "live/impairment.hang";
/// The accepted gate: 2 % loss and 200 ms delay on the relay's QUIC port.
const LOSS_PERCENT: f64 = 2.0;
const DELAY_MS: u64 = 200;
/// Long enough to cross several 2 s GOPs at the fixture's 30 fps.
const WATCH_SECS: u64 = 15;
/// The subscriber bounds its own connect/catalog/window; this is only the
/// harness's outer guard.
const SUBSCRIBER_TIMEOUT: Duration = Duration::from_secs(90);
const PUBLISH_TIMEOUT: Duration = Duration::from_secs(60);
/// Sustained A/V arrival skew bound. On a 2 % loss / 200 ms RTT path about a
/// twentieth of the pairs include a retransmission, so the tail can approach
/// one recovery interval; 500 ms holds that tail without hiding a track that
/// has actually stalled, which the frame counts also catch.
const MAX_P95_SKEW_US: u64 = 500_000;
/// Connect, subscribe and the first GOP under 200 ms delay, with headroom.
const MAX_STARTUP_MS: f64 = 10_000.0;

fn epoch_millis(now: SystemTime) -> u64 {
    now.duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis().min(u128::from(u64::MAX)) as u64)
        .unwrap_or(0)
}

/// The subscriber binary sits next to this executable in both
/// `target/debug` and `target/bench`.
fn subscriber_bin() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("MOQ_IMPAIRMENT_SUBSCRIBER_BIN") {
        return Ok(PathBuf::from(path));
    }
    let exe = std::env::current_exe().map_err(|error| error.to_string())?;
    let path = exe
        .parent()
        .ok_or_else(|| format!("{} has no parent directory", exe.display()))?
        .join("moq_subscriber");
    if !path.is_file() {
        return Err(format!(
            "moq_subscriber not found at {}; run `cargo xtask build-bench` (or `cargo build --bin moq_subscriber`)",
            path.display()
        ));
    }
    Ok(path)
}

fn tc(args: &[&str]) -> Result<String, String> {
    let output = std::process::Command::new("tc")
        .args(args)
        .output()
        .map_err(|error| format!("run tc ({}): {error}", args.join(" ")))?;
    if !output.status.success() {
        return Err(format!(
            "tc {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

/// A loopback `netem` qdisc with filters for one UDP port, removed on drop.
/// Applied only after publishing is up, so the impairment window starts with
/// the subscriber rather than making the publisher's startup part of the
/// measurement.
struct NetemGuard {
    commands: Vec<Value>,
    show: String,
    active: bool,
}

/// The tc recipe for one UDP port: a `prio` root, a netem band and a filter
/// per direction. Pure so its shape is unit-tested without CAP_NET_ADMIN.
fn netem_steps(udp_port: u16) -> Vec<Vec<String>> {
    let loss = format!("{LOSS_PERCENT}%");
    let delay = format!("{DELAY_MS}ms");
    let port = udp_port.to_string();
    vec![
        [
            "qdisc", "add", "dev", "lo", "root", "handle", "1:", "prio", "bands", "3",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect(),
        [
            "qdisc",
            "add",
            "dev",
            "lo",
            "parent",
            "1:3",
            "handle",
            "30:",
            "netem",
            "loss",
            loss.as_str(),
            "delay",
            delay.as_str(),
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect(),
        [
            "filter",
            "add",
            "dev",
            "lo",
            "parent",
            "1:",
            "protocol",
            "ip",
            "prio",
            "10",
            "u32",
            "match",
            "ip",
            "protocol",
            "17",
            "0xff",
            "match",
            "ip",
            "dport",
            port.as_str(),
            "0xffff",
            "flowid",
            "1:3",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect(),
        [
            "filter",
            "add",
            "dev",
            "lo",
            "parent",
            "1:",
            "protocol",
            "ip",
            "prio",
            "11",
            "u32",
            "match",
            "ip",
            "protocol",
            "17",
            "0xff",
            "match",
            "ip",
            "sport",
            port.as_str(),
            "0xffff",
            "flowid",
            "1:3",
        ]
        .iter()
        .map(|arg| arg.to_string())
        .collect(),
    ]
}

impl NetemGuard {
    fn apply(udp_port: u16) -> Result<Self, String> {
        // Start from a clean root qdisc whatever an earlier failed run left.
        let _ = tc(&["qdisc", "del", "dev", "lo", "root"]);
        let mut commands = Vec::new();
        for step in netem_steps(udp_port) {
            let borrowed: Vec<&str> = step.iter().map(String::as_str).collect();
            match tc(&borrowed) {
                Ok(stdout) => commands.push(json!({"argv": step, "stdout": stdout})),
                // The guard removes whatever the failed recipe left behind.
                Err(error) => {
                    let guard = Self {
                        commands,
                        show: String::new(),
                        active: true,
                    };
                    drop(guard);
                    return Err(error);
                }
            }
        }
        let show = tc(&["-s", "qdisc", "show", "dev", "lo"])?;
        Ok(Self {
            commands,
            show,
            active: true,
        })
    }
}

impl Drop for NetemGuard {
    fn drop(&mut self) {
        if self.active {
            let _ = tc(&["qdisc", "del", "dev", "lo", "root"]);
            self.active = false;
        }
    }
}

struct Session {
    work_dir: PathBuf,
    ffmpeg: PathBuf,
    fixture: PathBuf,
    tls: InteropTls,
    ports: TestPorts,
    dev_ports: DevPorts,
    relay: Option<DevRelay>,
    restream: Option<Child>,
    publisher: Option<Child>,
    subscriber: Option<Child>,
    netem: Option<NetemGuard>,
    checks: Vec<Value>,
    failures: Vec<String>,
}

impl Session {
    fn record(&mut self, id: &str, ok: bool, detail: Value) {
        if !ok {
            self.failures.push(id.to_string());
        }
        self.checks
            .push(json!({"id": id, "ok": ok, "detail": detail}));
    }

    fn spawn_publisher(&mut self, url: &str) -> Result<(), String> {
        let log = std::fs::File::create(self.work_dir.join("publisher.log"))
            .map_err(|error| error.to_string())?;
        let stderr = log.try_clone().map_err(|error| error.to_string())?;
        let child = command_with_optional_cgroup(&self.ffmpeg, "moq-impairment-publisher")
            .args(["-nostdin", "-hide_banner", "-loglevel", "error"])
            .args(crate::hls_push_duty::run::rtmp_publisher_args(
                &self.fixture,
                url,
            ))
            .stdout(Stdio::from(log))
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", self.ffmpeg.display()))?;
        self.publisher = Some(child);
        Ok(())
    }

    async fn execute(&mut self) -> Result<(), String> {
        let relay_bin = pinned_bin(&MOQ_RELAY).await?;
        let mut relay = DevRelay::new(
            relay_bin,
            self.dev_ports.relay,
            &self.tls,
            self.work_dir.join("moq-relay.log"),
        );
        relay.start().await?;
        self.relay = Some(relay);

        let env_overrides = [(
            "RESTREAM_MOQ_CA_DIR",
            self.tls
                .ca_pem
                .parent()
                .ok_or("CA has no parent directory")?
                .display()
                .to_string(),
        )];
        let restream = start_restream_child_with_env(
            &default_restream_bin(),
            &self.ports,
            &default_work_db_path(&self.work_dir, "moq-impairment.sqlite"),
            &self.work_dir.join("restream.log"),
            &env_overrides,
        )
        .await?;
        self.restream = Some(restream);
        let api = login_api(&self.ports).await?;

        let pipeline = create_resource_pipeline(&api, "moq-impairment", STREAM_KEY).await?;
        let moq_url = format!(
            "moqt://127.0.0.1:{}/?broadcast={BROADCAST}&version={LITE_VERSION}&ca=ca.pem",
            self.dev_ports.relay
        );
        let output = create_output(&api, &pipeline, "moq-relay", &moq_url, "source").await?;
        start_output(&api, &pipeline, &output).await?;

        let publish_url = format!("rtmp://127.0.0.1:{}/live/{STREAM_KEY}", self.ports.rtmp);
        self.spawn_publisher(&publish_url)?;
        wait_for_api_input_live(&api, &pipeline, PUBLISH_TIMEOUT).await?;
        wait_for_api_input_media_ready(&api, &pipeline, PUBLISH_TIMEOUT).await?;
        let relay = self.relay.as_ref().ok_or("relay was not started")?;
        let catalog = relay
            .wait_broadcast(BROADCAST, PUBLISH_TIMEOUT, false)
            .await?;
        let publish_ready_unix_ms = epoch_millis(SystemTime::now());
        self.record(
            "egressRelayCatalog",
            true,
            json!({"broadcast": BROADCAST, "catalog": catalog, "publishReadyUnixMs": publish_ready_unix_ms}),
        );

        let netem = NetemGuard::apply(self.dev_ports.relay)?;
        let rules = json!({
            "device": "lo",
            "lossPercent": LOSS_PERCENT,
            "delayMs": DELAY_MS,
            "udpPort": self.dev_ports.relay,
            "commands": netem.commands,
            "qdiscShow": netem.show,
        });
        println!("[{MODE}] {}", json!({"phase": "netem", "rules": rules}));
        self.netem = Some(netem);

        let bin = subscriber_bin()?;
        let stderr = std::fs::File::create(self.work_dir.join("subscriber.log"))
            .map_err(|error| error.to_string())?;
        let child = Command::new(&bin)
            .arg("--url")
            .arg(format!("https://127.0.0.1:{}/", self.dev_ports.relay))
            .arg("--ca")
            .arg(&self.tls.ca_pem)
            .arg("--broadcast")
            .arg(BROADCAST)
            .arg("--alpn")
            .arg(LITE_VERSION)
            .arg("--watch-secs")
            .arg(WATCH_SECS.to_string())
            .arg("--publish-ready-unix-ms")
            .arg(publish_ready_unix_ms.to_string())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(stderr))
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| format!("spawn {}: {error}", bin.display()))?;
        self.subscriber = Some(child);
        let child = self.subscriber.take().ok_or("subscriber was not spawned")?;
        let captured = tokio::time::timeout(SUBSCRIBER_TIMEOUT, child.wait_with_output()).await;
        let output = match captured {
            Ok(output) => output.map_err(|error| format!("wait for subscriber: {error}"))?,
            Err(_) => {
                return Err(format!(
                    "subscriber exceeded {} s",
                    SUBSCRIBER_TIMEOUT.as_secs()
                ));
            }
        };
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        let report = stdout
            .lines()
            .rev()
            .find(|line| line.trim_start().starts_with('{'))
            .and_then(|line| serde_json::from_str::<Value>(line).ok());
        let exit_ok = output.status.success();
        let Some(report) = report else {
            self.record(
                "subscriberReport",
                false,
                json!({"exit": output.status.code(), "stdout": stdout, "stderr": stderr}),
            );
            return Err(format!(
                "{MODE}: subscriber wrote no JSON report (exit {:?}); stderr={}",
                output.status.code(),
                stderr.trim()
            ));
        };
        self.record(
            "subscriberExit",
            exit_ok,
            json!({"code": output.status.code(), "stderr": stderr, "report": report}),
        );
        let error = report["error"].as_str();
        self.record(
            "subscriberNoProtocolError",
            error.is_none(),
            json!({"error": error}),
        );
        let video_frames = report["videoFrames"].as_u64().unwrap_or(0);
        let keyframes = report["keyframes"].as_u64().unwrap_or(0);
        let audio_frames = report["audioFrames"].as_u64().unwrap_or(0);
        let audio_bytes = report["audioBytes"].as_u64().unwrap_or(0);
        let skew_samples = report["skewSamples"].as_u64().unwrap_or(0);
        let max_abs_skew_us = report["maxAbsSkewUs"].as_u64().unwrap_or(u64::MAX);
        let p95_abs_skew_us = report["p95AbsSkewUs"].as_u64().unwrap_or(u64::MAX);
        let startup_delay_ms = report["startupDelayMs"].as_f64();
        self.record(
            "subscriberVideo",
            video_frames > 0,
            json!({"videoFrames": video_frames}),
        );
        self.record(
            "subscriberKeyframes",
            keyframes > 0,
            json!({"keyframes": keyframes}),
        );
        self.record(
            "subscriberAudio",
            audio_frames > 0 && audio_bytes > 0,
            json!({"audioFrames": audio_frames, "audioBytes": audio_bytes}),
        );
        self.record(
            "subscriberAvSkew",
            skew_samples > 0 && p95_abs_skew_us <= MAX_P95_SKEW_US,
            json!({
                "kind": "arrival",
                "samples": skew_samples,
                "p95AbsSkewUs": p95_abs_skew_us,
                "maxAbsSkewUs": max_abs_skew_us,
                "boundUs": MAX_P95_SKEW_US,
                "matchToleranceUs": report["skewMatchToleranceUs"],
            }),
        );
        self.record(
            "subscriberStartup",
            startup_delay_ms.is_some_and(|delay| delay <= MAX_STARTUP_MS),
            json!({"startupDelayMs": startup_delay_ms, "boundMs": MAX_STARTUP_MS}),
        );
        self.record(
            "subscriberAlpn",
            report["alpn"].as_str() == Some(LITE_VERSION),
            json!({"alpn": report["alpn"], "expected": LITE_VERSION}),
        );
        Ok(())
    }

    async fn shutdown(&mut self) {
        if let Some(mut child) = self.subscriber.take() {
            stop_child(&mut child).await;
        }
        self.netem.take();
        if let Some(mut child) = self.publisher.take() {
            stop_child(&mut child).await;
        }
        if let Some(mut restream) = self.restream.take() {
            stop_child(&mut restream).await;
        }
        if let Some(mut relay) = self.relay.take() {
            relay.kill().await;
        }
    }
}

pub(crate) async fn moq_impairment(args: &[String]) -> Result<Value, String> {
    let args = strip_netns_opt(args);
    if let Some(unknown) = args.first() {
        return Err(format!(
            "moq-impairment takes no options (got {unknown:?}); usage: moq-impairment [--no-netns]"
        ));
    }
    if std::env::var_os("RESTREAM_HARNESS_IN_NETNS").is_none() {
        return Err(
            "moq-impairment needs the default private network namespace with CAP_NET_ADMIN for tc netem; rerun without --no-netns"
                .to_string(),
        );
    }
    let started = Instant::now();
    let work_dir = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(".local/artifacts/moq-impairment"));
    std::fs::create_dir_all(&work_dir).map_err(|error| error.to_string())?;
    let work_dir = absolute_path(&work_dir)?;
    let report_path = work_dir.join("report.json");

    let fixture = restream::test_fixtures::checked_in_fixture(FIXTURE)?;
    let ffmpeg = restream::ffmpeg_binary::ffmpeg_bin_path().to_path_buf();
    let tls = InteropTls::generate(&work_dir.join("tls")).await?;
    let ports = TestPorts::from_env();
    let (http_port, rtmp_port) = (ports.http, ports.rtmp);
    let dev_ports = DevPorts::allocate();
    println!(
        "[{MODE}] {}",
        json!({
            "phase": "config",
            "fixture": fixture.display().to_string(),
            "ports": {"http": http_port, "rtmp": rtmp_port},
            "devPorts": dev_ports.to_json(),
            "lossPercent": LOSS_PERCENT,
            "delayMs": DELAY_MS,
            "watchSecs": WATCH_SECS,
        })
    );

    let mut session = Session {
        work_dir: work_dir.clone(),
        ffmpeg,
        fixture,
        tls,
        ports,
        dev_ports,
        relay: None,
        restream: None,
        publisher: None,
        subscriber: None,
        netem: None,
        checks: Vec::new(),
        failures: Vec::new(),
    };
    let outcome = session.execute().await;
    session.shutdown().await;

    let passed = outcome.is_ok() && session.failures.is_empty();
    let mut failures = session.failures.clone();
    if let Err(error) = &outcome {
        failures.push(format!("aborted: {error}"));
    }
    let report = json!({
        "passed": passed,
        "mode": MODE,
        "path": "relay",
        "broadcast": BROADCAST,
        "environment": {
            "moqRelay": MOQ_RELAY.path,
            "moqRelayVersion": MOQ_RELAY.version,
            "restreamBin": default_restream_bin().display().to_string(),
            "fixture": session.fixture.display().to_string(),
            "ports": {"http": http_port, "rtmp": rtmp_port},
            "devPorts": dev_ports.to_json(),
            "watchSecs": WATCH_SECS,
            "maxP95SkewUs": MAX_P95_SKEW_US,
            "maxStartupMs": MAX_STARTUP_MS,
        },
        "checks": session.checks,
        "failedChecks": failures,
        "wallClockSecs": round3(started.elapsed().as_secs_f64()),
    });
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

    #[test]
    fn mode_is_a_namespace_catalog_command_off_the_default_suite() {
        let spec = mode_spec("moq-impairment").expect("moq-impairment must be in modes.json");
        assert!(
            spec.requires_port_namespace,
            "netem needs the private namespace and CAP_NET_ADMIN"
        );
        assert!(!spec.requires_bench_profile);
        assert!(!spec.suite_default);
        assert!(
            supported_mode_names()
                .iter()
                .any(|name| name == "moq-impairment")
        );
    }

    #[test]
    fn netem_recipe_impairs_the_relay_udp_port_in_both_directions() {
        let steps = netem_steps(4443);
        assert_eq!(steps.len(), 4);
        let joined = steps
            .iter()
            .map(|step| step.join(" "))
            .collect::<Vec<_>>()
            .join("\n");
        assert!(joined.contains("netem loss 2% delay 200ms"), "{joined}");
        assert_eq!(
            steps
                .iter()
                .filter(|step| step.iter().any(|arg| arg == "match"))
                .count(),
            2,
            "{joined}"
        );
        assert!(joined.contains("match ip protocol 17 0xff match ip dport 4443 0xffff"));
        assert!(joined.contains("match ip protocol 17 0xff match ip sport 4443 0xffff"));
        assert!(
            steps.iter().all(|step| step.iter().any(|arg| arg == "lo")),
            "every step targets loopback: {joined}"
        );
    }
}
