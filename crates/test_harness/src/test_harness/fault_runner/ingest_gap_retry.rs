//! An output retrying a lost sink while its ingest also drops out briefly:
//! retry and disconnect grace both stay visible, and delivery resumes.

use super::super::*;

/// Stops the RTMP sink, then the publisher, inside the disconnect grace.
/// Expects retrying output and the RTMP disconnect to surface in status and
/// health during the gap, then the output to recover once both return.
pub(crate) async fn egress_retry_survives_ingest_gap(
    api: &mut RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-rtmp-retry-gap").await?;

    let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server = start_generalized_sink_server(sink_port, sink_metrics.clone()).await?;

    let oid = create_output(
        api,
        &pid,
        "rtmp-retry-gap-sink",
        &format!("rtmp://127.0.0.1:{sink_port}/live/fault-rtmp-retry-gap-sink"),
        "source",
    )
    .await?;

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!("rtmp://127.0.0.1:{}/live/fault-rtmp-retry-gap", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;
    start_output(api, &pid, &oid).await?;

    let _ = wait_for_sink_video_above(
        &sink_metrics,
        RECOVERY_WARM_VIDEO_MIN - 1,
        Duration::from_secs(15),
    )
    .await;
    let baseline_video = sink_metrics.video_count.load(Ordering::Relaxed);

    stop_generalized_sink_server(sink_server);

    let retry = wait_for_output_retry_observation(api, &pid, &oid, Duration::from_secs(10)).await;

    stop_child(&mut pub_child).await;
    let input_off = wait_for_api_input_off(api, &pid, Duration::from_secs(10)).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    let gap_status = api.get_output_status(&pid, &oid).await.ok();
    let gap_health = api.get_json("/api/v1/engine/health").await.ok();
    let gap_output_retrying = gap_status
        .as_ref()
        .map(|(status, _)| status.status == "retrying" && status.retrying)
        .unwrap_or(false);
    let gap_health_retrying = gap_health
        .as_ref()
        .map(|health| {
            let output = &health["pipelines"][&pid]["outputs"][&oid];
            output["status"].as_str() == Some("retrying")
                && output["retrying"].as_bool() == Some(true)
        })
        .unwrap_or(false);
    let gap_input = gap_health
        .as_ref()
        .map(|health| health["pipelines"][&pid]["input"].clone())
        .unwrap_or(Value::Null);
    let gap_disconnect_visible = gap_input["status"] == "off"
        && gap_input["lastSessionProtocol"] == "rtmp"
        && gap_input["lastDisconnectReason"] == "publisher disconnected"
        && gap_input["lastFailurePhase"] == "disconnect"
        && gap_input["recentDisconnectError"] == false;
    let gap_grace_active = gap_input["disconnectGraceActive"] == true;
    let gap_grace_remaining = gap_input["disconnectGraceRemainingMs"]
        .as_u64()
        .is_some_and(|remaining| remaining > 0 && remaining <= 5_000);

    let recovery_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let recovery_server =
        start_generalized_sink_server(sink_port, recovery_metrics.clone()).await?;

    let mut resumed_child = spawn_publisher(
        fixture_h264,
        &format!("rtmp://127.0.0.1:{}/live/fault-rtmp-retry-gap", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    let media_ready = wait_for_api_input_media_ready(api, &pid, Duration::from_secs(30)).await;

    let recovery_deadline = Instant::now() + Duration::from_secs(25);
    let mut recovered = false;
    let mut recovery_status = String::from("unknown");
    while Instant::now() < recovery_deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok((status, _)) = api.get_output_status(&pid, &oid).await {
            recovery_status = status.status;
        }
        if recovery_metrics.video_count.load(Ordering::Relaxed) >= 10 {
            recovered = true;
            break;
        }
    }

    let final_status = api.get_output_status(&pid, &oid).await.ok();
    let final_status_running = final_status
        .as_ref()
        .map(|(status, _)| status.status.as_str())
        == Some("running");
    let final_retrying = final_status
        .as_ref()
        .is_some_and(|(status, _)| status.retrying);
    let final_health = api.get_json("/api/v1/engine/health").await.ok();
    let final_input = final_health
        .as_ref()
        .map(|health| health["pipelines"][&pid]["input"].clone())
        .unwrap_or(Value::Null);
    let final_disconnect_cleared = final_input["status"] == "on"
        && final_input["probeStatus"] == "ready"
        && final_input["lastSessionProtocol"].is_null()
        && final_input["lastDisconnectReason"].is_null()
        && final_input["lastFailurePhase"].is_null()
        && final_input["recentDisconnectError"] == false;
    let passed = baseline_video >= RECOVERY_WARM_VIDEO_MIN
        && retry.status_visible
        && retry.health_visible
        && retry.has_error
        && input_off.is_ok()
        && gap_output_retrying
        && gap_health_retrying
        && gap_disconnect_visible
        && gap_grace_active
        && gap_grace_remaining
        && media_ready.is_ok()
        && recovered
        && final_status_running
        && !final_retrying
        && final_disconnect_cleared;
    println!(
        "[fault] Egress retry survives transient ingest gap: {} (retrying={} healthRetrying={} gapRetrying={} gapHealthRetrying={} recovered={} recoveryStatus={} finalRetrying={} disconnectCleared={})",
        if passed { "PASS" } else { "FAIL" },
        retry.status_visible,
        retry.health_visible,
        gap_output_retrying,
        gap_health_retrying,
        recovered,
        recovery_status,
        final_retrying,
        final_disconnect_cleared,
    );
    let result = json!({
        "test": "egress-retry-survives-transient-ingest-gap",
        "passed": passed,
        "baselineVideo": baseline_video,
        "retryPhase": retry.phase,
        "retryHasError": retry.has_error,
        "retryStatusVisible": retry.status_visible,
        "retryHealthVisible": retry.health_visible,
        "retryAttempts": retry.attempts,
        "retryBackoffMs": retry.backoff_ms,
        "inputOffError": input_off.err(),
        "gapOutputRetrying": gap_output_retrying,
        "gapHealthRetrying": gap_health_retrying,
        "gapDisconnectVisible": gap_disconnect_visible,
        "gapGraceActive": gap_grace_active,
        "gapGraceRemainingBounded": gap_grace_remaining,
        "gapInputSnapshot": gap_input,
        "mediaReady": media_ready.is_ok(),
        "mediaReadyError": media_ready.err(),
        "recovered": recovered,
        "recoveryStatus": recovery_status,
        "finalStatusRunning": final_status_running,
        "finalRetrying": final_retrying,
        "finalDisconnectCleared": final_disconnect_cleared,
        "finalInputSnapshot": final_input,
    });

    stop_generalized_sink_server(recovery_server);

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
    stop_child(&mut resumed_child).await;

    Ok(result)
}
