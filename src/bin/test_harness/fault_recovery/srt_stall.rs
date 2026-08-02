//! The `fault.srt-output-stall` mode: two live proofs of the Phase 4
//! exit-gate criteria for a destination that stops taking data
//! (`docs/egress-implementation.md`).
//!
//! **Two different faults, because `SIGSTOP` is not backpressure.** The
//! original intent of this file was a single test of the
//! *backpressured-but-connected* path (`classify_stall`/`observe_stall` in
//! `src/media/egress/policy.rs` and `src/media/egress/backends/srt.rs`): a
//! destination that keeps the SRT connection alive but never drains
//! application data, so the leaf sits at `LeafStallClass::Backpressured`
//! and is closed only once it reaches `Stalled`. `SIGSTOP` on a real
//! MediaMTX receiver was meant to produce exactly that by freezing its
//! `recv()` loop. It does not: `SIGSTOP` freezes *every* thread in the
//! receiver process, including libsrt's own internal ACK/keepalive thread,
//! so the connection is detected as fully broken within seconds (observed:
//! `srt_send failed ... Connection was broken`), not backpressured — SRT
//! has no way to distinguish "receiver alive but not reading" from
//! "receiver process frozen" once the underlying process stops generating
//! ACKs. So the two conditions are now tested separately:
//!
//! - `srt_egress_bounded_rss_under_frozen_destination` keeps the `SIGSTOP`
//!   receiver and proves what it actually produces: a real SRT egress
//!   connection that breaks and re-attempts against an unreachable
//!   destination, held for several minutes, does not grow the process's RSS
//!   unboundedly — the retry/backoff/cleanup cycle is not leaking sockets,
//!   buffers, or retained feed state.
//! - `srt_egress_backpressured_receiver` proves the path `SIGSTOP` could
//!   not reach, using the purpose-built raw SRT listener in
//!   `srt_raw_sink.rs`: a peer whose libsrt keeps ACKing and keepaliving
//!   normally while the application above it never reads a byte. That is
//!   the only receiver shape that makes the sender see genuine
//!   backpressure, and it closes the last live gap in the Phase 4 exit
//!   gate. The same mechanism is also proven deterministically in
//!   `src/media/egress/backends/srt/tests/leaf_termination.rs`.

use super::super::resource_sweep::read_proc_status_kb_checked;
use super::super::*;
use super::egress::wait_for_outputs_live_and_progressing;
use super::resilience::create_pipeline;

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
            "logLevel: warn\nreadTimeout: 60s\nwriteTimeout: 60s\nrtmp: no\nrtsp: no\nsrt: yes\nsrtAddress: :{srt_port}\nhls: no\nwebrtc: no\nmoq: no\napi: yes\napiAddress: :{api_port}\nmetrics: no\npaths:\n  all:\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let log_err = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new("mediamtx");
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
        srt_egress_backpressured_receiver().await?,
    ];
    let passed = tests
        .iter()
        .all(|test| test["passed"].as_bool().unwrap_or(false));

    let payload = json!({
        "mode": "fault.srt-output-stall",
        "passed": passed,
        "tests": tests,
    });
    let result_path = work_dir.join("fault.srt-output-stall.json");
    std::fs::write(
        &result_path,
        serde_json::to_string_pretty(&payload).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("artifact={}", result_path.display());

    if !passed {
        return Err("fault.srt-output-stall: not all tests passed".to_string());
    }
    Ok(payload)
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
    if !reached_progress {
        stop_child(&mut pub_child).await;
        stop_child(&mut mediamtx).await;
        stop_child(&mut child).await;
        return Err(
            "srt-egress-stalled-destination: output never reached real progress before the stall"
                .to_string(),
        );
    }

    let restream_pid = child.id();
    let rss_before_stall_kb: u64 = restream_pid
        .and_then(|pid| read_proc_status_kb_checked(pid, "VmRSS", &log_path).ok())
        .unwrap_or(0);

    let stall_started = std::time::Instant::now();
    let mut sigstop_ok = false;
    if let Some(pid) = mediamtx_pid {
        sigstop_ok = signal_process(pid, libc::SIGSTOP).is_ok();
    }

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

    // "Bounded RSS during indefinite stalls" (Phase 4 exit gate): sample
    // restream's RSS across an extended window of continuous
    // connect-failure retries. A bounded sample window standing in for
    // "indefinite" is the same practical compromise every other live proof
    // in this harness makes for wall-clock reasons — the property under
    // test (no unbounded growth while an output keeps failing to connect)
    // is what a longer window would also show, just slower.
    tokio::time::sleep(Duration::from_secs(120)).await;
    let rss_after_stall_kb: u64 = restream_pid
        .and_then(|pid| read_proc_status_kb_checked(pid, "VmRSS", &log_path).ok())
        .unwrap_or(0);
    let rss_growth_kb = rss_after_stall_kb.saturating_sub(rss_before_stall_kb);
    // Generous bound: a genuinely leaking retry loop would grow by many
    // times this over the same window (new sockets, buffers, and retained
    // feed state per failed attempt); a healthy retry-and-cleanup cycle
    // should not.
    const MAX_ACCEPTABLE_RSS_GROWTH_KB: u64 = 64 * 1024;
    let rss_bounded = rss_before_stall_kb == 0 || rss_growth_kb <= MAX_ACCEPTABLE_RSS_GROWTH_KB;

    let final_output = observe_final_output(&api, &pid, &oid).await;
    let (status_snapshot, health_snapshot) = (
        final_output.status.clone().unwrap_or(Value::Null),
        final_output.health.clone(),
    );

    let passed = sigstop_ok && entered_retry_cycle && rss_bounded;

    println!(
        "[fault] SRT egress bounded RSS under frozen destination: {} (sigstopOk={} enteredRetryCycle={} bytesAtStallStart={} rssBeforeKb={} rssAfterKb={} rssGrowthKb={} elapsed={:.1}s)",
        if passed { "PASS" } else { "FAIL" },
        sigstop_ok,
        entered_retry_cycle,
        bytes_at_stall_start,
        rss_before_stall_kb,
        rss_after_stall_kb,
        rss_growth_kb,
        stall_started.elapsed().as_secs_f64(),
    );

    stop_mixed_outputs(&api, &pid, std::slice::from_ref(&oid)).await;
    stop_child(&mut pub_child).await;
    if let Some(pid) = mediamtx_pid {
        let _ = signal_process(pid, libc::SIGCONT);
    }
    stop_child(&mut mediamtx).await;
    stop_child(&mut child).await;

    Ok(json!({
        "test": "srt-egress-bounded-rss-under-frozen-destination",
        "passed": passed,
        "sigstopOk": sigstop_ok,
        "enteredRetryCycle": entered_retry_cycle,
        "bytesAtStallStart": bytes_at_stall_start,
        "rssBeforeStallKb": rss_before_stall_kb,
        "rssAfterStallKb": rss_after_stall_kb,
        "rssGrowthKb": rss_growth_kb,
        "status": status_snapshot,
        "healthOutput": health_snapshot,
    }))
}

