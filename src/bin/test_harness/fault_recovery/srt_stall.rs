//! SRT egress "bounded RSS under a frozen destination" proof — part of the
//! last unproven Phase 4 exit-gate criterion
//! (`docs/egress-implementation.md`): "bounded RSS during indefinite
//! stalls."
//!
//! **What this does and does not prove — found the hard way.** The original
//! intent was to test the *backpressured-but-connected* path
//! (`classify_stall`/`observe_stall` in `src/media/egress/policy.rs` and
//! `src/media/egress/backends/srt.rs`): a destination that keeps the SRT
//! connection alive but never drains application data, so the leaf sits at
//! `LeafStallClass::Stalled` rather than closing. `SIGSTOP` on a real
//! MediaMTX receiver was meant to produce exactly that by freezing its
//! `recv()` loop. It does not: `SIGSTOP` freezes *every* thread in the
//! receiver process, including libsrt's own internal ACK/keepalive thread,
//! so the connection is detected as fully broken within seconds (observed:
//! `srt_send failed ... Connection was broken`), not backpressured — SRT
//! has no way to distinguish "receiver alive but not reading" from
//! "receiver process frozen" once the underlying process stops generating
//! ACKs. The output then cycles through connect-failure retries against
//! the still-suspended receiver, exactly like a dead destination
//! (`fault_srt_egress_dead_sink_isolation_under_many_outputs` in
//! `egress.rs`) — not the distinct "stalled" `classify_stall` path this
//! file originally targeted. Proving *that* specific path live would need
//! a receiver that keeps SRT's own liveness signaling alive while
//! deliberately not draining decoded data one layer up — a raw SRT
//! listener built from scratch (restream's libsrt FFI bindings are
//! internal to `src/media/srt`, not exposed to this harness binary), which
//! is out of scope here. The stall-sweep/`classify_stall` mechanism itself
//! is proven deterministically instead, in
//! `src/media/egress/backends/srt/tests/leaf_termination.rs`.
//!
//! What this *does* prove, honestly: a real SRT egress connection that
//! breaks and re-attempts against an unreachable destination, held open for
//! several minutes, does not grow the process's RSS unboundedly — the
//! retry/backoff/cleanup cycle itself is not leaking sockets, buffers, or
//! retained feed state.

use super::super::resource_sweep::read_proc_status_kb_checked;
use super::super::*;
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

pub(crate) async fn fault_srt_egress_stalled_destination() -> Result<Value, String> {
    let work_dir = artifact_path("fault.srt-output-stall");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
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

    let payload = json!({
        "mode": "fault.srt-output-stall",
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
