//! Fault/recovery runtime, orchestration, and assertion helpers.

use super::*;

pub(super) async fn recovery_live_cases(
    api: &mut RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    hls_put_port: u16,
    timeout: Duration,
) -> Result<Vec<Value>, String> {
    let mut results = Vec::new();

    for case in recovery_transient_cases() {
        let workflow_result =
            run_recovery_transient_case_via_workflow(api, ports, fixture_h264, sink_port, case)
                .await?;
        results.push(workflow_result);
    }

    // ── 2b. Rapid SRT publisher replacement preserves egress ownership ──
    {
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
            &format!(
                "srt://127.0.0.1:{}?streamid=publish:live/fault-srt-replacement-race&pkt_size=1316",
                ports.srt
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

        stop_child(&mut pub_child).await;

        // Reconnect immediately so the old ingest's late disconnect/unregister
        // cleanup races a replacement publisher on the same pipeline.
        let replacement_url = format!(
            "srt://127.0.0.1:{}?streamid=publish:live/fault-srt-replacement-race&pkt_size=1316",
            ports.srt
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

            let disconnect_cleared = input["status"] == "on"
                && input["probeStatus"] == "ready"
                && input["lastSessionProtocol"].is_null()
                && input["lastDisconnectReason"].is_null()
                && input["lastFailurePhase"].is_null()
                && input["recentDisconnectError"] == false;
            let output_progressed =
                metrics.video_count.load(Ordering::Relaxed) > baseline_video + 10;
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
        let final_recent_disconnect_count =
            final_input["recentDisconnectCount"].as_u64().unwrap_or(0);
        let passed = baseline_video >= RECOVERY_WARM_VIDEO_MIN
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
        results.push(json!({
            "test": "rapid-srt-replacement-preserves-egress",
            "passed": passed,
            "baselineVideo": baseline_video,
            "baselineConnections": baseline_connections,
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
        }));

        stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
        if let Some(child) = replacement_child.as_mut() {
            stop_child(child).await;
        }
        stop_generalized_sink_server(sink_server);
    }

    // ── 3. Egress retry survives transient ingest gap within grace ─────
    {
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

        let retry =
            wait_for_output_retry_observation(api, &pid, &oid, Duration::from_secs(10)).await;

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
        results.push(json!({
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
        }));

        stop_generalized_sink_server(recovery_server);

        stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
        stop_child(&mut resumed_child).await;
    }

    // ── 4. Hung HLS PUT sink times out, retries, and recovers after restart ──
    {
        let pid = create_pipeline(api, "fault-hls-put-timeout").await?;
        let sink_dir = artifact_path("recovery-hls-put-timeout");
        let _ = std::fs::remove_dir_all(&sink_dir);
        std::fs::create_dir_all(&sink_dir).map_err(|e| e.to_string())?;

        let (hang_cancel, hang_handle) =
            start_hls_put_hang_sink(hls_put_port, Duration::from_secs(30)).await?;
        let oid = create_output(
            api,
            &pid,
            "hls-put-timeout",
            &format!(
                "http://127.0.0.1:{hls_put_port}/upload?cid=fault-hls-put-timeout&copy=0&file=out.m3u8"
            ),
            "source",
        )
        .await?;

        let mut pub_child = spawn_publisher(
            fixture_h264,
            &format!("rtmp://127.0.0.1:{}/live/fault-hls-put-timeout", ports.rtmp),
            "flv",
            false,
        )
        .await?;
        wait_for_api_input_live(api, &pid, timeout).await?;
        start_output(api, &pid, &oid).await?;

        let retry =
            wait_for_output_retry_observation(api, &pid, &oid, Duration::from_secs(20)).await;

        hang_cancel.cancel();
        let _ = hang_handle.await;

        let (sink_cancel, sink_handle) = start_hls_put_sink(hls_put_port, sink_dir.clone()).await?;
        let artifacts = wait_for_hls_put_artifacts(&sink_dir, Duration::from_secs(30)).await;
        let requests = read_hls_put_requests(&sink_dir).ok();
        let content_types_ok = requests.as_ref().is_some_and(|requests| {
            request_seen(requests, |r| {
                r["file"] == "out.m3u8" && r["contentType"] == "application/vnd.apple.mpegurl"
            }) && request_seen(requests, |r| {
                r["file"]
                    .as_str()
                    .is_some_and(|f| is_segment_file(f, "seg"))
                    && r["contentType"] == "video/mp2t"
            })
        });

        let recovery_deadline = Instant::now() + Duration::from_secs(20);
        let mut recovered = false;
        let mut recovery_status = String::from("unknown");
        let mut final_bytes_out = 0u64;
        while Instant::now() < recovery_deadline {
            tokio::time::sleep(Duration::from_millis(500)).await;
            if let Ok((status, _)) = api.get_output_status(&pid, &oid).await {
                recovery_status = status.status.clone();
                final_bytes_out = status.bytes_out;
                if recovery_status == "running" && !status.retrying && final_bytes_out > 0 {
                    recovered = true;
                    break;
                }
            }
        }

        let final_output = observe_final_output(api, &pid, &oid).await;
        let timeout_error_visible = {
            let lower = retry.last_error.to_ascii_lowercase();
            lower.contains("timed out") || lower.contains("deadline")
        };
        let failure_phase_ok =
            retry.failure_phase == "upload_segment" || retry.failure_phase == "upload_playlist";
        let passed = retry.status_visible
            && retry.health_visible
            && retry.has_error
            && timeout_error_visible
            && retry.phase == "failed"
            && failure_phase_ok
            && artifacts.is_ok()
            && content_types_ok
            && recovered
            && final_output.running
            && !final_output.retrying
            && final_output.error_cleared
            && final_bytes_out > 0;
        println!(
            "[fault] Hung HLS PUT sink recovers after timeout: {} (retrying={} healthRetrying={} timeoutVisible={} failurePhase={} recovered={} recoveryStatus={} finalRetrying={} bytesOut={})",
            if passed { "PASS" } else { "FAIL" },
            retry.status_visible,
            retry.health_visible,
            timeout_error_visible,
            retry.failure_phase,
            recovered,
            recovery_status,
            final_output.retrying,
            final_bytes_out,
        );
        results.push(json!({
            "test": "hls-put-timeout-recovers-after-restart",
            "passed": passed,
            "retryStatusVisible": retry.status_visible,
            "retryHealthVisible": retry.health_visible,
            "retryHasError": retry.has_error,
            "retryPhase": retry.phase,
            "retryFailurePhase": retry.failure_phase,
            "retryError": retry.last_error,
            "timeoutErrorVisible": timeout_error_visible,
            "artifactsFound": artifacts.is_ok(),
            "contentTypesCorrect": content_types_ok,
            "recovered": recovered,
            "recoveryStatus": recovery_status,
            "finalStatusRunning": final_output.running,
            "finalRetrying": final_output.retrying,
            "finalErrorCleared": final_output.error_cleared,
            "finalBytesOut": final_bytes_out,
            "finalStatus": final_output.status,
        }));

        stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
        stop_child(&mut pub_child).await;
        sink_cancel.cancel();
        let _ = sink_handle.await;
    }

    // ── 5. Transient RTMP sink flaps surface recovered output instability ──
    {
        let pid = create_pipeline(api, "fault-rtmp-sink-flap").await?;

        let oid = create_output(
            api,
            &pid,
            "rtmp-sink-flap",
            &format!("rtmp://127.0.0.1:{sink_port}/live/fault-rtmp-sink-flap"),
            "source",
        )
        .await?;

        let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
        let mut sink_server =
            Some(start_generalized_sink_server(sink_port, sink_metrics.clone()).await?);

        let mut pub_child = spawn_publisher(
            fixture_h264,
            &format!("rtmp://127.0.0.1:{}/live/fault-rtmp-sink-flap", ports.rtmp),
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

        if let Some(server) = sink_server.take() {
            stop_generalized_sink_server(server);
        }
        let first_retry =
            wait_for_output_retry_observation(api, &pid, &oid, Duration::from_secs(10)).await;

        sink_server = Some(start_generalized_sink_server(sink_port, sink_metrics.clone()).await?);
        let first_recovered = wait_for_output_running_and_sink_video_above(
            api,
            &pid,
            &oid,
            &sink_metrics,
            baseline_video + 10,
            Duration::from_secs(25),
        )
        .await;

        if let Some(server) = sink_server.take() {
            stop_generalized_sink_server(server);
        }
        let second_retry =
            wait_for_output_retry_observation(api, &pid, &oid, Duration::from_secs(10)).await;

        sink_server = Some(start_generalized_sink_server(sink_port, sink_metrics.clone()).await?);
        let second_recovered = wait_for_output_running_and_sink_video_above(
            api,
            &pid,
            &oid,
            &sink_metrics,
            baseline_video + 20,
            Duration::from_secs(25),
        )
        .await;

        let final_output = observe_final_output(api, &pid, &oid).await;
        let passed = baseline_video >= RECOVERY_WARM_VIDEO_MIN
            && first_retry.status_visible
            && first_retry.health_visible
            && first_retry.has_error
            && first_recovered
            && second_retry.status_visible
            && second_retry.health_visible
            && second_retry.has_error
            && second_recovered
            && final_output.running
            && !final_output.retrying
            && final_output.error_cleared
            && final_output.recent_failure_count >= 2
            && final_output.flapping
            && final_output.health_recent_failure_count >= 2
            && final_output.health_flapping;
        println!(
            "[fault] RTMP sink flaps surface recovered-output instability: {} (firstRetrying={} secondRetrying={} firstRecovered={} secondRecovered={} finalRetrying={} finalFlapping={} recentFailureCount={})",
            if passed { "PASS" } else { "FAIL" },
            first_retry.status_visible,
            second_retry.status_visible,
            first_recovered,
            second_recovered,
            final_output.retrying,
            final_output.flapping,
            final_output.recent_failure_count,
        );
        results.push(json!({
            "test": "rtmp-sink-flaps-surface-output-instability",
            "passed": passed,
            "baselineVideo": baseline_video,
            "firstRetrying": first_retry.status_visible,
            "firstHealthRetrying": first_retry.health_visible,
            "firstRetryError": first_retry.has_error,
            "firstRecovered": first_recovered,
            "secondRetrying": second_retry.status_visible,
            "secondHealthRetrying": second_retry.health_visible,
            "secondRetryError": second_retry.has_error,
            "secondRecovered": second_recovered,
            "finalStatusRunning": final_output.running,
            "finalRetrying": final_output.retrying,
            "finalErrorCleared": final_output.error_cleared,
            "finalRecentFailureCount": final_output.recent_failure_count,
            "finalFlapping": final_output.flapping,
            "healthRecentFailureCount": final_output.health_recent_failure_count,
            "healthFlapping": final_output.health_flapping,
            "finalStatus": final_output.status,
            "finalHealthOutput": final_output.health,
        }));

        stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
        stop_child(&mut pub_child).await;
        if let Some(server) = sink_server.take() {
            stop_generalized_sink_server(server);
        }
    }

    // ── 6. Transient SRT sink flaps surface recovered output instability ──
    {
        let pid = create_pipeline(api, "fault-srt-sink-flap").await?;
        let sink_stream_key = "fault-srt-sink-flap-target";
        let mut sink_pid =
            create_pipeline_with_stream_key(api, "srt-sink-flap-target-1", sink_stream_key).await?;

        let oid = create_output(
            api,
            &pid,
            "srt-sink-flap",
            &format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{}&pkt_size=1316",
                ports.srt, sink_stream_key
            ),
            "source",
        )
        .await?;

        let mut pub_child = spawn_publisher(
            fixture_h264,
            &format!(
                "srt://127.0.0.1:{}?streamid=publish:live/fault-srt-sink-flap&pkt_size=1316",
                ports.srt
            ),
            "mpegts",
            true,
        )
        .await?;
        wait_for_api_input_live(api, &pid, timeout).await?;
        start_output(api, &pid, &oid).await?;

        wait_for_api_input_media_ready(api, &sink_pid, Duration::from_secs(25)).await?;

        delete_pipeline_v1(api, &sink_pid).await?;
        let first_retry =
            wait_for_output_retry_observation(api, &pid, &oid, Duration::from_secs(12)).await;

        sink_pid =
            create_pipeline_with_stream_key(api, "srt-sink-flap-target-2", sink_stream_key).await?;
        let first_recovery_ready =
            wait_for_api_input_media_ready(api, &sink_pid, Duration::from_secs(25)).await;
        let first_recovered =
            wait_for_output_running(api, &pid, &oid, Duration::from_secs(25)).await;

        delete_pipeline_v1(api, &sink_pid).await?;
        let second_retry =
            wait_for_output_retry_observation(api, &pid, &oid, Duration::from_secs(12)).await;

        sink_pid =
            create_pipeline_with_stream_key(api, "srt-sink-flap-target-3", sink_stream_key).await?;
        let second_recovery_ready =
            wait_for_api_input_media_ready(api, &sink_pid, Duration::from_secs(25)).await;
        let second_recovered =
            wait_for_output_running(api, &pid, &oid, Duration::from_secs(25)).await;

        let final_output = observe_final_output(api, &pid, &oid).await;
        let passed = first_retry.status_visible
            && first_retry.health_visible
            && first_retry.has_error
            && first_recovery_ready.is_ok()
            && first_recovered
            && second_retry.status_visible
            && second_retry.health_visible
            && second_retry.has_error
            && second_recovery_ready.is_ok()
            && second_recovered
            && final_output.running
            && !final_output.retrying
            && final_output.error_cleared
            && final_output.recent_failure_count >= 2
            && final_output.flapping
            && final_output.health_recent_failure_count >= 2
            && final_output.health_flapping;
        println!(
            "[fault] SRT sink flaps surface recovered-output instability: {} (firstRetrying={} secondRetrying={} firstRecovered={} secondRecovered={} finalRetrying={} finalFlapping={} recentFailureCount={})",
            if passed { "PASS" } else { "FAIL" },
            first_retry.status_visible,
            second_retry.status_visible,
            first_recovered,
            second_recovered,
            final_output.retrying,
            final_output.flapping,
            final_output.recent_failure_count,
        );
        results.push(json!({
            "test": "srt-sink-flaps-surface-output-instability",
            "passed": passed,
            "firstRetrying": first_retry.status_visible,
            "firstHealthRetrying": first_retry.health_visible,
            "firstRetryError": first_retry.has_error,
            "firstRecoveryReady": first_recovery_ready.is_ok(),
            "firstRecoveryReadyError": first_recovery_ready.err(),
            "firstRecovered": first_recovered,
            "secondRetrying": second_retry.status_visible,
            "secondHealthRetrying": second_retry.health_visible,
            "secondRetryError": second_retry.has_error,
            "secondRecoveryReady": second_recovery_ready.is_ok(),
            "secondRecoveryReadyError": second_recovery_ready.err(),
            "secondRecovered": second_recovered,
            "finalStatusRunning": final_output.running,
            "finalRetrying": final_output.retrying,
            "finalErrorCleared": final_output.error_cleared,
            "finalRecentFailureCount": final_output.recent_failure_count,
            "finalFlapping": final_output.flapping,
            "healthRecentFailureCount": final_output.health_recent_failure_count,
            "healthFlapping": final_output.health_flapping,
            "finalStatus": final_output.status,
            "finalHealthOutput": final_output.health,
        }));

        stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
        stop_child(&mut pub_child).await;
        let _ = delete_pipeline_v1(api, &sink_pid).await;
    }

    Ok(results)
}

pub(super) async fn run_publisher_disconnect_case(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    timeout: Duration,
    case: &PublisherDisconnectCase,
) -> Result<Value, String> {
    let pid = create_pipeline(api, &case.pipeline).await?;

    let mut pub_child = spawn_publisher(
        fixture_h264,
        &case.protocol.publish_url(ports, &case.pipeline),
        case.protocol.ffmpeg_format(),
        case.protocol.map_all_streams(),
    )
    .await?;
    wait_for_api_input_live(api, &pid, timeout).await?;
    println!("[fault] {} publisher live", case.log_label);

    stop_child(&mut pub_child).await;
    let started = Instant::now();
    let off_result = wait_for_api_input_off(api, &pid, timeout).await;
    let elapsed = started.elapsed();
    let off_health = api.get_json("/api/v1/engine/health").await.ok();
    let off_input = health_input_snapshot(off_health.as_ref(), &pid);
    let assert_disconnect_fields = matches!(case.protocol, HarnessPublisherProtocol::Rtmp);
    let disconnect_fields_ok = !assert_disconnect_fields
        || (off_input["lastSessionProtocol"] == "rtmp"
            && off_input["lastDisconnectAt"].is_string()
            && off_input["lastDisconnectReason"] == "publisher disconnected"
            && off_input["lastFailurePhase"] == "disconnect"
            && off_input["recentDisconnectError"] == false);
    let passed = off_result.is_ok() && disconnect_fields_ok;
    println!(
        "[fault] {} publisher disconnect: {} ({:.1}s)",
        case.log_label,
        if passed { "PASS" } else { "FAIL" },
        elapsed.as_secs_f64()
    );

    let mut result = json!({
        "test": case.test_name,
        "passed": passed,
        "elapsedMs": elapsed.as_millis(),
        "error": off_result.err(),
        "disconnectFieldsOk": disconnect_fields_ok,
    });
    if assert_disconnect_fields {
        result["inputSnapshot"] = off_input;
    }
    Ok(result)
}

async fn configure_file_ingest_case(
    api: &RampApi,
    pipeline_id: &str,
    stream_key: &str,
    fixture: &Path,
) -> Result<String, String> {
    let fixture_name = fixture.file_name().unwrap().to_string_lossy().to_string();
    let media_root = harness_media_root();
    std::fs::create_dir_all(&media_root).map_err(|e| e.to_string())?;
    let media_dest = media_root.join(&fixture_name);
    if !media_dest.exists() {
        std::fs::copy(fixture, &media_dest).map_err(|e| e.to_string())?;
    }

    api.put_json(
        &format!("/api/v1/pipelines/{pipeline_id}/file-ingest"),
        json!({"filename": fixture_name, "loop": false}),
    )
    .await?;

    let ingest_list = api.get_json("/api/v1/ingests").await?;
    ingest_list
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|ingest| ingest["streamKey"].as_str() == Some(stream_key))
        })
        .and_then(|ingest| ingest["id"].as_str())
        .map(str::to_string)
        .ok_or_else(|| format!("file ingest not found in list for {stream_key}"))
}

