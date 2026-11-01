//! The `fault.srt-output-stall` mode: two live proofs of stalled-destination
//! recovery and bounded RSS for a destination that stops taking data.
//!
//! **Two different faults, because `SIGSTOP` is not backpressure.** A single
//! `SIGSTOP` test cannot cover the *backpressured-but-connected* path
//! (`classify_stall`/`observe_stall` in
//! `crates/restream-engine/src/egress/policy.rs` and `crates/restream-engine/src/egress/backends/srt.rs`): a
//! destination that keeps the SRT connection alive but never drains
//! application data, so the leaf sits at `LeafStallClass::Backpressured`
//! and is closed only once it reaches `Stalled`. `SIGSTOP` on a real
//! MediaMTX receiver looks like it would produce exactly that by freezing its
//! `recv()` loop. It does not: `SIGSTOP` freezes *every* thread in the
//! receiver process, including its SRT protocol/ACK workers,
//! so the connection is detected as fully broken within seconds (observed:
//! `srt_send failed ... Connection was broken`), not backpressured — SRT
//! has no way to distinguish "receiver alive but not reading" from
//! "receiver process frozen" once the underlying process stops generating
//! ACKs. So the two conditions are tested separately:
//!
//! - `srt_egress_bounded_rss_under_frozen_destination` keeps the `SIGSTOP`
//!   receiver and proves what it actually produces: a real SRT egress
//!   connection that breaks and re-attempts against an unreachable
//!   destination, held for several minutes, does not grow the process's RSS
//!   unboundedly — the retry/backoff/cleanup cycle is not leaking sockets,
//!   buffers, or retained feed state. A 1 s sampler records RSS, PSS,
//!   threads, fds, SRT shard rows, output health and retained-ring
//!   accounting from live progress to capture completion in `samples.jsonl`;
//!   a sample error, a gap over 2 s, or samples that miss the fault or the
//!   retry window fail the test. The RSS gate is the peak sampled RSS in the
//!   retry window over the first sample at retry observation, so ring
//!   warm-up before the first retry is not counted.
//! - `srt_egress_srt_rs_receiver_delivery` uses the purpose-built Tokio/
//!   srt-rs listener in `srt_raw_sink.rs` to prove a live handshake and
//!   delivered media events without making MediaMTX part of the receiver
//!   path. srt-rs bounds unread delivery and charges it to the advertised
//!   receive window, so the sink can now exercise genuine receiver-side
//!   backpressure without a native-library-specific test double.
//!
//! **Scheduling-contention phase** (`FAULT_CONTENTION_BURNER_THREADS=N`,
//! default 0): with the raw sink in place and the watch window running, N
//! pure-spin threads deliberately starve restream's egress shard threads
//! the way a loaded MSR box does (the scale investigation measured
//! 131-262ms run-queue waits under sweep load). The artifact records the
//! load average before/during/after plus the output's SRT drop counter
//! (`packetsSentDrop`) across the watch. The contention shape and its drop
//! signature remain evidence, rather than an additional pass gate.

use super::super::*;
use super::egress::wait_for_outputs_live_and_progressing;
use super::resilience::{create_pipeline, delete_pipeline_v1};

#[path = "srt_stall_sampler.rs"]
mod sampler;
use sampler::{CaptureMarkers, RETRY_WINDOW, Sampler};

/// Environmental scheduling contention for `fault.srt-output-stall`:
/// `FAULT_CONTENTION_BURNER_THREADS=N` spawns N pure-spin threads for the
/// watch window of `srt_egress_srt_rs_receiver_delivery`, deliberately
/// starving restream's egress shard threads the way a loaded MSR box does
/// (the ad-hoc `runqlat` evidence in the scale investigation showed
/// 131-262ms scheduling waits on the 6-core box during sweeps). Off by
/// default so the deterministic receiver proof also doubles as the CI gate;
/// the scale investigation runs it with N=8 to reproduce the contention
/// shape live (artifact records load average + sampled drop counters either
/// way).
const CONTENTION_BURNER_ENV: &str = "FAULT_CONTENTION_BURNER_THREADS";