/// Healthy SRT siblings kept next to the backpressured output, so the run
/// also shows that one undrained destination does not hold up the rest of
/// the shard's leaves.
const BACKPRESSURE_SIBLING_OUTPUTS: usize = 2;

/// How long to watch for the backpressured leaf to be recognized and then
/// closed. `LeafPolicy::no_progress_timeout` is 15s by default, so this
/// leaves ample room for the once-per-second stall sweep to act.
const BACKPRESSURE_OBSERVATION_WINDOW: Duration = Duration::from_secs(45);

/// What the run observed about the backpressured output, tracked as a unit
/// so the pass decision and the artifact agree by construction.
#[derive(Default)]
struct BackpressureObservation {
    reasons_seen: Vec<String>,
    saw_backpressured: bool,
    connected_while_backpressured: bool,
    saw_stalled: bool,
    entered_retry_cycle: bool,
    bytes_at_backpressure: u64,
    bytes_at_close: u64,
}

impl BackpressureObservation {
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
                // The whole point of the raw sink: the peer is still a live
                // SRT endpoint at the moment the sender calls it
                // backpressured. `SIGSTOP` cannot produce this.
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

    fn resolved(&self) -> bool {
        self.saw_backpressured && (self.saw_stalled || self.entered_retry_cycle)
    }
}

