use super::super::*;
use super::resilience::{
    create_pipeline, delete_pipeline_v1, observe_final_output, wait_for_sink_video_above,
};

pub(crate) fn effective_fault_output_stall_siblings(
    configured_siblings: usize,
    n_per_group: Option<usize>,
) -> usize {
    let configured = configured_siblings.max(1);
    let n_per_group = n_per_group.unwrap_or(configured).max(1);
    configured.min(n_per_group)
}

pub(crate) fn fault_output_stall_sibling_count() -> usize {
    let configured = env_usize("FAULT_OUTPUT_STALL_SIBLINGS", 12);
    let n_per_group = std::env::var("N_PER_GROUP")
        .ok()
        .and_then(|value| value.parse::<usize>().ok());
    effective_fault_output_stall_siblings(configured, n_per_group)
}

pub(super) async fn fault_rtmp_egress_sink_disappear(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-egress-rtmp").await?;

    let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server = start_generalized_sink_server(sink_port, sink_metrics.clone()).await?;

    let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live/fault-egress-rtmp-sink");
    let oid = create_output(api, &pid, "rtmp-sink", &sink_url, "source").await?;

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!("rtmp://127.0.0.1:{}/live/fault-egress-rtmp", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    start_output(api, &pid, &oid).await?;

    let _ = wait_for_sink_video_above(&sink_metrics, 9, timeout).await;
    println!("[fault] RTMP egress delivering data");

    stop_generalized_sink_server(sink_server);

    let started = Instant::now();
    let retry =
        wait_for_output_retry_or_cleanup_observation(api, &pid, &oid, Duration::from_secs(10))
            .await;
    let elapsed = started.elapsed();
    let recovery_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let recovered_server =
        start_generalized_sink_server(sink_port, recovery_metrics.clone()).await?;

    let recovery_started = Instant::now();
    let recovery_deadline = recovery_started + Duration::from_secs(25);
    let mut recovered = false;
    let mut recovery_status = String::from("unknown");
    let mut saw_retrying = retry.status_visible;
    while Instant::now() < recovery_deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok((status, _)) = api.get_output_status(&pid, &oid).await {
            recovery_status = status.status;
            if recovery_status == "retrying" {
                saw_retrying = true;
            }
        }
        if recovery_metrics.video_count.load(Ordering::Relaxed) >= 10 {
            recovered = true;
            break;
        }
    }
    let final_output = observe_final_output(api, &pid, &oid).await;
    stop_generalized_sink_server(recovered_server);
    let retry_phase_ok = output_retry_or_cleanup_phase_ok(&retry);
    println!(
        "[fault] RTMP egress sink disappear: {} (phase={}, hasError={}, sawRetrying={}, healthSawRetrying={}, recovered={}, recoveryStatus={}, finalRetrying={}, {:.1}s)",
        if retry_phase_ok
            && recovered
            && saw_retrying
            && retry.health_visible
            && !final_output.retrying
        {
            "PASS"
        } else {
            "FAIL"
        },
        retry.phase,
        retry.has_error,
        saw_retrying,
        retry.health_visible,
        recovered,
        recovery_status,
        final_output.retrying,
        elapsed.as_secs_f64()
    );

    stop_child(&mut pub_child).await;

    Ok(json!({
        "test": "rtmp-egress-sink-disappear",
        "passed": retry_phase_ok && recovered && saw_retrying && retry.health_visible && !final_output.retrying,
        "phase": retry.phase,
        "hasError": retry.has_error,
        "elapsedMs": elapsed.as_millis(),
        "sawRetrying": saw_retrying,
        "healthSawRetrying": retry.health_visible,
        "retryAttempts": retry.attempts,
        "retryBackoffMs": retry.backoff_ms,
        "recovered": recovered,
        "recoveryStatus": recovery_status,
        "finalRetrying": final_output.retrying,
    }))
}

