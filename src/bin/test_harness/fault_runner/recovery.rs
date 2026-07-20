//! Fault/recovery runtime, orchestration, and assertion helpers.

use super::super::*;

pub(crate) async fn recovery_live_cases(
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

    for case in input_promotion_cases() {
        results.push(
            run_input_promotion_case(api, ports, fixture_h264, sink_port, timeout, case).await?,
        );
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
            &harness_srt_output_url(ports.srt, sink_stream_key, HarnessSrtMode::Publish),
            "source",
        )
        .await?;

        let mut pub_child = spawn_publisher(
            fixture_h264,
            &harness_srt_ffmpeg_url(
                ports.srt,
                "fault-srt-sink-flap",
                HarnessSrtMode::Publish,
                None,
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
