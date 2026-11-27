use super::super::*;
use super::egress::{
    fault_rtmp_egress_output_churn, fault_rtmp_egress_sink_disappear,
    fault_rtmp_egress_sink_stalls, fault_srt_egress_sink_disappear,
};
use super::rtmps_egress::{fault_rtmps_egress_sink_disappear, fault_rtmps_egress_sink_stalls};

pub(crate) async fn recovery() -> Result<Value, String> {
    let work_dir = artifact_path("recovery");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let sink_port = harness_port_defaults().sink;
    let hls_put_port = harness_port_defaults().hls_put;
    let ports = TestPorts::from_env();
    let timeout = Duration::from_secs(15);

    let (mut child, mut api) =
        start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture_h264 = checked_h264_fixture()?;
    let results = recovery_live_cases(
        &mut api,
        &ports,
        &fixture_h264,
        sink_port,
        hls_put_port,
        timeout,
        &work_dir,
    )
    .await?;

    let history_contract = verify_live_history_contract(&api, &["egress.failed"]).await?;
    println!("[recovery] history contract verified");

    stop_child(&mut child).await;

    let all_passed = results.iter().all(|r| r["passed"] == true);
    let result = json!({
        "mode": "recovery",
        "passed": all_passed,
        "tests": results,
        "historyContract": history_contract,
    });
    super::finish_fault_mode(&work_dir, "recovery", all_passed, result)
}

async fn terminate_restream_gracefully(
    child: &mut tokio::process::Child,
    log_path: &std::path::Path,
) -> Result<Value, String> {
    let pid = child
        .id()
        .ok_or_else(|| "Restream child has no process id before SIGTERM".to_string())?;
    // SAFETY: `pid` is the live child process owned by this harness.
    if unsafe { libc::kill(pid as libc::pid_t, libc::SIGTERM) } != 0 {
        let error = std::io::Error::last_os_error();
        stop_child(child).await;
        return Err(format!("send SIGTERM to Restream pid {pid}: {error}"));
    }

    let started = Instant::now();
    let status = match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
        Ok(Ok(status)) => status,
        Ok(Err(error)) => {
            stop_child(child).await;
            return Err(format!(
                "wait for Restream pid {pid} after SIGTERM: {error}"
            ));
        }
        Err(_) => {
            stop_child(child).await;
            return Err(format!(
                "Restream pid {pid} did not exit within five seconds after SIGTERM"
            ));
        }
    };
    let elapsed = started.elapsed();
    if !status.success() {
        return Err(format!(
            "Restream pid {pid} exited unsuccessfully after SIGTERM: {status}"
        ));
    }

    let logs = std::fs::read_to_string(log_path)
        .map_err(|error| format!("read Restream shutdown log {}: {error}", log_path.display()))?;
    if !logs.contains("restream.shutdown.completed") {
        return Err(format!(
            "Restream pid {pid} exited without restream.shutdown.completed in {}",
            log_path.display()
        ));
    }
    Ok(json!({
        "passed": true,
        "shutdownMs": elapsed.as_millis(),
        "shutdownCompleted": true
    }))
}

/// Live handles shared by the `fault.resilience` phases.
struct ResilienceEnv<'a> {
    api: &'a RampApi,
    ports: &'a TestPorts,
    fixture_h264: &'a Path,
    sink_port: u16,
    timeout: Duration,
    rtmps_cert: &'a Path,
    rtmps_key: &'a Path,
}

/// Publisher-disconnect, transient-recovery and first ingest-lifecycle cases.
async fn resilience_early_cases(
    api: &mut RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    work_dir: &Path,
    sink_port: u16,
    hls_put_port: u16,
    timeout: Duration,
) -> Result<Vec<Value>, String> {
    let mut results: Vec<Value> = Vec::new();

    for case in publisher_disconnect_cases() {
        results.push(run_publisher_disconnect_case(api, ports, fixture_h264, timeout, case).await?);
    }

    results.extend(
        recovery_live_cases(
            api,
            ports,
            fixture_h264,
            sink_port,
            hls_put_port,
            timeout,
            work_dir,
        )
        .await?,
    );

    for test_name in [
        "file-ingest-stop",
        "recording-stops-after-ingest-disconnect",
    ] {
        results.push(
            run_ingest_lifecycle_case(api, ports, fixture_h264, ingest_lifecycle_case(test_name)?)
                .await?,
        );
    }
    Ok(results)
}