pub(super) async fn fault_rtmp_egress_output_churn(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-egress-churn").await?;
    let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server = start_generalized_sink_server(sink_port, sink_metrics.clone()).await?;

    let sink_url_1 = format!("rtmp://127.0.0.1:{sink_port}/live/fault-churn-sink-1");
    let oid_1 = create_output(api, &pid, "rtmp-churn-1", &sink_url_1, "source").await?;

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!("rtmp://127.0.0.1:{}/live/fault-egress-churn", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    // 1. Start output 1 and wait for media frames
    start_output(api, &pid, &oid_1).await?;
    let output_1_started_frames = sink_metrics.video_count.load(Ordering::Relaxed);
    let _ = wait_for_sink_video_above(&sink_metrics, output_1_started_frames + 9, timeout).await;

    // 2. Add and start output 2 mid-stream while output 1 is running
    let sink_url_2 = format!("rtmp://127.0.0.1:{sink_port}/live/fault-churn-sink-2");
    let oid_2 = create_output(api, &pid, "rtmp-churn-2", &sink_url_2, "source").await?;
    start_output(api, &pid, &oid_2).await?;
    let output_2_started_frames = sink_metrics.video_count.load(Ordering::Relaxed);
    let saw_output_2_data =
        wait_for_sink_video_above(&sink_metrics, output_2_started_frames + 9, timeout).await;

    // 3. Stop and delete output 1 mid-stream while output 2 continues running
    let _ = api
        .post_null(&format!("/api/v1/pipelines/{pid}/outputs/{oid_1}/stop"))
        .await;
    let _ = api
        .delete_json(&format!("/api/v1/pipelines/{pid}/outputs/{oid_1}"))
        .await;

    // 4. Verify output 2 continues making progress after output 1 teardown
    let post_churn_started_frames = sink_metrics.video_count.load(Ordering::Relaxed);
    let saw_post_churn_data =
        wait_for_sink_video_above(&sink_metrics, post_churn_started_frames + 9, timeout).await;

    // 5. Clean up output 2 and server
    let _ = api
        .post_null(&format!("/api/v1/pipelines/{pid}/outputs/{oid_2}/stop"))
        .await;
    let _ = api
        .delete_json(&format!("/api/v1/pipelines/{pid}/outputs/{oid_2}"))
        .await;
    stop_generalized_sink_server(sink_server);
    stop_child(&mut pub_child).await;

    let passed = saw_output_2_data && saw_post_churn_data;
    println!(
        "[fault] RTMP egress mid-stream output churn: {}",
        if passed { "PASS" } else { "FAIL" }
    );

    Ok(json!({
        "test": "rtmp-egress-output-churn",
        "passed": passed,
        "sawOutput2Data": saw_output_2_data,
        "sawPostChurnData": saw_post_churn_data,
    }))
}

pub(super) async fn fault_srt_egress_sink_disappear(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    timeout: Duration,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-egress-srt").await?;

    let sink_pid = create_pipeline(api, "srt-sink-target").await?;

    let sink_url = harness_srt_output_url(ports.srt, "srt-sink-target", HarnessSrtMode::Publish);
    let oid = create_output(api, &pid, "srt-sink", &sink_url, "source").await?;

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &harness_srt_ffmpeg_url(ports.srt, "fault-egress-srt", HarnessSrtMode::Publish, None),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    start_output(api, &pid, &oid).await?;

    let deadline = Instant::now() + timeout;
    let mut sink_live = false;
    while Instant::now() < deadline {
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            let status = health["pipelines"][&sink_pid]["input"]["status"]
                .as_str()
                .unwrap_or("off");
            if status == "on" {
                sink_live = true;
                break;
            }
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    if sink_live {
        println!("[fault] SRT egress delivering to sink pipeline");
    }

    let delete_url = format!("{}/api/v1/pipelines/{sink_pid}", api.base_url);
    let mut request = api.client.delete(&delete_url);
    if let Some(cookie) = &api.cookie {
        request = request.header(reqwest::header::COOKIE, cookie);
    }
    let _ = request.send().await;

    let started = Instant::now();
    let retry =
        wait_for_output_retry_or_cleanup_observation(api, &pid, &oid, Duration::from_secs(10))
            .await;
    let elapsed = started.elapsed();
    let final_output = observe_final_output(api, &pid, &oid).await;
    let retry_phase_ok = output_retry_or_cleanup_phase_ok(&retry);
    println!(
        "[fault] SRT egress sink disappear: {} (phase={}, hasError={}, sawRetrying={}, healthSawRetrying={}, finalRetrying={}, {:.1}s)",
        if retry_phase_ok && retry.status_visible && retry.health_visible && final_output.retrying {
            "PASS"
        } else {
            "FAIL"
        },
        retry.phase,
        retry.has_error,
        retry.status_visible,
        retry.health_visible,
        final_output.retrying,
        elapsed.as_secs_f64()
    );

    stop_child(&mut pub_child).await;

    Ok(json!({
        "test": "srt-egress-sink-disappear",
        "passed": retry_phase_ok && retry.status_visible && retry.health_visible && final_output.retrying,
        "phase": retry.phase,
        "hasError": retry.has_error,
        "elapsedMs": elapsed.as_millis(),
        "sawRetrying": retry.status_visible,
        "healthSawRetrying": retry.health_visible,
        "retryAttempts": retry.attempts,
        "retryBackoffMs": retry.backoff_ms,
        "finalRetrying": final_output.retrying,
    }))
}

pub(super) async fn fault_rtmp_egress_sink_stalls(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-egress-rtmp-stall").await?;

    let oid = create_output(
        api,
        &pid,
        "rtmp-stall-sink",
        &format!("rtmp://127.0.0.1:{sink_port}/live/fault-egress-rtmp-stall-sink"),
        "source",
    )
    .await?;

    let stall_server = start_stalled_rtmp_sink_server(sink_port).await?;
    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!(
            "rtmp://127.0.0.1:{}/live/fault-egress-rtmp-stall",
            ports.rtmp
        ),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    start_output(api, &pid, &oid).await?;

    let accept_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < accept_deadline && !stall_server.publish_accepted.load(Ordering::Relaxed)
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }

    let accepted = stall_server.publish_accepted.load(Ordering::Relaxed);
    let stalled_result =
        wait_for_output_stalled_status(api, &pid, &oid, Duration::from_secs(45)).await;
    let (status_snapshot, health_snapshot) = stalled_result
        .as_ref()
        .map(|(status, health)| (status.clone(), health.clone()))
        .unwrap_or((Value::Null, Value::Null));
    let passed = accepted && stalled_result.is_ok();

    println!(
        "[fault] RTMP egress sink stalls: {} (publishAccepted={} status={} phase={} targetAddr={} totalSize={} lastProgressAgeMs={})",
        if passed { "PASS" } else { "FAIL" },
        accepted,
        status_snapshot["status"].as_str().unwrap_or("unknown"),
        status_snapshot["phase"].as_str().unwrap_or("unknown"),
        status_snapshot["targetAddr"].as_str().unwrap_or(""),
        status_snapshot["totalSize"].as_u64().unwrap_or(0),
        status_snapshot["lastProgressAgeMs"]
            .as_u64()
            .map(|age| age.to_string())
            .unwrap_or_else(|| "null".to_string()),
    );

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
    stop_child(&mut pub_child).await;
    stop_stalled_rtmp_sink_server(stall_server);

    Ok(json!({
        "test": "rtmp-egress-sink-stalls",
        "passed": passed,
        "publishAccepted": accepted,
        "status": status_snapshot,
        "healthOutput": health_snapshot,
        "error": stalled_result.err(),
    }))
}

async fn wait_for_outputs_live_and_progressing(
    api: &RampApi,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let mut stabilized = Vec::new();
    let mut attempts = 0u32;
    let mut latest = Value::Null;

    while Instant::now() < deadline {
        attempts = attempts.saturating_add(1);
        let health = api.get_json("/api/v1/engine/health").await?;
        let mut snapshots = Vec::with_capacity(output_ids.len());
        let mut all_live = true;

        for output_id in output_ids {
            let output = health["pipelines"][pipeline_id]["outputs"][output_id].clone();
            let status = ApiOutputStatus::from_value(output_id, &output)?;
            let healthy = status.status == "running"
                && matches!(status.phase.as_str(), "sending" | "uploading")
                && status.raw_status == "running"
                && status.bytes_out > 0
                && status.total_size > 0
                && !status.retrying
                && status.failure_phase_is_empty()
                && status.last_error_is_empty()
                && status.last_progress_age_ms.is_some_and(|age| age <= 5_000);
            if !healthy {
                all_live = false;
            }
            snapshots.push(json!({
                "outputId": status.output_id,
                "status": status.status,
                "phase": status.phase,
                "rawStatus": status.raw_status,
                "bytesOut": status.bytes_out,
                "totalSize": status.total_size,
                "lastProgressAgeMs": status.last_progress_age_ms,
                "retrying": status.retrying,
                "failurePhase": status.failure_phase,
                "lastError": status.last_error,
                "healthy": healthy,
            }));
        }

        latest = json!({
            "attempt": attempts,
            "outputs": snapshots,
        });

        if all_live {
            stabilized.push(latest.clone());
            if stabilized.len() >= 2 {
                return Ok(json!({
                    "attempts": attempts,
                    "stabilizedSamples": stabilized,
                }));
            }
        } else {
            stabilized.clear();
        }

        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    Err(format!(
        "{} output(s) for pipeline {pipeline_id} did not stay live/progressing within {}s; latest={latest}",
        output_ids.len(),
        timeout.as_secs()
    ))
}

async fn fault_rtmp_stalled_sink_isolation_under_many_outputs(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    stall_sink_port: u16,
    healthy_sink_base_port: u16,
    sibling_outputs: usize,
    timeout: Duration,
) -> Result<Value, String> {
    let sibling_outputs = sibling_outputs.max(1);
    let pid = create_pipeline(api, "fault-egress-rtmp-stall-isolation").await?;

    let stalled_oid = create_output(
        api,
        &pid,
        "rtmp-stall-sink-isolation",
        &format!(
            "rtmp://127.0.0.1:{stall_sink_port}/live/fault-egress-rtmp-stall-isolation-stalled"
        ),
        "source",
    )
    .await?;

    let mut healthy_servers = Vec::with_capacity(sibling_outputs);
    let mut healthy_output_ids = Vec::with_capacity(sibling_outputs);
    let mut healthy_metrics = Vec::with_capacity(sibling_outputs);
    for index in 0..sibling_outputs {
        let port = healthy_sink_base_port.saturating_add(index as u16);
        let metrics = Arc::new(GeneralizedSinkMetrics::default());
        let server = start_generalized_sink_server(port, metrics.clone()).await?;
        let oid = create_output(
            api,
            &pid,
            &format!("rtmp-healthy-sink-{index:02}"),
            &format!(
                "rtmp://127.0.0.1:{port}/live/fault-egress-rtmp-stall-isolation-healthy-{index:02}"
            ),
            "source",
        )
        .await?;
        healthy_output_ids.push(oid);
        healthy_metrics.push(metrics);
        healthy_servers.push(server);
    }

    let stall_server = start_stalled_rtmp_sink_server(stall_sink_port).await?;
    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!(
            "rtmp://127.0.0.1:{}/live/fault-egress-rtmp-stall-isolation",
            ports.rtmp
        ),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    start_output(api, &pid, &stalled_oid).await?;
    for output_id in &healthy_output_ids {
        start_output(api, &pid, output_id).await?;
    }

    let stalled_accept_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < stalled_accept_deadline
        && !stall_server.publish_accepted.load(Ordering::Relaxed)
    {
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let stalled_publish_accepted = stall_server.publish_accepted.load(Ordering::Relaxed);
    let healthy_accept_deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < healthy_accept_deadline {
        let accepted = healthy_metrics
            .iter()
            .all(|metrics| metrics.publishing.load(Ordering::Relaxed) > 0);
        if accepted {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let healthy_publish_accepted = healthy_metrics
        .iter()
        .all(|metrics| metrics.publishing.load(Ordering::Relaxed) > 0);

    let healthy_progress_result = wait_for_outputs_live_and_progressing(
        api,
        &pid,
        &healthy_output_ids,
        Duration::from_secs(25),
    )
    .await;
    let stalled_result =
        wait_for_output_stalled_status(api, &pid, &stalled_oid, Duration::from_secs(45)).await;

    let healthy_snapshots = healthy_progress_result.as_ref().ok().cloned();
    let stalled_snapshots = stalled_result
        .as_ref()
        .map(|(status, health)| json!({ "status": status, "health": health }))
        .ok();

    let mut healthy_metric_summaries = Vec::with_capacity(healthy_metrics.len());
    for (index, metrics) in healthy_metrics.iter().enumerate() {
        healthy_metric_summaries.push(json!({
            "index": index,
            "publishing": metrics.publishing.load(Ordering::Relaxed),
            "videoCount": metrics.video_count.load(Ordering::Relaxed),
            "audioCount": metrics.audio_count.load(Ordering::Relaxed),
            "bytes": metrics.bytes.load(Ordering::Relaxed),
        }));
    }

    let passed = stalled_publish_accepted
        && healthy_publish_accepted
        && healthy_progress_result.is_ok()
        && stalled_result.is_ok();

    println!(
        "[fault] RTMP stalled sink isolation under sibling load: {} (siblings={} stalledAccepted={} healthyAccepted={} healthyProgress={} stalledVisible={})",
        if passed { "PASS" } else { "FAIL" },
        sibling_outputs,
        stalled_publish_accepted,
        healthy_publish_accepted,
        healthy_progress_result.is_ok(),
        stalled_result.is_ok(),
    );

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&stalled_oid)).await;
    stop_mixed_outputs(api, &pid, &healthy_output_ids).await;
    stop_child(&mut pub_child).await;
    stop_stalled_rtmp_sink_server(stall_server);
    for server in healthy_servers {
        stop_generalized_sink_server(server);
    }

    Ok(json!({
        "test": "rtmp-stalled-sink-isolation-under-many-outputs",
        "passed": passed,
        "siblingOutputs": sibling_outputs,
        "stalledOutputId": stalled_oid,
        "healthyOutputIds": healthy_output_ids,
        "stalledPublishAccepted": stalled_publish_accepted,
        "healthyPublishAccepted": healthy_publish_accepted,
        "healthyProgress": healthy_snapshots,
        "stalledSnapshot": stalled_snapshots,
        "healthySinkMetrics": healthy_metric_summaries,
        "healthyProgressError": healthy_progress_result.err(),
        "stalledError": stalled_result.err(),
    }))
}

/// Pure-fabric SRT bad-neighbor isolation at scale: one SRT destination
/// disappears mid-stream (its target pipeline is deleted) while N healthy
/// SRT siblings, fed from the same source pipeline and sharing the same
/// local SRT muxer port, keep progressing. Unlike
/// `fault_rtmp_stalled_sink_isolation_under_many_outputs`, this drives a
/// dead destination rather than a connected-but-non-reading one — the
/// harness has no raw SRT listener to hold a connection open without
/// reading, but a dead destination is an equally valid bad-neighbor shape
/// (one of the Phase 0 baseline manifest rows) and, like a stalled peer,
/// exercises the same isolation property: one leaf's failure must not slow
/// or stop its shard siblings.
async fn fault_srt_egress_dead_sink_isolation_under_many_outputs(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sibling_outputs: usize,
    timeout: Duration,
) -> Result<Value, String> {
    let sibling_outputs = sibling_outputs.max(1);
    let pid = create_pipeline(api, "fault-egress-srt-isolation").await?;

    let bad_sink_name = "srt-isolation-bad-sink".to_string();
    let bad_sink_pid = create_pipeline(api, &bad_sink_name).await?;
    let bad_oid = create_output(
        api,
        &pid,
        "srt-isolation-bad",
        &harness_srt_output_url(ports.srt, &bad_sink_name, HarnessSrtMode::Publish),
        "source",
    )
    .await?;

    let mut healthy_sink_pids = Vec::with_capacity(sibling_outputs);
    let mut healthy_output_ids = Vec::with_capacity(sibling_outputs);
    for index in 0..sibling_outputs {
        let sink_name = format!("srt-isolation-healthy-sink-{index:02}");
        let sink_pid = create_pipeline(api, &sink_name).await?;
        let oid = create_output(
            api,
            &pid,
            &format!("srt-isolation-healthy-{index:02}"),
            &harness_srt_output_url(ports.srt, &sink_name, HarnessSrtMode::Publish),
            "source",
        )
        .await?;
        healthy_sink_pids.push(sink_pid);
        healthy_output_ids.push(oid);
    }

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &harness_srt_ffmpeg_url(
            ports.srt,
            "fault-egress-srt-isolation",
            HarnessSrtMode::Publish,
            None,
        ),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;

    start_output(api, &pid, &bad_oid).await?;
    for output_id in &healthy_output_ids {
        start_output(api, &pid, output_id).await?;
    }

    let accept_deadline = Instant::now() + Duration::from_secs(15);
    let mut bad_sink_live = false;
    let mut healthy_sinks_live = false;
    while Instant::now() < accept_deadline && !(bad_sink_live && healthy_sinks_live) {
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            bad_sink_live =
                health["pipelines"][&bad_sink_pid]["input"]["status"].as_str() == Some("on");
            healthy_sinks_live = healthy_sink_pids.iter().all(|sink_pid| {
                health["pipelines"][sink_pid]["input"]["status"].as_str() == Some("on")
            });
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    delete_pipeline_v1(api, &bad_sink_pid).await?;

    let started = Instant::now();
    let retry =
        wait_for_output_retry_or_cleanup_observation(api, &pid, &bad_oid, Duration::from_secs(10))
            .await;
    let elapsed = started.elapsed();

    let healthy_progress_result = wait_for_outputs_live_and_progressing(
        api,
        &pid,
        &healthy_output_ids,
        Duration::from_secs(25),
    )
    .await;

    let final_bad_output = observe_final_output(api, &pid, &bad_oid).await;
    let retry_phase_ok = output_retry_or_cleanup_phase_ok(&retry);

    let passed =
        bad_sink_live && healthy_sinks_live && retry_phase_ok && healthy_progress_result.is_ok();

    println!(
        "[fault] SRT dead sink isolation under sibling load: {} (siblings={} badSinkLive={} healthySinksLive={} retryPhaseOk={} healthyProgress={} {:.1}s)",
        if passed { "PASS" } else { "FAIL" },
        sibling_outputs,
        bad_sink_live,
        healthy_sinks_live,
        retry_phase_ok,
        healthy_progress_result.is_ok(),
        elapsed.as_secs_f64(),
    );

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&bad_oid)).await;
    stop_mixed_outputs(api, &pid, &healthy_output_ids).await;
    stop_child(&mut pub_child).await;

    Ok(json!({
        "test": "srt-egress-dead-sink-isolation-under-many-outputs",
        "passed": passed,
        "siblingOutputs": sibling_outputs,
        "badOutputId": bad_oid,
        "healthyOutputIds": healthy_output_ids,
        "badSinkLive": bad_sink_live,
        "healthySinksLive": healthy_sinks_live,
        "retryPhaseOk": retry_phase_ok,
        "elapsedMs": elapsed.as_millis(),
        "healthyProgress": healthy_progress_result.as_ref().ok().cloned(),
        "finalBadOutput": final_bad_output.status,
        "healthyProgressError": healthy_progress_result.err(),
    }))
}

pub(crate) async fn fault_egress_retry() -> Result<Value, String> {
    let work_dir = artifact_path("fault.egress-retry");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let retry_limit_db_path = work_dir.join("retry-limit.sqlite");
    let retry_limit_log_path = work_dir.join("retry-limit.log");
    let sink_port = harness_port_defaults().sink;
    let ports = TestPorts::from_env();
    let timeout = Duration::from_secs(15);

    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture_h264 = checked_h264_fixture()?;
    let results = vec![
        fault_rtmp_egress_sink_disappear(&api, &ports, &fixture_h264, sink_port, timeout).await?,
        fault_srt_egress_sink_disappear(&api, &ports, &fixture_h264, timeout).await?,
    ];

    stop_child(&mut child).await;

    let retry_limit_env = [
        ("RESTREAM_OUTPUT_MAX_RETRIES", "2".to_string()),
        ("RESTREAM_OUTPUT_RETRY_BASE_MS", "200".to_string()),
        ("RESTREAM_OUTPUT_RETRY_MAX_MS", "400".to_string()),
        ("RESTREAM_RECONCILER_INTERVAL_MS", "100".to_string()),
        ("RESTREAM_SRT_CONNECT_TIMEOUT_MS", "500".to_string()),
    ];
    let mut retry_limit_child = start_restream_child_with_env(
        &restream_bin,
        &ports,
        &retry_limit_db_path,
        &retry_limit_log_path,
        &retry_limit_env,
    )
    .await?;
    let retry_limit_api = login_api(&ports).await?;
    let mut retry_limit_results = Vec::new();
    for case in retry_budget_cases() {
        let workflow_result = run_retry_budget_case_via_workflow(
            &retry_limit_api,
            &ports,
            &fixture_h264,
            sink_port,
            case,
        )
        .await?;
        retry_limit_results.push(workflow_result);
    }
    stop_child(&mut retry_limit_child).await;

    let mut results = results;
    results.extend(retry_limit_results);

    let all_passed = results.iter().all(|r| r["passed"] == true);
    let result = json!({
        "mode": "fault.egress-retry",
        "passed": all_passed,
        "tests": results,
    });

    let result_path = work_dir.join("fault.egress-retry.json");
    std::fs::write(&result_path, serde_json::to_string_pretty(&result).unwrap())
        .map_err(|e| e.to_string())?;
    println!("artifact={}", result_path.display());

    if !all_passed {
        return Err("fault.egress-retry: not all tests passed".to_string());
    }
    Ok(result)
}
pub(crate) async fn fault_output_stall() -> Result<Value, String> {
    let work_dir = artifact_path("fault.output-stall");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let sink_port = harness_port_defaults().sink;
    let ports = TestPorts::from_env();
    let timeout = Duration::from_secs(15);

    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture_h264 = checked_h264_fixture()?;
    let stall_single =
        fault_rtmp_egress_sink_stalls(&api, &ports, &fixture_h264, sink_port, timeout).await?;
    let sibling_outputs = fault_output_stall_sibling_count();
    let isolation = fault_rtmp_stalled_sink_isolation_under_many_outputs(
        &api,
        &ports,
        &fixture_h264,
        sink_port.saturating_add(10),
        sink_port.saturating_add(100),
        sibling_outputs,
        timeout,
    )
    .await?;
    let srt_isolation = fault_srt_egress_dead_sink_isolation_under_many_outputs(
        &api,
        &ports,
        &fixture_h264,
        sibling_outputs,
        timeout,
    )
    .await?;

    stop_child(&mut child).await;

    let tests = vec![stall_single, isolation, srt_isolation];
    let passed = tests
        .iter()
        .all(|result| result["passed"].as_bool().unwrap_or(false));
    let payload = json!({
        "mode": "fault.output-stall",
        "passed": passed,
        "siblingOutputs": sibling_outputs,
        "tests": tests,
    });

    let result_path = work_dir.join("fault.output-stall.json");
    std::fs::write(
        &result_path,
        serde_json::to_string_pretty(&payload).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    println!("artifact={}", result_path.display());

    if !passed {
        return Err("fault.output-stall: not all tests passed".to_string());
    }
    Ok(payload)
}

/// Output retry state observed from both the public status endpoint and engine health.
pub(crate) struct OutputRetryObservation {
    pub(crate) status_visible: bool,
    pub(crate) health_visible: bool,
    pub(crate) has_error: bool,
    pub(crate) cleaned_up: bool,
    pub(crate) phase: String,
    pub(crate) failure_phase: String,
    pub(crate) last_error: String,
    pub(crate) attempts: Option<u64>,
    pub(crate) backoff_ms: Option<u64>,
}

impl Default for OutputRetryObservation {
    fn default() -> Self {
        Self {
            status_visible: false,
            health_visible: false,
            has_error: false,
            cleaned_up: false,
            phase: String::from("unknown"),
            failure_phase: String::from("unknown"),
            last_error: String::new(),
            attempts: None,
            backoff_ms: None,
        }
    }
}

pub(crate) async fn wait_for_output_retry_observation(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> OutputRetryObservation {
    let deadline = Instant::now() + timeout;
    let mut observation = OutputRetryObservation::default();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok((status, _)) = api.get_output_status(pipeline_id, output_id).await {
            observation.status_visible = status.status == "retrying" && status.retrying;
            observation.phase = status.phase;
            observation.failure_phase = status
                .failure_phase
                .unwrap_or_else(|| "unknown".to_string());
            observation.last_error = status.last_error.unwrap_or_default();
            observation.has_error = !observation.last_error.is_empty();
            if observation.status_visible {
                observation.attempts = status.retry_attempts;
                observation.backoff_ms = status.retry_backoff_ms;
            }
        }
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            let output = &health["pipelines"][pipeline_id]["outputs"][output_id];
            observation.health_visible = output["status"].as_str() == Some("retrying")
                && output["retrying"].as_bool() == Some(true);
        }
        if observation.status_visible && observation.health_visible && observation.has_error {
            break;
        }
    }
    observation
}

async fn wait_for_output_retry_or_cleanup_observation(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> OutputRetryObservation {
    let deadline = Instant::now() + timeout;
    let mut observation = OutputRetryObservation::default();
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        match api.get_output_status(pipeline_id, output_id).await {
            Err(_) => {
                observation.cleaned_up = true;
                observation.phase = "cleaned-up".to_string();
                break;
            }
            Ok((status, _)) => {
                observation.status_visible = status.status == "retrying";
                observation.phase = status.phase;
                observation.last_error = status.last_error.unwrap_or_default();
                observation.has_error = !observation.last_error.is_empty();
                if observation.status_visible {
                    observation.attempts = status.retry_attempts;
                    observation.backoff_ms = status.retry_backoff_ms;
                }
            }
        }
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            observation.health_visible =
                health["pipelines"][pipeline_id]["outputs"][output_id]["status"].as_str()
                    == Some("retrying");
        }
        if observation.status_visible && observation.has_error {
            break;
        }
    }
    observation
}

pub(crate) fn output_retry_or_cleanup_phase_ok(observation: &OutputRetryObservation) -> bool {
    observation.cleaned_up || (observation.status_visible && observation.has_error)
}