fn harness_media_root() -> PathBuf {
    PathBuf::from(
        std::env::var("RESTREAM_MEDIA_DIR")
            .unwrap_or_else(|_| restream::config::DEFAULT_MEDIA_DIR.into()),
    )
}

fn recording_file_exists(media_root: &Path, pipeline_name: &str) -> bool {
    std::fs::read_dir(media_root)
        .ok()
        .into_iter()
        .flatten()
        .flatten()
        .any(|entry| {
            let path = entry.path();
            path.extension()
                .is_some_and(|ext| ext == "ts" || ext == "mp4")
                && path
                    .file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.contains(pipeline_name))
        })
}

async fn wait_for_recording_file(media_root: &Path, pipeline_name: &str) -> bool {
    let deadline = Instant::now() + Duration::from_secs(20);
    while Instant::now() < deadline {
        if recording_file_exists(media_root, pipeline_name) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
    recording_file_exists(media_root, pipeline_name)
}

pub(super) async fn run_ingest_lifecycle_case(
    api: &RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    case: &IngestLifecycleCase,
) -> Result<Value, String> {
    let pid = create_pipeline(api, &case.pipeline).await?;
    let file_eof_restart = matches!(case.file_completion, Some(FileIngestCompletion::EofRestart));
    let (mut publisher, mut file_ingest_id): (Option<Child>, Option<String>) = (None, None);

    match case.kind {
        IngestLifecycleKind::FileIngest => {
            let ingest_id =
                configure_file_ingest_case(api, &pid, &case.pipeline, fixture_h264).await?;
            api.post_empty(&format!("/api/v1/ingests/{ingest_id}/start"))
                .await?;
            file_ingest_id = Some(ingest_id);
        }
        IngestLifecycleKind::HlsPreview | IngestLifecycleKind::Recording => {
            publisher = Some(
                spawn_publisher(
                    fixture_h264,
                    &format!("rtmp://127.0.0.1:{}/live/{}", ports.rtmp, case.pipeline),
                    "flv",
                    false,
                )
                .await?,
            );
        }
    }
    wait_for_api_input_live(api, &pid, Duration::from_secs(30)).await?;

    let (mut hls_playlist_status, mut hls_playlist_ok, mut hls_playlist_error) = (None, None, None);
    let active_result = match case.kind {
        IngestLifecycleKind::FileIngest => match case.file_completion {
            Some(FileIngestCompletion::EofRestart) => Some(
                wait_for_pipeline_file_ingest_running_state(
                    api,
                    &pid,
                    true,
                    Duration::from_secs(10),
                )
                .await,
            ),
            Some(FileIngestCompletion::Stop) => {
                println!("[fault] File ingest live");
                None
            }
            None => return Err(format!("{} missing fileCompletion", case.test_name)),
        },
        IngestLifecycleKind::HlsPreview => {
            match wait_for_hls_playlist_ready(api, &pid, Duration::from_secs(15)).await {
                Ok((status, body)) => {
                    hls_playlist_status = Some(status);
                    hls_playlist_ok = Some(body.contains("#EXTM3U"));
                }
                Err(error) => {
                    hls_playlist_status = Some(reqwest::StatusCode::NOT_FOUND);
                    hls_playlist_ok = Some(false);
                    hls_playlist_error = Some(error);
                }
            }
            Some(wait_for_api_hls_preview_state(api, &pid, true, Duration::from_secs(10)).await)
        }
        IngestLifecycleKind::Recording => {
            api.post_empty(&format!("/api/v1/pipelines/{pid}/recording/start"))
                .await?;
            Some(wait_for_api_recording_state(api, &pid, true, Duration::from_secs(10)).await)
        }
    };

    match case.kind {
        IngestLifecycleKind::FileIngest => {
            if matches!(case.file_completion, Some(FileIngestCompletion::Stop)) {
                let ingest_id = file_ingest_id.as_ref().ok_or("file ingest id missing")?;
                api.post_empty(&format!("/api/v1/ingests/{ingest_id}/stop"))
                    .await?;
            }
        }
        IngestLifecycleKind::HlsPreview | IngestLifecycleKind::Recording => {
            if matches!(case.kind, IngestLifecycleKind::Recording) {
                tokio::time::sleep(Duration::from_secs(6)).await;
            }
            if let Some(child) = publisher.as_mut() {
                stop_child(child).await;
            }
        }
    }

    let started = Instant::now();
    let off_result =
        wait_for_api_input_off(api, &pid, Duration::from_secs(case.input_off_timeout_secs)).await;
    let inactive_result = match case.kind {
        IngestLifecycleKind::FileIngest if file_eof_restart => Some(
            wait_for_pipeline_file_ingest_running_state(api, &pid, false, Duration::from_secs(10))
                .await,
        ),
        IngestLifecycleKind::HlsPreview => {
            Some(wait_for_api_hls_preview_state(api, &pid, false, Duration::from_secs(15)).await)
        }
        IngestLifecycleKind::Recording => {
            Some(wait_for_api_recording_state(api, &pid, false, Duration::from_secs(10)).await)
        }
        IngestLifecycleKind::FileIngest => None,
    };

    let active_ok = active_result.as_ref().is_none_or(Result::is_ok);
    let inactive_ok = inactive_result.as_ref().is_none_or(Result::is_ok);
    let restart_result = if file_eof_restart {
        if off_result.is_ok() && inactive_ok {
            let ingest_id = file_ingest_id.as_ref().ok_or("file ingest id missing")?;
            match api
                .post_empty(&format!("/api/v1/ingests/{ingest_id}/start"))
                .await
            {
                Ok(_) => {
                    if let Err(error) =
                        wait_for_api_input_live(api, &pid, Duration::from_secs(30)).await
                    {
                        Err(error)
                    } else {
                        api.post_empty(&format!("/api/v1/ingests/{ingest_id}/stop"))
                            .await
                            .map(|_| ())
                    }
                }
                Err(error) => Err(error),
            }
        } else {
            Err("skipped restart because EOF cleanup did not complete".to_string())
        }
    } else {
        Ok(())
    };
    let feature_result = match case.kind {
        IngestLifecycleKind::FileIngest => json!({}),
        IngestLifecycleKind::HlsPreview => {
            let mut final_status = reqwest::StatusCode::OK;
            let mut playlist_gone = false;
            let shutdown_deadline = Instant::now() + Duration::from_secs(15);
            while Instant::now() < shutdown_deadline {
                let (status, _) = api
                    .get_text_response(&format!("/hls/{pid}/master.m3u8"))
                    .await?;
                final_status = status;
                if status == reqwest::StatusCode::NOT_FOUND {
                    playlist_gone = true;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(500)).await;
            }
            json!({"finalPlaylistStatus": final_status.as_u16(), "finalPlaylistGone": playlist_gone})
        }
        IngestLifecycleKind::Recording => {
            // A completed recording may already have been remuxed from .ts to
            // .mp4 (recording.rs deletes the source .ts on successful remux
            // unless retention is enabled), so either extension counts as found.
            let media_root = harness_media_root();
            let recording_file_found = wait_for_recording_file(&media_root, &case.pipeline).await;
            let state = inactive_result
                .as_ref()
                .and_then(|result| result.as_ref().ok());
            json!({
                "recordingEnabled": state.and_then(|state| state["enabled"].as_bool()).unwrap_or(false),
                "recordingActive": state.and_then(|state| state["active"].as_bool()).unwrap_or(true),
                "recordingFileFound": recording_file_found,
            })
        }
    };
    let elapsed = started.elapsed();
    let feature_ok = match case.kind {
        IngestLifecycleKind::FileIngest => true,
        IngestLifecycleKind::HlsPreview => {
            hls_playlist_ok == Some(true) && feature_result["finalPlaylistGone"] == true
        }
        IngestLifecycleKind::Recording => {
            feature_result["recordingEnabled"] == true
                && feature_result["recordingActive"] == false
                && feature_result["recordingFileFound"] == true
        }
    };
    let passed =
        active_ok && off_result.is_ok() && inactive_ok && restart_result.is_ok() && feature_ok;
    println!(
        "[fault] {}: {} ({:.1}s)",
        case.test_name,
        if passed { "PASS" } else { "FAIL" },
        elapsed.as_secs_f64()
    );

    let mut result = json!({
        "test": case.test_name,
        "passed": passed,
        "elapsedMs": elapsed.as_millis(),
    });
    if file_eof_restart {
        result["runningError"] = json!(active_result.and_then(Result::err));
        result["inputOffError"] = json!(off_result.err());
        result["stoppedError"] = json!(inactive_result.and_then(Result::err));
        result["restartError"] = json!(restart_result.err());
    } else if matches!(case.kind, IngestLifecycleKind::FileIngest) {
        result["error"] = json!(off_result.err());
    } else {
        result["inputOffError"] = json!(off_result.err());
        if matches!(case.kind, IngestLifecycleKind::HlsPreview) {
            result["playlistStatus"] = json!(hls_playlist_status.map(|status| status.as_u16()));
            result["playlistOk"] = json!(hls_playlist_ok);
            result["playlistError"] = json!(hls_playlist_error);
            result["hlsPreviewActiveError"] = json!(active_result.and_then(Result::err));
            result["hlsPreviewInactiveError"] = json!(inactive_result.and_then(Result::err));
        } else {
            result["recordingActiveError"] = json!(active_result.and_then(Result::err));
            result["recordingInactiveError"] = json!(inactive_result.and_then(Result::err));
        }
        if let Some(extra) = feature_result.as_object() {
            for (key, value) in extra {
                result[key] = value.clone();
            }
        }
    }
    Ok(result)
}
