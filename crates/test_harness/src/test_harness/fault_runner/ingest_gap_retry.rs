//! An output retrying a lost sink while its ingest also drops out briefly:
//! retry and disconnect grace both stay visible, and delivery resumes.

use super::super::*;

/// Started pipeline, output and publisher for the ingest-gap case.
struct IngestGapSetup {
    pid: String,
    oid: String,
    pub_child: Child,
    baseline_video: u64,
}

/// Start the pipeline, its output and publisher, then drop the sink.
async fn start_ingest_gap(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
) -> Result<IngestGapSetup, String> {
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

    let pub_child = spawn_publisher(
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

    Ok(IngestGapSetup {
        pid,
        oid,
        pub_child,
        baseline_video,
    })
}

/// What the gap showed: input-off result, retry visibility and grace state.
struct IngestGapObservation {
    input_off: Result<(), String>,
    output_retrying: bool,
    health_retrying: bool,
    disconnect_visible: bool,
    grace_active: bool,
    grace_remaining: bool,
    input: Value,
}

/// Drop the publisher inside the grace and read the gap state.
async fn observe_ingest_gap(
    api: &RampApi,
    pid: &str,
    oid: &str,
    pub_child: &mut Child,
) -> IngestGapObservation {
    stop_child(pub_child).await;
    let input_off = wait_for_api_input_off(api, pid, Duration::from_secs(10)).await;
    tokio::time::sleep(Duration::from_millis(1_500)).await;

    let gap_status = api.get_output_status(pid, oid).await.ok();
    let gap_health = api.get_json("/api/v1/engine/health").await.ok();
    let gap_output_retrying = gap_status
        .as_ref()
        .map(|(status, _)| status.status == "retrying" && status.retrying)
        .unwrap_or(false);
    let gap_health_retrying = gap_health
        .as_ref()
        .map(|health| {
            let output = &health["pipelines"][pid]["outputs"][oid];
            output["status"].as_str() == Some("retrying")
                && output["retrying"].as_bool() == Some(true)
        })
        .unwrap_or(false);
    let gap_input = gap_health
        .as_ref()
        .map(|health| health["pipelines"][pid]["input"].clone())
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
    IngestGapObservation {
        input_off,
        output_retrying: gap_output_retrying,
        health_retrying: gap_health_retrying,
        disconnect_visible: gap_disconnect_visible,
        grace_active: gap_grace_active,
        grace_remaining: gap_grace_remaining,
        input: gap_input,
    }
}

/// Restored sink and publisher, plus whether delivery resumed.
struct IngestGapRecovery {
    sink_server: GeneralizedSinkServer,
    resumed_child: Child,
    media_ready: Result<Value, String>,
    recovered: bool,
    recovery_status: String,
}

/// Restore the sink and the publisher and wait for delivery to resume.
async fn recover_from_ingest_gap(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    pid: &str,
    oid: &str,
    sink_port: u16,
) -> Result<IngestGapRecovery, String> {
    let recovery_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let recovery_server =
        start_generalized_sink_server(sink_port, recovery_metrics.clone()).await?;

    let resumed_child = spawn_publisher(
        fixture_h264,
        &format!("rtmp://127.0.0.1:{}/live/fault-rtmp-retry-gap", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    let media_ready = wait_for_api_input_media_ready(api, pid, Duration::from_secs(30)).await;

    let recovery_deadline = Instant::now() + Duration::from_secs(25);
    let mut recovered = false;
    let mut recovery_status = String::from("unknown");
    while Instant::now() < recovery_deadline {
        tokio::time::sleep(Duration::from_millis(500)).await;
        if let Ok((status, _)) = api.get_output_status(pid, oid).await {
            recovery_status = status.status;
        }
        if recovery_metrics.video_count.load(Ordering::Relaxed) >= 10 {
            recovered = true;
            break;
        }
    }
    Ok(IngestGapRecovery {
        sink_server: recovery_server,
        resumed_child,
        media_ready,
        recovered,
        recovery_status,
    })
}

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
    let mut setup = start_ingest_gap(api, ports, fixture_h264, sink_port, timeout).await?;

    let retry =
        wait_for_output_retry_observation(api, &setup.pid, &setup.oid, Duration::from_secs(10))
            .await;

    let gap = observe_ingest_gap(api, &setup.pid, &setup.oid, &mut setup.pub_child).await;

    let mut recovery =
        recover_from_ingest_gap(api, ports, fixture_h264, &setup.pid, &setup.oid, sink_port)
            .await?;

    let final_status = api.get_output_status(&setup.pid, &setup.oid).await.ok();
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
        .map(|health| health["pipelines"][&setup.pid]["input"].clone())
        .unwrap_or(Value::Null);
    let final_disconnect_cleared = final_input["status"] == "on"
        && final_input["probeStatus"] == "ready"
        && final_input["lastSessionProtocol"].is_null()
        && final_input["lastDisconnectReason"].is_null()
        && final_input["lastFailurePhase"].is_null()
        && final_input["recentDisconnectError"] == false;
    let passed = setup.baseline_video >= RECOVERY_WARM_VIDEO_MIN
        && retry.status_visible
        && retry.health_visible
        && retry.has_error
        && gap.input_off.is_ok()
        && gap.output_retrying
        && gap.health_retrying
        && gap.disconnect_visible
        && gap.grace_active
        && gap.grace_remaining
        && recovery.media_ready.is_ok()
        && recovery.recovered
        && final_status_running
        && !final_retrying
        && final_disconnect_cleared;
    println!(
        "[fault] Egress retry survives transient ingest gap: {} (retrying={} healthRetrying={} gapRetrying={} gapHealthRetrying={} recovered={} recoveryStatus={} finalRetrying={} disconnectCleared={})",
        if passed { "PASS" } else { "FAIL" },
        retry.status_visible,
        retry.health_visible,
        gap.output_retrying,
        gap.health_retrying,
        recovery.recovered,
        recovery.recovery_status,
        final_retrying,
        final_disconnect_cleared,
    );
    let result = json!({
        "test": "egress-retry-survives-transient-ingest-gap",
        "passed": passed,
        "baselineVideo": setup.baseline_video,
        "retryPhase": retry.phase,
        "retryHasError": retry.has_error,
        "retryStatusVisible": retry.status_visible,
        "retryHealthVisible": retry.health_visible,
        "retryAttempts": retry.attempts,
        "retryBackoffMs": retry.backoff_ms,
        "inputOffError": gap.input_off.as_ref().err(),
        "gapOutputRetrying": gap.output_retrying,
        "gapHealthRetrying": gap.health_retrying,
        "gapDisconnectVisible": gap.disconnect_visible,
        "gapGraceActive": gap.grace_active,
        "gapGraceRemainingBounded": gap.grace_remaining,
        "gapInputSnapshot": gap.input,
        "mediaReady": recovery.media_ready.is_ok(),
        "mediaReadyError": recovery.media_ready.as_ref().err(),
        "recovered": recovery.recovered,
        "recoveryStatus": recovery.recovery_status,
        "finalStatusRunning": final_status_running,
        "finalRetrying": final_retrying,
        "finalDisconnectCleared": final_disconnect_cleared,
        "finalInputSnapshot": final_input,
    });

    stop_generalized_sink_server(recovery.sink_server);

    stop_mixed_outputs(api, &setup.pid, std::slice::from_ref(&setup.oid)).await;
    stop_child(&mut recovery.resumed_child).await;

    Ok(result)
}
