use super::super::resource_sweep::ffmpeg_children_stats;
use super::super::*;
use super::egress::{
    fault_rtmp_egress_output_churn, fault_rtmp_egress_sink_disappear,
    fault_rtmp_egress_sink_stalls, fault_srt_egress_sink_disappear,
};

pub(crate) async fn create_pipeline_with_stream_key(
    api: &RampApi,
    name: &str,
    stream_key: &str,
) -> Result<String, String> {
    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": name, "streamKey": stream_key}),
        )
        .await?;
    pipeline["pipeline"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("missing pipeline id for {name}"))
}

pub(crate) async fn create_pipeline(api: &RampApi, name: &str) -> Result<String, String> {
    create_pipeline_with_stream_key(api, name, name).await
}

pub(crate) async fn delete_pipeline_v1(api: &RampApi, pipeline_id: &str) -> Result<(), String> {
    let delete_url = format!("{}/api/v1/pipelines/{pipeline_id}", api.base_url);
    let mut request = api.client.delete(&delete_url);
    if let Some(cookie) = &api.cookie {
        request = request.header(reqwest::header::COOKIE, cookie);
    }
    let response = request
        .send()
        .await
        .map_err(|e| format!("delete pipeline {pipeline_id}: {e}"))?;
    if response.status().is_success() || response.status() == reqwest::StatusCode::NOT_FOUND {
        Ok(())
    } else {
        Err(format!(
            "delete pipeline {pipeline_id}: unexpected status {}",
            response.status()
        ))
    }
}

pub(crate) const RECOVERY_WARM_VIDEO_MIN: u64 = 10;

pub(crate) async fn wait_for_sink_video_above(
    metrics: &GeneralizedSinkMetrics,
    threshold: u64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        if metrics.video_count.load(Ordering::Relaxed) > threshold {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    false
}

pub(crate) fn health_input_snapshot(health: Option<&Value>, pipeline_id: &str) -> Value {
    health
        .map(|health| health["pipelines"][pipeline_id]["input"].clone())
        .unwrap_or(Value::Null)
}

pub(crate) fn disconnect_grace_remaining_bounded(input: &Value) -> bool {
    input["disconnectGraceRemainingMs"]
        .as_u64()
        .is_some_and(|remaining| remaining > 0 && remaining <= 5_000)
}

pub(crate) fn input_disconnect_cleared(input: &Value) -> bool {
    input["status"] == "on"
        && input["probeStatus"] == "ready"
        && input["lastSessionProtocol"].is_null()
        && input["lastDisconnectReason"].is_null()
        && input["lastFailurePhase"].is_null()
        && input["recentDisconnectError"] == false
}

/// Final output state checked by recovery/fault cells after perturbation.
pub(crate) struct FinalOutputObservation {
    pub(crate) status: Option<Value>,
    pub(crate) health: Value,
    pub(crate) running: bool,
    pub(crate) retrying: bool,
    pub(crate) error_cleared: bool,
    pub(crate) recent_failure_count: u64,
    pub(crate) flapping: bool,
    pub(crate) health_recent_failure_count: u64,
    pub(crate) health_flapping: bool,
}

pub(crate) async fn observe_final_output(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
) -> FinalOutputObservation {
    let status = api.get_output_status(pipeline_id, output_id).await.ok();
    let status_json = status.as_ref().map(|(_, json)| json.clone());
    let health = api.get_json("/api/v1/engine/health").await.ok();
    let output_health = health
        .as_ref()
        .map(|health| health["pipelines"][pipeline_id]["outputs"][output_id].clone())
        .unwrap_or(Value::Null);

    FinalOutputObservation {
        running: status.as_ref().map(|(status, _)| status.status.as_str()) == Some("running"),
        retrying: status.as_ref().is_some_and(|(status, _)| status.retrying),
        error_cleared: status
            .as_ref()
            .is_some_and(|(status, _)| status.last_error_is_empty()),
        recent_failure_count: status
            .as_ref()
            .map(|(status, _)| status.recent_failure_count)
            .unwrap_or(0),
        flapping: status.as_ref().is_some_and(|(status, _)| status.flapping),
        health_recent_failure_count: output_health["recentFailureCount"].as_u64().unwrap_or(0),
        health_flapping: output_health["flapping"].as_bool().unwrap_or(false),
        status: status_json,
        health: output_health,
    }
}

async fn output_running_without_retry(api: &RampApi, pipeline_id: &str, output_id: &str) -> bool {
    api.get_output_status(pipeline_id, output_id)
        .await
        .ok()
        .is_some_and(|(status, _)| status.status == "running" && !status.retrying)
}

pub(crate) async fn wait_for_output_running(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if output_running_without_retry(api, pipeline_id, output_id).await {
            return true;
        }
    }
    false
}

