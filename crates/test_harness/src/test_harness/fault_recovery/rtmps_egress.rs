use super::super::*;
use super::egress::{
    output_retry_or_cleanup_phase_ok, wait_for_output_retry_or_cleanup_observation,
};
use super::resilience::{create_pipeline, observe_final_output, wait_for_sink_video_above};

pub(super) async fn fault_rtmps_egress_sink_disappear(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
    cert_path: &Path,
    key_path: &Path,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-egress-rtmps").await?;
    let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server =
        start_generalized_rtmps_sink_server(sink_port, cert_path, key_path, sink_metrics.clone())
            .await?;
    let sink_url = format!("rtmps://127.0.0.1:{sink_port}/live/fault-egress-rtmps-sink");
    let oid = create_output(api, &pid, "rtmps-sink", &sink_url, "source").await?;
    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!("rtmp://127.0.0.1:{}/live/fault-egress-rtmps", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;
    start_output(api, &pid, &oid).await?;

    let initial_delivery = wait_for_sink_video_above(&sink_metrics, 9, timeout).await;
    stop_generalized_sink_server(sink_server);

    let started = Instant::now();
    let retry =
        wait_for_output_retry_or_cleanup_observation(api, &pid, &oid, Duration::from_secs(10))
            .await;
    let elapsed = started.elapsed();
    let recovery_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let recovered_server = start_generalized_rtmps_sink_server(
        sink_port,
        cert_path,
        key_path,
        recovery_metrics.clone(),
    )
    .await?;

    let recovery_deadline = Instant::now() + Duration::from_secs(25);
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
    let initial_sink = sink_metrics.summary();
    let recovered_sink = recovery_metrics.summary();
    let rtmps_telemetry = api
        .get_json("/metrics/system?view=summary")
        .await
        .ok()
        .map(|system| system["rtmps"].clone())
        .unwrap_or(Value::Null);
    stop_generalized_sink_server(recovered_server);
    let retry_phase_ok = output_retry_or_cleanup_phase_ok(&retry);
    let passed = initial_delivery
        && retry_phase_ok
        && recovered
        && saw_retrying
        && retry.health_visible
        && !final_output.retrying;
    println!(
        "[fault] RTMPS egress sink disappear: {} (initialDelivery={} phase={} sawRetrying={} healthSawRetrying={} recovered={} finalRetrying={} {:.1}s)",
        if passed { "PASS" } else { "FAIL" },
        initial_delivery,
        retry.phase,
        saw_retrying,
        retry.health_visible,
        recovered,
        final_output.retrying,
        elapsed.as_secs_f64()
    );
    stop_child(&mut pub_child).await;

    Ok(json!({
        "test": "rtmps-egress-sink-disappear",
        "passed": passed,
        "initialDelivery": initial_delivery,
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
        "initialSink": initial_sink,
        "recoveredSink": recovered_sink,
        "rtmpsTelemetry": rtmps_telemetry,
    }))
}

pub(super) async fn fault_rtmps_egress_sink_stalls(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
    cert_path: &Path,
    key_path: &Path,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-egress-rtmps-stall").await?;
    let sibling_port = sink_port
        .checked_add(1)
        .ok_or("RTMPS stall sibling port overflow")?;
    let sibling_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sibling_server =
        start_generalized_sink_server(sibling_port, sibling_metrics.clone()).await?;
    let sibling_oid = create_output(
        api,
        &pid,
        "rtmp-healthy-sibling",
        &format!("rtmp://127.0.0.1:{sibling_port}/live/fault-egress-rtmps-sibling"),
        "source",
    )
    .await?;
    let oid = create_output(
        api,
        &pid,
        "rtmps-stall-sink",
        &format!("rtmps://127.0.0.1:{sink_port}/live/fault-egress-rtmps-stall-sink"),
        "source",
    )
    .await?;
    let stall_server = start_stalled_rtmps_sink_server(sink_port, cert_path, key_path).await?;
    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!(
            "rtmp://127.0.0.1:{}/live/fault-egress-rtmps-stall",
            ports.rtmp
        ),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;
    start_output(api, &pid, &oid).await?;
    start_output(api, &pid, &sibling_oid).await?;
    let sibling_initial_delivery = wait_for_sink_video_above(&sibling_metrics, 9, timeout).await;

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
    let sibling_during_stall = sibling_metrics.video_count.load(Ordering::Relaxed);
    let sibling_progress_during_stall = stalled_result.is_ok()
        && wait_for_sink_video_above(
            &sibling_metrics,
            sibling_during_stall.saturating_add(9),
            Duration::from_secs(10),
        )
        .await;
    let sibling_sink = sibling_metrics.summary();
    let rtmps_telemetry = api
        .get_json("/metrics/system?view=summary")
        .await
        .ok()
        .map(|system| system["rtmps"].clone())
        .unwrap_or(Value::Null);
    let passed = accepted
        && stalled_result.is_ok()
        && sibling_initial_delivery
        && sibling_progress_during_stall;
    println!(
        "[fault] RTMPS egress sink stalls: {} (publishAccepted={} status={} phase={} healthySiblingProgress={})",
        if passed { "PASS" } else { "FAIL" },
        accepted,
        status_snapshot["status"].as_str().unwrap_or("unknown"),
        status_snapshot["phase"].as_str().unwrap_or("unknown"),
        sibling_progress_during_stall,
    );
    let output_ids = [oid.clone(), sibling_oid.clone()];
    stop_mixed_outputs(api, &pid, &output_ids).await;
    stop_child(&mut pub_child).await;
    stop_stalled_rtmp_sink_server(stall_server);
    stop_generalized_sink_server(sibling_server);

    Ok(json!({
        "test": "rtmps-egress-sink-stalls",
        "passed": passed,
        "publishAccepted": accepted,
        "status": status_snapshot,
        "healthOutput": health_snapshot,
        "error": stalled_result.err(),
        "siblingInitialDelivery": sibling_initial_delivery,
        "siblingProgressDuringStall": sibling_progress_during_stall,
        "siblingSink": sibling_sink,
        "rtmpsTelemetry": rtmps_telemetry,
    }))
}