/// `N` pure-spin OS threads, alive for the enclosing scope. Each burner
/// competes for a CFS time slice without yielding, so every other thread
/// on the box — restream's shard threads among them — sees the scheduling
/// starvation of a heavily loaded host. Pure spin loops allocate nothing,
/// so a 6-core box runs a dozen of them comfortably; only the CPU is
/// consumed, which is the point.
struct SpinBurners {
    stop: Arc<AtomicBool>,
    handles: Vec<JoinHandle<()>>,
}

impl SpinBurners {
    fn spawn(count: usize) -> Result<Self, String> {
        let stop = Arc::new(AtomicBool::new(false));
        let mut handles = Vec::with_capacity(count);
        for index in 0..count {
            let stop = Arc::clone(&stop);
            let handle = thread::Builder::new()
                .name(format!("fault-sched-burner-{index:02}"))
                .spawn(move || {
                    while !stop.load(Ordering::Relaxed) {
                        std::hint::spin_loop();
                    }
                })
                .map_err(|e| format!("spawn CPU contention burner {index}: {e}"))?;
            handles.push(handle);
        }
        Ok(Self { stop, handles })
    }
}

impl Drop for SpinBurners {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for handle in self.handles.drain(..) {
            let _ = handle.join();
        }
    }
}

/// One-minute load average from `/proc/loadavg` (first field), for the
/// contention evidence trail.
fn loadavg() -> Option<f64> {
    let contents = std::fs::read_to_string("/proc/loadavg").ok()?;
    contents.split_whitespace().next()?.parse::<f64>().ok()
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};

async fn spawn_bare_mediamtx_srt(
    work_dir: &std::path::Path,
    srt_port: u16,
    api_port: u16,
) -> Result<Child, String> {
    std::fs::create_dir_all(work_dir).map_err(|e| e.to_string())?;
    let config_path = work_dir.join("mediamtx-srt-stall.yml");
    let log_path = work_dir.join("mediamtx-srt-stall.log");
    std::fs::write(
        &config_path,
        format!(
            "logLevel: warn\nreadTimeout: 60s\nwriteTimeout: 60s\n{udp}rtmp: no\nrtsp: no\nsrt: yes\nsrtAddress: :{srt_port}\nhls: no\nwebrtc: no\nmoq: no\napi: yes\napiAddress: :{api_port}\nmetrics: no\npaths:\n  all:\n",
            udp = mediamtx_udp_read_buffer_line(),
        ),
    )
    .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new(pinned_mediamtx()?);
    let mediamtx = remove_mediamtx_config_env(&mut command)
        .arg(&config_path)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(log_err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("spawn mediamtx: {e}"))?;

    wait_for_http_ok(
        &format!("http://127.0.0.1:{api_port}/v3/paths/list"),
        Duration::from_secs(60),
    )
    .await
    .map_err(|error| format!("mediamtx did not become ready: {error}"))?;

    Ok(mediamtx)
}

/// Suspend a process without reaping it — `SIGSTOP` freezes every thread;
/// the process resumes exactly where it left off on `SIGCONT`. Used here to
/// simulate a destination that stops reading without disconnecting.
fn signal_process(pid: u32, signal: libc::c_int) -> Result<(), String> {
    // SAFETY: Category 8 - FFI boundary. `pid` is a real child PID this
    // harness owns for the duration of the call (mediamtx, spawned above
    // and not yet reaped), and `signal` is a valid POSIX signal number.
    let result = unsafe { libc::kill(pid as libc::pid_t, signal) };
    if result == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error().to_string())
    }
}

/// Mode entry: run both destination-stall proofs against their own restream
/// instances and fold them into one artifact.
pub(crate) async fn fault_srt_output_stall() -> Result<Value, String> {
    let work_dir = artifact_path("fault.srt-output-stall");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let tests = vec![
        srt_egress_bounded_rss_under_frozen_destination().await?,
        srt_egress_srt_rs_receiver_delivery().await?,
    ];
    let passed = tests
        .iter()
        .all(|test| test["passed"].as_bool().unwrap_or(false));

    let payload = json!({
        "mode": "fault.srt-output-stall",
        "passed": passed,
        "tests": tests,
    });
    super::finish_fault_mode(&work_dir, "fault.srt-output-stall", passed, payload)
}