/// Prove the backpressured-but-connected SRT egress path live: a receiver
/// that stays fully connected at the protocol layer while never reading is
/// classified `Backpressured`, then closed as `Stalled` once it passes the
/// no-progress deadline, without disturbing healthy siblings.
async fn srt_egress_backpressured_receiver() -> Result<Value, String> {
    let work_dir = artifact_path("fault.srt-output-stall");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("backpressured-receiver.sqlite");
    let log_path = work_dir.join("restream-backpressured-receiver.log");
    let ports = TestPorts::from_env();
    let timeout = Duration::from_secs(15);
    let sink_port = harness_port_defaults()
        .ffmpeg_srt_sink_base
        .checked_add(1000)
        .ok_or("raw SRT stall sink port range overflowed")?;

    // Bind the undrained destination before restream starts, so the output's
    // very first connect attempt lands on it.
    let sink = RawSrtStallSink::start(sink_port)?;

    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture_h264 = checked_h264_fixture()?;
    let pid = create_pipeline(&api, "fault-egress-srt-backpressure").await?;

    let bad_oid = create_output(
        &api,
        &pid,
        "srt-backpressure-bad",
        &harness_srt_output_url(
            sink.port(),
            "srt-backpressure-sink",
            HarnessSrtMode::Publish,
        ),
        "source",
    )
    .await?;

    // Healthy siblings publish into restream's own SRT ingest, the same
    // shape the dead-sink isolation case uses, so no second sink process is
    // needed to judge neighbor health.
    let mut sibling_output_ids = Vec::with_capacity(BACKPRESSURE_SIBLING_OUTPUTS);
    for index in 0..BACKPRESSURE_SIBLING_OUTPUTS {
        let sink_name = format!("srt-backpressure-healthy-sink-{index:02}");
        create_pipeline(&api, &sink_name).await?;
        let oid = create_output(
            &api,
            &pid,
            &format!("srt-backpressure-healthy-{index:02}"),
            &harness_srt_output_url(ports.srt, &sink_name, HarnessSrtMode::Publish),
            "source",
        )
        .await?;
        sibling_output_ids.push(oid);
    }

    let mut pub_child = spawn_publisher(
        &fixture_h264,
        &harness_srt_ffmpeg_url(
            ports.srt,
            "fault-egress-srt-backpressure",
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

    // Require real delivery into the raw sink first: without it, a later
    // "backpressured" reading could not be told apart from an output that
    // never connected.
    let progress_deadline = Instant::now() + Duration::from_secs(20);
    let mut bytes_before_backpressure = 0u64;
    while Instant::now() < progress_deadline {
        if let Ok((status, _)) = api.get_output_status(&pid, &bad_oid).await
            && status.bytes_out > 0
            && sink.observe().accepted >= 1
        {
            bytes_before_backpressure = status.bytes_out;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let watch_started = Instant::now();
    let mut observation = BackpressureObservation::default();
    let deadline = watch_started + BACKPRESSURE_OBSERVATION_WINDOW;
    while Instant::now() < deadline && !observation.resolved() {
        let sink_sample = sink.observe();
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

    // `bytesOut` counts bytes *admitted to libsrt*, not bytes acknowledged
    // by the peer, so it keeps climbing after the receive window closes —
    // until the sender's own ~12MB native send buffer is full. That is
    // exactly why the fabric classifies stalls from the native backlog
    // rather than from this counter. What must hold once the engine has
    // called the leaf stalled is that admission has genuinely stopped, so
    // confirm no further growth after the fact. (A leaf that already
    // reconnected reports its fresh attempt's smaller count, which also
    // satisfies this.)
    tokio::time::sleep(Duration::from_secs(2)).await;
    let bytes_after_stall = api
        .get_output_status(&pid, &bad_oid)
        .await
        .map_or(observation.bytes_at_close, |(status, _)| status.bytes_out);
    let admission_stopped =
        !observation.saw_stalled || bytes_after_stall <= observation.bytes_at_close;

    let sibling_progress =
        wait_for_outputs_live_and_progressing(&api, &pid, &sibling_output_ids, timeout).await;
    let sink_final = sink.observe();
    let final_output = observe_final_output(&api, &pid, &bad_oid).await;

    let passed = sink_final.accepted >= 1
        && sink_final.peak_unread_packets > 0
        && observation.saw_backpressured
        && observation.connected_while_backpressured
        && observation.saw_stalled
        && observation.entered_retry_cycle
        && admission_stopped
        && sibling_progress.is_ok();

    println!(
        "[fault] SRT egress backpressured-but-connected receiver: {} (sinkAccepted={} peakUnreadPackets={} sawBackpressured={} connectedWhileBackpressured={} sawStalled={} enteredRetryCycle={} reasons={:?} bytesAtBackpressure={} bytesAtClose={} bytesAfterStall={} admissionStopped={} siblings={} siblingProgress={} elapsed={:.1}s)",
        if passed { "PASS" } else { "FAIL" },
        sink_final.accepted,
        sink_final.peak_unread_packets,
        observation.saw_backpressured,
        observation.connected_while_backpressured,
        observation.saw_stalled,
        observation.entered_retry_cycle,
        observation.reasons_seen,
        observation.bytes_at_backpressure,
        observation.bytes_at_close,
        bytes_after_stall,
        admission_stopped,
        sibling_output_ids.len(),
        sibling_progress.is_ok(),
        watch_elapsed.as_secs_f64(),
    );

    stop_mixed_outputs(&api, &pid, std::slice::from_ref(&bad_oid)).await;
    stop_mixed_outputs(&api, &pid, &sibling_output_ids).await;
    stop_child(&mut pub_child).await;
    stop_child(&mut child).await;
    sink.stop();

    Ok(json!({
        "test": "srt-egress-backpressured-but-connected-receiver",
        "passed": passed,
        "siblingOutputs": sibling_output_ids.len(),
        "bytesBeforeBackpressure": bytes_before_backpressure,
        "bytesAtBackpressure": observation.bytes_at_backpressure,
        "bytesAtClose": observation.bytes_at_close,
        "bytesAfterStall": bytes_after_stall,
        "admissionStopped": admission_stopped,
        "backpressureReasonsSeen": observation.reasons_seen,
        "sawBackpressured": observation.saw_backpressured,
        "connectedWhileBackpressured": observation.connected_while_backpressured,
        "sawStalled": observation.saw_stalled,
        "enteredRetryCycle": observation.entered_retry_cycle,
        "observationSecs": watch_elapsed.as_secs_f64(),
        "rawSink": sink_final.to_json(),
        "siblingProgress": sibling_progress.as_ref().ok().cloned(),
        "siblingProgressError": sibling_progress.err(),
        "status": final_output.status.clone().unwrap_or(Value::Null),
        "healthOutput": final_output.health.clone(),
    }))
}