/// An external transcoder tears its ffmpeg children down after ingest
/// disappears.
async fn resilience_external_transcoder_case(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
    restream: &Child,
    results: &mut Vec<Value>,
) -> Result<(), String> {
    let pid = create_pipeline(api, "fault-transcoder").await?;

    let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server = start_generalized_sink_server(sink_port, sink_metrics.clone()).await?;

    let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live/fault-transcoder-sink");
    let oid = create_output(api, &pid, "rtmp.720p.a0", &sink_url, "720p").await?;

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!("rtmp://127.0.0.1:{}/live/fault-transcoder", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    start_output(api, &pid, &oid).await?;

    let restream_pid = restream.id().ok_or("restream pid missing")?;
    let warm_deadline = Instant::now() + Duration::from_secs(15);
    let mut ffmpeg_spawned = false;
    let mut peak_ffmpeg_children = 0u64;
    let mut peak_transcoder_buffers = 0u64;
    let mut saw_output_bytes = false;
    while Instant::now() < warm_deadline {
        let ffmpeg = ffmpeg_children_stats(restream_pid)?;
        let telemetry = api.get_json("/api/v1/engine/telemetry").await?;
        let active_transcoder_buffers = telemetry["activeTranscoderBuffers"].as_u64().unwrap_or(0);
        peak_ffmpeg_children = peak_ffmpeg_children.max(ffmpeg.count);
        peak_transcoder_buffers = peak_transcoder_buffers.max(active_transcoder_buffers);
        saw_output_bytes |= sink_metrics.bytes.load(Ordering::Relaxed) > 0;
        if (ffmpeg.count > 0 || active_transcoder_buffers > 0) && saw_output_bytes {
            ffmpeg_spawned = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }

    stop_child(&mut pub_child).await;
    let started = Instant::now();
    let off_result = wait_for_api_input_off(api, &pid, timeout).await;
    let cleanup_deadline = Instant::now() + Duration::from_secs(15);
    let mut cleanup_ok = false;
    let mut final_ffmpeg_count = u64::MAX;
    let mut final_transcoder_buffers = u64::MAX;
    while Instant::now() < cleanup_deadline {
        let ffmpeg = ffmpeg_children_stats(restream_pid)?;
        let telemetry = api.get_json("/api/v1/engine/telemetry").await?;
        let active_transcoder_buffers = telemetry["activeTranscoderBuffers"]
            .as_u64()
            .unwrap_or(u64::MAX);
        final_ffmpeg_count = ffmpeg.count;
        final_transcoder_buffers = active_transcoder_buffers;
        if ffmpeg.count == 0 && active_transcoder_buffers == 0 {
            cleanup_ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    let status = api
        .get_json(&format!("/api/v1/pipelines/{pid}/outputs/{oid}/status"))
        .await;
    let output_cleaned_up = match &status {
        Err(_) => true,
        Ok(json) if json.get("error").is_some() => true,
        Ok(json) => {
            json["endedAt"].is_string()
                && matches!(json["status"].as_str(), Some("stopped" | "failed"))
        }
    };
    let elapsed = started.elapsed();
    let passed = ffmpeg_spawned && off_result.is_ok() && cleanup_ok && output_cleaned_up;
    println!(
        "[fault] External transcoder tears down: {} (spawned={}, peakFfmpegChildren={}, peakTranscoderBuffers={}, finalFfmpegChildren={}, activeTranscoderBuffers={}, outputCleanedUp={}, {:.1}s)",
        if passed { "PASS" } else { "FAIL" },
        ffmpeg_spawned,
        peak_ffmpeg_children,
        peak_transcoder_buffers,
        final_ffmpeg_count,
        final_transcoder_buffers,
        output_cleaned_up,
        elapsed.as_secs_f64()
    );
    results.push(json!({
        "test": "external-transcoder-stops-after-ingest-disconnect",
        "passed": passed,
        "elapsedMs": elapsed.as_millis(),
        "inputOffError": off_result.err(),
        "ffmpegSpawned": ffmpeg_spawned,
        "peakFfmpegChildren": peak_ffmpeg_children,
        "peakTranscoderBuffers": peak_transcoder_buffers,
        "sawOutputBytes": saw_output_bytes,
        "finalFfmpegChildren": final_ffmpeg_count,
        "finalActiveTranscoderBuffers": final_transcoder_buffers,
        "outputCleanedUp": output_cleaned_up,
    }));

    stop_generalized_sink_server(sink_server);
    Ok(())
}

/// RTMP egress loss, churn and stalled-sink cases.
async fn resilience_rtmp_egress_cases(env: &ResilienceEnv<'_>) -> Result<Vec<Value>, String> {
    let mut results = Vec::new();
    // ── 6. RTMP egress sink disappears ──────────────────────────────────
    results.push(
        fault_rtmp_egress_sink_disappear(
            env.api,
            env.ports,
            env.fixture_h264,
            env.sink_port,
            env.timeout,
        )
        .await?,
    );

    // ── 6b. RTMP egress output mid-stream churn ─────────────────────────
    results.push(
        fault_rtmp_egress_output_churn(
            env.api,
            env.ports,
            env.fixture_h264,
            env.sink_port,
            env.timeout,
        )
        .await?,
    );

    // ── 7. RTMP egress sink stops draining and surfaces stalled ──────────
    results.push(
        fault_rtmp_egress_sink_stalls(
            env.api,
            env.ports,
            env.fixture_h264,
            env.sink_port,
            env.timeout,
        )
        .await?,
    );
    Ok(results)
}

/// RTMPS egress reconnect cases.
async fn resilience_rtmps_egress_cases(env: &ResilienceEnv<'_>) -> Result<Vec<Value>, String> {
    let mut results = Vec::new();
    // ── 7b. RTMPS egress reconnects after the sink disappears ────────────
    results.push(
        fault_rtmps_egress_sink_disappear(
            env.api,
            env.ports,
            env.fixture_h264,
            env.sink_port,
            env.timeout,
            env.rtmps_cert,
            env.rtmps_key,
        )
        .await?,
    );

    // ── 7c. RTMPS slow receiver surfaces stalled output ─────────────────
    results.push(
        fault_rtmps_egress_sink_stalls(
            env.api,
            env.ports,
            env.fixture_h264,
            env.sink_port,
            env.timeout,
            env.rtmps_cert,
            env.rtmps_key,
        )
        .await?,
    );
    Ok(results)
}

/// Trailing ingest-lifecycle cases that run after the egress suites.
async fn resilience_late_lifecycle_cases(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
) -> Result<Vec<Value>, String> {
    let mut results = Vec::new();
    for test_name in [
        "hls-preview-stops-after-ingest-disconnect",
        "file-ingest-eof-clears-and-restarts",
    ] {
        results.push(
            run_ingest_lifecycle_case(api, ports, fixture_h264, ingest_lifecycle_case(test_name)?)
                .await?,
        );
    }
    Ok(results)
}

/// RTMP and RTMPS outputs reconnecting when restream shuts down gracefully.
async fn resilience_shutdown_cases(
    env: &ResilienceEnv<'_>,
    child: &mut Child,
    log_path: &Path,
) -> Result<Vec<Value>, String> {
    let api = env.api;
    let ports = env.ports;
    let fixture_h264 = env.fixture_h264;
    let sink_port = env.sink_port;
    let timeout = env.timeout;
    let rtmps_cert = env.rtmps_cert;
    let rtmps_key = env.rtmps_key;
    let mut results: Vec<Value> = Vec::new();

    let shutdown_pipeline = create_pipeline(api, "fault-graceful-rtmp-rtmps").await?;
    let rtmp_shutdown_port = sink_port
        .checked_add(1)
        .ok_or_else(|| "SINK_PORT must leave room for the RTMP shutdown sink".to_string())?;
    let rtmp_shutdown_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let rtmp_shutdown_sink =
        start_generalized_sink_server(rtmp_shutdown_port, rtmp_shutdown_metrics.clone()).await?;
    let rtmps_shutdown_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let rtmps_shutdown_sink = match start_generalized_rtmps_sink_server(
        sink_port,
        rtmps_cert,
        rtmps_key,
        rtmps_shutdown_metrics.clone(),
    )
    .await
    {
        Ok(server) => server,
        Err(error) => {
            stop_generalized_sink_server(rtmp_shutdown_sink);
            return Err(error);
        }
    };
    let rtmp_shutdown_output_id = create_output(
        api,
        &shutdown_pipeline,
        "rtmp-shutdown-sink",
        &format!("rtmp://127.0.0.1:{rtmp_shutdown_port}/live/fault-graceful-rtmp-sink"),
        "source",
    )
    .await?;
    let rtmps_shutdown_output_id = create_output(
        api,
        &shutdown_pipeline,
        "rtmps-shutdown-sink",
        &format!("rtmps://127.0.0.1:{sink_port}/live/fault-graceful-rtmps-sink"),
        "source",
    )
    .await?;
    let mut shutdown_publisher = spawn_publisher(
        fixture_h264,
        &format!(
            "rtmp://127.0.0.1:{}/live/fault-graceful-rtmp-rtmps",
            ports.rtmp
        ),
        "flv",
        false,
    )
    .await?;
    let startup_result = async {
        wait_for_api_input_live(api, &shutdown_pipeline, timeout).await?;
        start_output(api, &shutdown_pipeline, &rtmp_shutdown_output_id).await?;
        start_output(api, &shutdown_pipeline, &rtmps_shutdown_output_id).await?;
        let rtmp_initial_media =
            wait_for_sink_video_above(&rtmp_shutdown_metrics, 9, timeout).await;
        let rtmps_initial_media =
            wait_for_sink_video_above(&rtmps_shutdown_metrics, 9, timeout).await;
        Ok::<(bool, bool), String>((rtmp_initial_media, rtmps_initial_media))
    }
    .await;
    let (rtmp_initial_media, rtmps_initial_media) = match startup_result {
        Ok(initial_media) => initial_media,
        Err(error) => {
            stop_child(&mut shutdown_publisher).await;
            stop_generalized_sink_server(rtmp_shutdown_sink);
            stop_generalized_sink_server(rtmps_shutdown_sink);
            stop_child(child).await;
            return Err(error);
        }
    };
    stop_generalized_sink_server(rtmp_shutdown_sink);
    stop_generalized_sink_server(rtmps_shutdown_sink);

    let retry_deadline = Instant::now() + Duration::from_secs(10);
    let mut rtmp_saw_retrying = false;
    let mut rtmps_saw_retrying = false;
    while Instant::now() < retry_deadline && !(rtmp_saw_retrying && rtmps_saw_retrying) {
        if !rtmp_saw_retrying
            && let Ok((status, _)) = api
                .get_output_status(&shutdown_pipeline, &rtmp_shutdown_output_id)
                .await
        {
            rtmp_saw_retrying = status.status == "retrying";
        }
        if !rtmps_saw_retrying
            && let Ok((status, _)) = api
                .get_output_status(&shutdown_pipeline, &rtmps_shutdown_output_id)
                .await
        {
            rtmps_saw_retrying = status.status == "retrying";
        }
        if !(rtmp_saw_retrying && rtmps_saw_retrying) {
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    }

    let shutdown_result = terminate_restream_gracefully(child, log_path).await;
    stop_child(&mut shutdown_publisher).await;
    let shutdown_result = match shutdown_result {
        Ok(result) => result,
        Err(error) => json!({"passed": false, "error": error}),
    };
    let shutdown_passed = shutdown_result["passed"] == true;
    let mut rtmp_shutdown_result = shutdown_result.clone();
    rtmp_shutdown_result["test"] = json!("rtmp-egress-reconnect-shutdown");
    rtmp_shutdown_result["initialMedia"] = json!(rtmp_initial_media);
    rtmp_shutdown_result["sawRetrying"] = json!(rtmp_saw_retrying);
    rtmp_shutdown_result["passed"] =
        json!(rtmp_initial_media && rtmp_saw_retrying && shutdown_passed);
    results.push(rtmp_shutdown_result);
    let mut rtmps_shutdown_result = shutdown_result;
    rtmps_shutdown_result["test"] = json!("rtmps-egress-reconnect-shutdown");
    rtmps_shutdown_result["initialMedia"] = json!(rtmps_initial_media);
    rtmps_shutdown_result["sawRetrying"] = json!(rtmps_saw_retrying);
    rtmps_shutdown_result["passed"] =
        json!(rtmps_initial_media && rtmps_saw_retrying && shutdown_passed);
    results.push(rtmps_shutdown_result);
    Ok(results)
}

pub(crate) async fn fault_resilience() -> Result<Value, String> {
    let work_dir = artifact_path("fault.resilience");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let sink_port = harness_port_defaults().sink;
    let hls_put_port = harness_port_defaults().hls_put;
    let ports = TestPorts::from_env();
    let timeout = Duration::from_secs(15);

    let (mut child, mut api) =
        start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture_h264 = checked_h264_fixture()?;
    preflight_rtmps_ktls_capabilities(&api).await?;
    let (rtmps_cert, rtmps_key) = restream_engine::test_fixtures::rtmps_harness_cert_fixture()?;

    let mut results: Vec<Value> = Vec::new();
    results.extend(
        resilience_early_cases(
            &mut api,
            &ports,
            &fixture_h264,
            &work_dir,
            sink_port,
            hls_put_port,
            timeout,
        )
        .await?,
    );

    resilience_external_transcoder_case(
        &api,
        &ports,
        &fixture_h264,
        sink_port,
        timeout,
        &child,
        &mut results,
    )
    .await?;

    let env = ResilienceEnv {
        api: &api,
        ports: &ports,
        fixture_h264: fixture_h264.as_path(),
        sink_port,
        timeout,
        rtmps_cert: rtmps_cert.as_path(),
        rtmps_key: rtmps_key.as_path(),
    };
    results.extend(resilience_rtmp_egress_cases(&env).await?);
    results.extend(resilience_rtmps_egress_cases(&env).await?);

    // ── 8. SRT egress sink disappears ──────────────────────────────────
    results.push(
        fault_srt_egress_sink_disappear(env.api, env.ports, env.fixture_h264, env.timeout).await?,
    );

    results.extend(resilience_late_lifecycle_cases(env.api, env.ports, env.fixture_h264).await?);

    let history_contract = verify_live_history_contract(&api, &["egress.failed"]).await?;
    let external_transcoder_history = verify_external_transcoder_history_contract(&api).await?;
    println!("[fault.resilience] history contract verified");

    results.extend(resilience_shutdown_cases(&env, &mut child, &log_path).await?);

    stop_child(&mut child).await;

    let all_passed = results.iter().all(|r| r["passed"] == true);
    let result = json!({
        "mode": "fault.resilience",
        "passed": all_passed,
        "tests": results,
        "historyContract": history_contract,
        "externalTranscoderHistory": external_transcoder_history,
    });
    super::finish_fault_mode(&work_dir, "fault.resilience", all_passed, result)
}