async fn srt_egress_bounded_rss_under_frozen_destination() -> Result<Value, String> {
    let work_dir = artifact_path("fault.srt-output-stall");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("frozen-destination.sqlite");
    let log_path = work_dir.join("restream-frozen-destination.log");
    let ports = TestPorts::from_env();
    let mtx_defaults = harness_port_defaults();
    let timeout = Duration::from_secs(15);

    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let mut mediamtx =
        spawn_bare_mediamtx_srt(&work_dir, mtx_defaults.mtx_srt, mtx_defaults.mtx_api).await?;
    let mediamtx_pid = mediamtx.id();

    let fixture_h264 = checked_h264_fixture()?;
    let pid = create_pipeline(&api, "fault-egress-srt-stall").await?;
    let oid = create_output(
        &api,
        &pid,
        "srt-stall-sink",
        &harness_srt_output_url(
            mtx_defaults.mtx_srt,
            "fault-egress-srt-stall-sink",
            HarnessSrtMode::Publish,
        ),
        "source",
    )
    .await?;

    let mut pub_child = spawn_publisher(
        &fixture_h264,
        &harness_srt_ffmpeg_url(
            ports.srt,
            "fault-egress-srt-stall",
            HarnessSrtMode::Publish,
            None,
        ),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &pid, timeout).await?;

    start_output(&api, &pid, &oid).await?;

    // Confirm the output is genuinely progressing through a real SRT
    // handshake and real MediaMTX ingest before freezing the receiver —
    // otherwise a "stalled" observation afterward would be indistinguishable
    // from "never actually connected."
    let live_deadline = std::time::Instant::now() + Duration::from_secs(15);
    let mut reached_progress = false;
    let mut bytes_at_stall_start = 0u64;
    while std::time::Instant::now() < live_deadline {
        if let Ok((status, _)) = api.get_output_status(&pid, &oid).await
            && status.status == "running"
            && status.bytes_out > 0
        {
            reached_progress = true;
            bytes_at_stall_start = status.bytes_out;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let capture_origin = std::time::Instant::now();
    let since_capture_origin = || capture_origin.elapsed().as_secs_f64();
    let samples_path = work_dir.join("samples.jsonl");
    let started = if reached_progress {
        child
            .id()
            .ok_or_else(|| "restream child PID unavailable".to_string())
            .and_then(|restream_pid| {
                Sampler::spawn(&api, restream_pid, capture_origin, samples_path.clone())
            })
    } else {
        Err("output never reached real progress before the stall".to_string())
    };
    let sampler = match started {
        Ok(started) => started,
        Err(error) => {
            stop_child(&mut pub_child).await;
            stop_child(&mut mediamtx).await;
            stop_child(&mut child).await;
            return Err(format!("srt-egress-stalled-destination: {error}"));
        }
    };
    let mut markers = CaptureMarkers::default();
    let receiver_progress = crate::mediamtx_probe::verify_mediamtx_path_health(
        mtx_defaults.mtx_api,
        &["fault-egress-srt-stall-sink".to_string()],
        5,
        timeout,
    )
    .await;

    let stall_started = std::time::Instant::now();
    let mut sigstop_ok = false;
    if let Some(pid) = mediamtx_pid {
        sigstop_ok = signal_process(pid, libc::SIGSTOP).is_ok();
    }
    markers.sigstop = since_capture_origin();

    // Confirm the output actually entered the retry/failure cycle (not just
    // silently frozen) before measuring RSS across an extended window of
    // repeated connect-failure retries against the still-suspended
    // receiver.
    let mut entered_retry_cycle = false;
    let retry_deadline = std::time::Instant::now() + Duration::from_secs(30);
    while std::time::Instant::now() < retry_deadline {
        if let Ok((status, _)) = api.get_output_status(&pid, &oid).await
            && (status.status == "retrying" || status.status == "failed")
        {
            entered_retry_cycle = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    markers.retry_observed = since_capture_origin();

    // The runtime state names the frozen destination as the cause, with the
    // ingest and the feed (and the shard, while an attempt runs on it)
    // ruled out: the shard and the publisher are not blamed.
    let mut diagnosis = Value::Null;
    let diagnosis_deadline = std::time::Instant::now() + Duration::from_secs(15);
    while entered_retry_cycle && std::time::Instant::now() < diagnosis_deadline {
        if let Ok(state) = api.get_json("/api/v1/state").await
            && let Some(found) = state["outputs"]
                .as_array()
                .into_iter()
                .flatten()
                .find(|output| output["id"] == oid.as_str())
            && found["diagnosis"]["layer"] == "destination"
        {
            diagnosis = found["diagnosis"].clone();
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
    let ruled_out = diagnosis["ruledOut"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let diagnosed_destination = ["ingest", "feed"]
        .iter()
        .all(|layer| ruled_out.iter().any(|value| value == layer));

    // Bounded RSS during indefinite stalls: sample
    // restream's RSS across an extended window of continuous
    // connect-failure retries. A bounded sample window standing in for
    // "indefinite" is the same practical compromise every other live proof
    // in this harness makes for wall-clock reasons — the property under
    // test (no unbounded growth while an output keeps failing to connect)
    // is what a longer window would also show, just slower.
    tokio::time::sleep(RETRY_WINDOW).await;

    let final_output = observe_final_output(&api, &pid, &oid).await;
    let (status_snapshot, health_snapshot) = (
        final_output.status.clone().unwrap_or(Value::Null),
        final_output.health.clone(),
    );

    // Every exit from here runs the same cleanup, and the sampler is always
    // finished before the engine stops, so no sample observes the shutdown.
    let teardown = async {
        api.post_null(&format!("/api/v1/pipelines/{pid}/outputs/{oid}/stop"))
            .await?;
        wait_for_outputs_stopped(&api, &pid, std::slice::from_ref(&oid), timeout).await?;
        markers.output_stopped = since_capture_origin();
        tokio::time::sleep(Duration::from_secs(3)).await;
        stop_child(&mut pub_child).await;
        delete_pipeline_v1(&api, &pid).await?;
        markers.pipeline_deleted = since_capture_origin();
        tokio::time::sleep(Duration::from_secs(3)).await;
        Ok::<(), String>(())
    }
    .await;
    stop_child(&mut pub_child).await;
    if let Some(pid) = mediamtx_pid {
        let _ = signal_process(pid, libc::SIGCONT);
    }
    stop_child(&mut mediamtx).await;
    markers.capture_complete = since_capture_origin();
    let capture = sampler.finish().await;
    stop_child(&mut child).await;
    teardown?;

    let (capture_errors, sampled, rss) = match &capture {
        Ok(outcome) => (
            sampler::capture_errors(outcome, &markers),
            sampler::sampled_summary(&outcome.samples, markers.sigstop),
            sampler::retry_window_rss(&outcome.samples, &markers),
        ),
        Err(error) => (
            vec![error.clone()],
            Value::Null,
            sampler::retry_window_rss(&[], &markers),
        ),
    };
    let rss_bounded = rss["bounded"].as_bool().unwrap_or(false);
    let passed = sigstop_ok
        && entered_retry_cycle
        && rss_bounded
        && diagnosed_destination
        && receiver_progress.is_ok()
        && capture_errors.is_empty();

    println!(
        "[fault] SRT egress bounded RSS under frozen destination: {} (sigstopOk={} enteredRetryCycle={} diagnosedDestination={} bytesAtStallStart={} rss={} sampled={} captureErrors={:?} elapsed={:.1}s)",
        if passed { "PASS" } else { "FAIL" },
        sigstop_ok,
        entered_retry_cycle,
        diagnosed_destination,
        bytes_at_stall_start,
        rss,
        sampled,
        capture_errors,
        stall_started.elapsed().as_secs_f64(),
    );

    Ok(json!({
        "test": "srt-egress-bounded-rss-under-frozen-destination",
        "passed": passed,
        "sigstopOk": sigstop_ok,
        "enteredRetryCycle": entered_retry_cycle,
        "diagnosedDestination": diagnosed_destination,
        "diagnosis": diagnosis,
        "bytesAtStallStart": bytes_at_stall_start,
        "receiverProgress": receiver_progress.as_ref().ok().map(|health|
            crate::mediamtx_probe::mediamtx_path_health_json("frozen", "before-sigstop", health)),
        "receiverProgressError": receiver_progress.err(),
        "rssRetryWindow": rss,
        "sampledRss": sampled,
        "captureErrors": capture_errors,
        "phases": markers,
        "samplesPath": samples_path,
        "status": status_snapshot,
        "healthOutput": health_snapshot,
    }))
}

/// Healthy SRT siblings kept next to the direct srt-rs receiver, so the run
/// also shows that the custom sink does not hold up the rest of the shard's
/// leaves.
const SRT_RS_RECEIVER_SIBLING_OUTPUTS: usize = 2;

/// How long to observe the direct srt-rs receiver and its siblings.
const SRT_RS_RECEIVER_OBSERVATION_WINDOW: Duration = Duration::from_secs(10);

/// What the run observed about the direct srt-rs receiver, tracked as a unit
/// so the pass decision and the artifact agree by construction.
#[derive(Default)]
struct SrtRsReceiverObservation {
    reasons_seen: Vec<String>,
    saw_backpressured: bool,
    connected_while_backpressured: bool,
    saw_stalled: bool,
    entered_retry_cycle: bool,
    bytes_at_backpressure: u64,
    bytes_at_close: u64,
    drops_first: Option<u64>,
    drops_last: Option<u64>,
}

impl SrtRsReceiverObservation {
    fn record(&mut self, reason: &str, bytes_out: u64, sink_connected: u64) {
        if !reason.is_empty() && !self.reasons_seen.iter().any(|seen| seen == reason) {
            self.reasons_seen.push(reason.to_string());
        }
        match reason {
            "backpressured" => {
                if !self.saw_backpressured {
                    self.bytes_at_backpressure = bytes_out;
                }
                self.saw_backpressured = true;
                // Keep this as diagnostic evidence if the sender reports a
                // transient backpressure reason; it is not a pass gate for
                // the srt-rs receiver proof.
                self.connected_while_backpressured |= sink_connected >= 1;
            }
            "stalled" => {
                if !self.saw_stalled {
                    self.bytes_at_close = bytes_out;
                }
                self.saw_stalled = true;
            }
            _ => {}
        }
    }

    /// Sample the output's health drop counter (`pktSndDropTotal`,
    /// exposed as `quality.packetsSentDrop`) for the artifact's evidence
    /// trail. `None` means the health snapshot had no usable counter (API
    /// hiccup or the output was recreated); never a pass gate.
    fn record_drop_sample(&mut self, sample: Option<u64>) {
        if let Some(sample) = sample {
            if self.drops_first.is_none() {
                self.drops_first = Some(sample);
            }
            self.drops_last = Some(sample);
        }
    }
}

/// Prove the direct srt-rs receiver path live: a receiver that completes the
/// handshake and receives application data without a MediaMTX bottleneck,
/// while healthy sibling outputs continue to make progress.
async fn srt_egress_srt_rs_receiver_delivery() -> Result<Value, String> {
    let work_dir = artifact_path("fault.srt-output-stall");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("srt-rs-receiver.sqlite");
    let log_path = work_dir.join("restream-srt-rs-receiver.log");
    let ports = TestPorts::from_env();
    let timeout = Duration::from_secs(15);
    let sink_port = harness_port_defaults()
        .ffmpeg_srt_sink_base
        .checked_add(1000)
        .ok_or("raw srt-rs receiver port range overflowed")?;

    // Bind the direct srt-rs destination before restream starts, so the
    // output's very first connect attempt lands on it.
    let sink = RawSrtSink::start(sink_port)?;

    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture_h264 = checked_h264_fixture()?;
    let pid = create_pipeline(&api, "fault-egress-srt-rs-receiver").await?;

    let bad_oid = create_output(
        &api,
        &pid,
        "srt-rs-receiver",
        &harness_srt_output_url(sink.port(), "srt-rs-receiver-sink", HarnessSrtMode::Publish),
        "source",
    )
    .await?;

    // Independent receivers provide the sibling oracle before and after the
    // target's logical retirement; sender counters alone cannot prove delivery.
    let mut sibling_output_ids = Vec::with_capacity(SRT_RS_RECEIVER_SIBLING_OUTPUTS);
    let mut sibling_sinks = Vec::with_capacity(SRT_RS_RECEIVER_SIBLING_OUTPUTS);
    for index in 0..SRT_RS_RECEIVER_SIBLING_OUTPUTS {
        let sink_name = format!("srt-rs-receiver-healthy-sink-{index:02}");
        let sibling_port = sink_port
            .checked_add(index as u16 + 1)
            .ok_or("sibling srt-rs receiver port range overflowed")?;
        let sibling_sink = RawSrtSink::start(sibling_port)?;
        let oid = create_output(
            &api,
            &pid,
            &format!("srt-rs-receiver-healthy-{index:02}"),
            &harness_srt_output_url(sibling_sink.port(), &sink_name, HarnessSrtMode::Publish),
            "source",
        )
        .await?;
        sibling_output_ids.push(oid);
        sibling_sinks.push(sibling_sink);
    }

    let mut pub_child = spawn_publisher(
        &fixture_h264,
        &harness_srt_ffmpeg_url(
            ports.srt,
            "fault-egress-srt-rs-receiver",
            HarnessSrtMode::Publish,
            None,
        ),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &pid, timeout).await?;

    start_output(&api, &pid, &bad_oid).await?;
    for output_id in &sibling_output_ids {
        start_output(&api, &pid, output_id).await?;
    }

    // Require real delivery into the raw sink first: without it, a passing
    // sibling check could not distinguish a working receiver from an output
    // that never connected.
    let progress_deadline = Instant::now() + Duration::from_secs(20);
    let mut bytes_before_receiver_delivery = 0u64;
    while Instant::now() < progress_deadline {
        if let Ok((status, _)) = api.get_output_status(&pid, &bad_oid).await
            && status.bytes_out > 0
            && sink.observe().accepted >= 1
        {
            bytes_before_receiver_delivery = status.bytes_out;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    // Optional scheduling-contention phase: burners starve restream's
    // shard threads during the watch (off by default; the scale
    // investigation runs this mode with `FAULT_CONTENTION_BURNER_THREADS=8`).
    let contention_threads = std::env::var(CONTENTION_BURNER_ENV)
        .ok()
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0);
    let contention = SpinBurners::spawn(contention_threads)?;
    let loadavg_before = loadavg();
    let mut loadavg_during = loadavg_before;

    let watch_started = Instant::now();
    let mut observation = SrtRsReceiverObservation::default();
    let deadline = watch_started + SRT_RS_RECEIVER_OBSERVATION_WINDOW;
    while Instant::now() < deadline {
        let sink_sample = sink.observe();
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            let drop_sample =
                health["pipelines"][&pid]["outputs"][&bad_oid]["quality"]["packetsSentDrop"]
                    .as_u64()
                    .or_else(|| {
                        health["pipelines"][&pid]["outputs"][&bad_oid]["quality"]["packetsSentDrop"]
                            .as_i64()
                            .and_then(|value| u64::try_from(value).ok())
                    });
            observation.record_drop_sample(drop_sample);
        }
        if let Some(sample) = loadavg() {
            loadavg_during = Some(loadavg_during.map_or(sample, |current| current.max(sample)));
        }
        if let Ok((status, value)) = api.get_output_status(&pid, &bad_oid).await {
            let reason = value["backpressureReason"].as_str().unwrap_or_default();
            observation.record(reason, status.bytes_out, sink_sample.connected_now);
            if status.retrying || status.status == "retrying" || status.status == "failed" {
                observation.entered_retry_cycle = true;
                if observation.bytes_at_close == 0 {
                    observation.bytes_at_close = status.bytes_out;
                }
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let watch_elapsed = watch_started.elapsed();
    let loadavg_after = loadavg();
    let contention_burners = contention.handles.len();
    drop(contention);

    let sibling_progress =
        wait_for_outputs_live_and_progressing(&api, &pid, &sibling_output_ids, timeout).await;
    stop_mixed_outputs(&api, &pid, std::slice::from_ref(&bad_oid)).await;
    let retirement_deadline = Instant::now() + timeout;
    let mut retired_output_stopped = false;
    let mut sibling_receivers_progressed = false;
    while Instant::now() < retirement_deadline {
        if let Ok((status, _)) = api.get_output_status(&pid, &bad_oid).await
            && status.status == "stopped"
        {
            retired_output_stopped = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    // Start fresh receiver counters only after the target is observably stopped.
    let sibling_events_before: Vec<_> = sibling_sinks
        .iter()
        .map(|sink| sink.observe().data_events)
        .collect();
    let mut sibling_events_after = sibling_events_before.clone();
    while retired_output_stopped && Instant::now() < retirement_deadline {
        for (after, sink) in sibling_events_after.iter_mut().zip(&sibling_sinks) {
            *after = sink.observe().data_events;
        }
        sibling_receivers_progressed = sibling_events_after
            .iter()
            .zip(&sibling_events_before)
            .all(|(after, before)| after > before);
        if sibling_receivers_progressed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let sink_final = sink.observe();
    let final_output = observe_final_output(&api, &pid, &bad_oid).await;
    let passed = sink_final.accepted >= 1
        && sink_final.data_events > 0
        && sibling_progress.is_ok()
        && retired_output_stopped
        && sibling_receivers_progressed;

    println!(
        "[fault] SRT egress srt-rs receiver delivery: {} (sinkAccepted={} dataEvents={} sawBackpressured={} connectedWhileBackpressured={} sawStalled={} enteredRetryCycle={} reasons={:?} bytesBeforeReceiverDelivery={} siblings={} siblingProgress={} contentionBurners={} loadavgBefore={} loadavgDuring={} loadavgAfter={} dropsFirst={:?} dropsLast={:?} retiredOutputStopped={} siblingReceiversProgressed={} elapsed={:.1}s)",
        if passed { "PASS" } else { "FAIL" },
        sink_final.accepted,
        sink_final.data_events,
        observation.saw_backpressured,
        observation.connected_while_backpressured,
        observation.saw_stalled,
        observation.entered_retry_cycle,
        observation.reasons_seen,
        bytes_before_receiver_delivery,
        sibling_output_ids.len(),
        sibling_progress.is_ok(),
        contention_burners,
        loadavg_before.map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}")),
        loadavg_during.map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}")),
        loadavg_after.map_or_else(|| "n/a".to_string(), |v| format!("{v:.2}")),
        observation.drops_first,
        observation.drops_last,
        retired_output_stopped,
        sibling_receivers_progressed,
        watch_elapsed.as_secs_f64(),
    );

    stop_mixed_outputs(&api, &pid, &sibling_output_ids).await;
    stop_child(&mut pub_child).await;
    stop_child(&mut child).await;
    sink.stop();
    for sibling in sibling_sinks {
        sibling.stop();
    }

    Ok(json!({
        "test": "srt-egress-srt-rs-receiver-delivery",
        "passed": passed,
        "siblingOutputs": sibling_output_ids.len(),
        "bytesBeforeReceiverDelivery": bytes_before_receiver_delivery,
        "bytesAtBackpressure": observation.bytes_at_backpressure,
        "bytesAtClose": observation.bytes_at_close,
        "backpressureReasonsSeen": observation.reasons_seen,
        "sawBackpressured": observation.saw_backpressured,
        "connectedWhileBackpressured": observation.connected_while_backpressured,
        "sawStalled": observation.saw_stalled,
        "enteredRetryCycle": observation.entered_retry_cycle,
        "observationSecs": watch_elapsed.as_secs_f64(),
        "contentionBurnerThreads": contention_burners,
        "loadavgBefore": loadavg_before,
        "loadavgDuring": loadavg_during,
        "loadavgAfter": loadavg_after,
        "dropsFirst": observation.drops_first,
        "dropsLast": observation.drops_last,
        "srtRsSink": sink_final.to_json(),
        "siblingProgress": sibling_progress.as_ref().ok().cloned(),
        "siblingProgressError": sibling_progress.err(),
        "retiredOutputStopped": retired_output_stopped,
        "siblingReceiverDataBeforeRetirement": sibling_events_before,
        "siblingReceiverDataAfterRetirement": sibling_events_after,
        "siblingReceiversProgressedAfterRetirement": sibling_receivers_progressed,
        "status": final_output.status.clone().unwrap_or(Value::Null),
        "healthOutput": final_output.health.clone(),
    }))
}
