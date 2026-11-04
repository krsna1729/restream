//! An SRT publisher replaced immediately after it stops: late cleanup of the
//! old ingest must not disturb the running egress of the pipeline.

use super::super::*;

/// Stops an SRT publisher and reconnects at once, so the old ingest's late
/// disconnect cleanup races the replacement. Expects the output to keep its
/// sink connection, stay running without retry, and progress from the new
/// publisher once its disconnect state clears.
pub(crate) async fn rapid_srt_replacement_preserves_egress(
    api: &mut RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
) -> Result<Value, String> {
    let pid = create_pipeline_with_stream_key(
        api,
        "fault-srt-replacement-race",
        "fault-srt-replacement-race",
    )
    .await?;

    let metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server = start_generalized_sink_server(sink_port, metrics.clone()).await?;

    let oid = create_output(
        api,
        &pid,
        "srt-replacement-race-sink",
        &format!("rtmp://127.0.0.1:{sink_port}/live/fault-srt-replacement-race-sink"),
        "source",
    )
    .await?;

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &harness_srt_ffmpeg_url(
            ports.srt,
            "fault-srt-replacement-race",
            HarnessSrtMode::Publish,
            None,
        ),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;
    start_output(api, &pid, &oid).await?;

    let _ = wait_for_sink_video_above(
        &metrics,
        RECOVERY_WARM_VIDEO_MIN - 1,
        Duration::from_secs(15),
    )
    .await;
    let baseline_video = metrics.video_count.load(Ordering::Relaxed);
    let baseline_connections = metrics.connections.load(Ordering::Relaxed);
    // A killed publisher stays `on` until SRT timeouts: recovery needs a new address.
    let health = api.get_json("/api/v1/engine/health").await.ok();
    let original_publisher = health.map_or(Value::Null, |health| {
        health["pipelines"][&pid]["input"]["publisher"]["remoteAddr"].clone()
    });

    stop_child(&mut pub_child).await;

    // Reconnect immediately so the old ingest's late disconnect/unregister
    // cleanup races a replacement publisher on the same pipeline.
    let replacement_url = harness_srt_ffmpeg_url(
        ports.srt,
        "fault-srt-replacement-race",
        HarnessSrtMode::Publish,
        None,
    );
    let mut replacement_child =
        Some(spawn_publisher(fixture_h264, &replacement_url, "mpegts", true).await?);
    let mut replacement_attempts = 1u64;
    let recovery_deadline = Instant::now() + Duration::from_secs(30);
    let mut saw_gap_grace = false;
    let mut saw_recent_disconnect = false;
    let mut saw_output_retrying = false;
    let mut saw_output_missing = false;
    let mut saw_output_nonrunning = false;
    let mut recovered = false;
    while Instant::now() < recovery_deadline {
        if let Some(child) = replacement_child.as_mut()
            && child.try_wait().map_err(|e| e.to_string())?.is_some()
        {
            replacement_child =
                Some(spawn_publisher(fixture_h264, &replacement_url, "mpegts", true).await?);
            replacement_attempts += 1;
        }

        let health = api.get_json("/api/v1/engine/health").await.ok();
        let input = health
            .as_ref()
            .map(|snapshot| snapshot["pipelines"][&pid]["input"].clone())
            .unwrap_or(Value::Null);
        saw_gap_grace |= input["disconnectGraceActive"] == true;
        saw_recent_disconnect |= input["recentDisconnectCount"]
            .as_u64()
            .is_some_and(|count| count >= 1);

        let status = api.get_output_status(&pid, &oid).await;
        match status {
            Ok((status, _)) => {
                saw_output_retrying |= status.retrying;
                saw_output_nonrunning |= status.status != "running";
            }
            Err(_) => {
                saw_output_missing = true;
            }
        }

        let publisher = &input["publisher"]["remoteAddr"];
        let replacement_live = publisher.is_string() && *publisher != original_publisher;
        let disconnect_cleared = replacement_live
            && input["status"] == "on"
            && input["probeStatus"] == "ready"
            && input["lastSessionProtocol"].is_null()
            && input["lastDisconnectReason"].is_null()
            && input["lastFailurePhase"].is_null()
            && input["recentDisconnectError"] == false;
        let output_progressed = metrics.video_count.load(Ordering::Relaxed) > baseline_video + 10;
        if disconnect_cleared && output_progressed {
            recovered = true;
            break;
        }

        tokio::time::sleep(Duration::from_millis(200)).await;
    }

    let final_connections = metrics.connections.load(Ordering::Relaxed);
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
    let final_recent_disconnect_count = final_input["recentDisconnectCount"].as_u64().unwrap_or(0);
    let passed = baseline_video >= RECOVERY_WARM_VIDEO_MIN
        && original_publisher.is_string()
        && baseline_connections == 1
        && recovered
        && final_connections == baseline_connections
        && final_status_running
        && !final_retrying
        && final_disconnect_cleared
        && !saw_output_retrying
        && !saw_output_missing
        && !saw_output_nonrunning;
    println!(
        "[fault] Rapid SRT publisher replacement preserves egress: {} (connections={} recovered={} attempts={} sawGapGrace={} sawRecentDisconnect={} sawRetrying={} sawMissing={} sawNonRunning={} finalRetrying={} disconnectCleared={} recentDisconnectCount={})",
        if passed { "PASS" } else { "FAIL" },
        final_connections,
        recovered,
        replacement_attempts,
        saw_gap_grace,
        saw_recent_disconnect,
        saw_output_retrying,
        saw_output_missing,
        saw_output_nonrunning,
        final_retrying,
        final_disconnect_cleared,
        final_recent_disconnect_count,
    );
    let result = json!({
        "test": "rapid-srt-replacement-preserves-egress",
        "passed": passed,
        "baselineVideo": baseline_video,
        "baselineConnections": baseline_connections,
        "originalPublisher": original_publisher,
        "recovered": recovered,
        "replacementAttempts": replacement_attempts,
        "sawGapGrace": saw_gap_grace,
        "sawRecentDisconnect": saw_recent_disconnect,
        "sawOutputRetrying": saw_output_retrying,
        "sawOutputMissing": saw_output_missing,
        "sawOutputNonRunning": saw_output_nonrunning,
        "finalConnections": final_connections,
        "finalStatusRunning": final_status_running,
        "finalRetrying": final_retrying,
        "finalDisconnectCleared": final_disconnect_cleared,
        "finalRecentDisconnectCount": final_recent_disconnect_count,
        "finalInputSnapshot": final_input,
    });

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
    if let Some(child) = replacement_child.as_mut() {
        stop_child(child).await;
    }
    stop_generalized_sink_server(sink_server);

    Ok(result)
}