pub(crate) async fn wait_for_output_running_and_sink_video_above(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    metrics: &GeneralizedSinkMetrics,
    threshold: u64,
    timeout: Duration,
) -> bool {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        let video_progressed = metrics.video_count.load(Ordering::Relaxed) > threshold;
        if video_progressed && output_running_without_retry(api, pipeline_id, output_id).await {
            return true;
        }
    }
    false
}

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

    let result_path = work_dir.join("recovery.json");
    std::fs::write(&result_path, serde_json::to_string_pretty(&result).unwrap())
        .map_err(|e| e.to_string())?;
    println!("artifact={}", result_path.display());

    if !all_passed {
        return Err("recovery: not all tests passed".to_string());
    }
    Ok(result)
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

    let mut results: Vec<Value> = Vec::new();

    for case in publisher_disconnect_cases() {
        results
            .push(run_publisher_disconnect_case(&api, &ports, &fixture_h264, timeout, case).await?);
    }

    results.extend(
        recovery_live_cases(
            &mut api,
            &ports,
            &fixture_h264,
            sink_port,
            hls_put_port,
            timeout,
        )
        .await?,
    );

    for test_name in [
        "file-ingest-stop",
        "recording-stops-after-ingest-disconnect",
    ] {
        results.push(
            run_ingest_lifecycle_case(
                &api,
                &ports,
                &fixture_h264,
                ingest_lifecycle_case(test_name)?,
            )
            .await?,
        );
    }

    // ── 5. External transcoder tears down after ingest disappears ───────
    {
        let pid = create_pipeline(&api, "fault-transcoder").await?;

        let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
        let sink_server = start_generalized_sink_server(sink_port, sink_metrics.clone()).await?;

        let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live/fault-transcoder-sink");
        let oid = create_output(&api, &pid, "rtmp.720p.a0", &sink_url, "720p").await?;

        let mut pub_child = spawn_publisher(
            &fixture_h264,
            &format!("rtmp://127.0.0.1:{}/live/fault-transcoder", ports.rtmp),
            "flv",
            false,
        )
        .await?;
        wait_for_api_input_live(&api, &pid, timeout).await?;

        start_output(&api, &pid, &oid).await?;

        let restream_pid = child.id().ok_or("restream pid missing")?;
        let warm_deadline = Instant::now() + Duration::from_secs(15);
        let mut ffmpeg_spawned = false;
        let mut peak_ffmpeg_children = 0u64;
        let mut peak_transcoder_buffers = 0u64;
        let mut saw_output_bytes = false;
        while Instant::now() < warm_deadline {
            let ffmpeg = ffmpeg_children_stats(restream_pid)?;
            let telemetry = api.get_json("/api/v1/engine/telemetry").await?;
            let active_transcoder_buffers =
                telemetry["activeTranscoderBuffers"].as_u64().unwrap_or(0);
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
        let off_result = wait_for_api_input_off(&api, &pid, timeout).await;
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
    }

    // ── 6. RTMP egress sink disappears ──────────────────────────────────
    results.push(
        fault_rtmp_egress_sink_disappear(&api, &ports, &fixture_h264, sink_port, timeout).await?,
    );

    // ── 6b. RTMP egress output mid-stream churn ─────────────────────────
    results.push(
        fault_rtmp_egress_output_churn(&api, &ports, &fixture_h264, sink_port, timeout).await?,
    );

    // ── 7. RTMP egress sink stops draining and surfaces stalled ─────────
    results.push(
        fault_rtmp_egress_sink_stalls(&api, &ports, &fixture_h264, sink_port, timeout).await?,
    );

    // ── 8. SRT egress sink disappears ───────────────────────────────────
    results.push(fault_srt_egress_sink_disappear(&api, &ports, &fixture_h264, timeout).await?);

    for test_name in [
        "hls-preview-stops-after-ingest-disconnect",
        "file-ingest-eof-clears-and-restarts",
    ] {
        results.push(
            run_ingest_lifecycle_case(
                &api,
                &ports,
                &fixture_h264,
                ingest_lifecycle_case(test_name)?,
            )
            .await?,
        );
    }

    let history_contract = verify_live_history_contract(&api, &["egress.failed"]).await?;
    let external_transcoder_history = verify_external_transcoder_history_contract(&api).await?;
    println!("[fault.resilience] history contract verified");

    stop_child(&mut child).await;

    let all_passed = results.iter().all(|r| r["passed"] == true);
    let result = json!({
        "mode": "fault.resilience",
        "passed": all_passed,
        "tests": results,
        "historyContract": history_contract,
        "externalTranscoderHistory": external_transcoder_history,
    });

    let result_path = work_dir.join("fault.resilience.json");
    std::fs::write(&result_path, serde_json::to_string_pretty(&result).unwrap())
        .map_err(|e| e.to_string())?;
    println!("artifact={}", result_path.display());

    if !all_passed {
        return Err("fault.resilience: not all tests passed".to_string());
    }
    Ok(result)
}
