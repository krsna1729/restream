//! Transient sink flaps: each loss retries and recovers, and the output
//! reports recovered instability (recent failures and flapping).

use super::super::*;

fn read_log_or_error(path: &Path) -> String {
    std::fs::read_to_string(path)
        .unwrap_or_else(|error| format!("failed to read {}: {error}", path.display()))
}

fn srt_owner_timeline(
    restream_log: &Path,
    source_pipeline_id: &str,
    sink_pipeline_ids: &[String],
) -> Vec<String> {
    match std::fs::read_to_string(restream_log) {
        Ok(log) => log
            .lines()
            .filter(|line| {
                line.contains(source_pipeline_id)
                    || sink_pipeline_ids
                        .iter()
                        .any(|pipeline_id| line.contains(pipeline_id.as_str()))
                    || line.contains("SRT ingress Owner")
                    || line.contains("srt ingress owner")
                    || line.contains("SRT listener")
                    || line.contains("SRT peer")
                    || line.contains("rejecting SRT")
                    || line.contains("rejecting unauthorized SRT stream")
            })
            .map(str::to_owned)
            .collect(),
        Err(error) => vec![format!(
            "failed to read {}: {error}",
            restream_log.display()
        )],
    }
}

fn write_failure_diagnostic(path: &Path, label: &str, detail: &Value) {
    println!("[fault-detail] {label}: {detail}");
    match serde_json::to_vec_pretty(detail) {
        Ok(contents) => {
            if let Err(error) = std::fs::write(path, contents) {
                eprintln!("[fault-detail] failed to write {}: {error}", path.display());
            } else {
                println!("[fault-detail] artifact={}", path.display());
            }
        }
        Err(error) => eprintln!("[fault-detail] failed to serialize diagnostic: {error}"),
    }
}

/// Drops and restores an RTMP sink twice. Expects each loss to retry and
/// each return to resume delivery, ending running with flapping reported.
pub(crate) async fn rtmp_sink_flaps_surface_instability(
    api: &mut RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
    timeout: Duration,
) -> Result<Value, String> {
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
    let first_recovery_output = observe_final_output(api, &pid, &oid).await;
    let first_recovery_sink = json!({
        "connections": sink_metrics.connections.load(Ordering::Relaxed),
        "publishing": sink_metrics.publishing.load(Ordering::Relaxed),
        "videoCount": sink_metrics.video_count.load(Ordering::Relaxed),
    });

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
    let second_recovery_sink = json!({
        "connections": sink_metrics.connections.load(Ordering::Relaxed),
        "publishing": sink_metrics.publishing.load(Ordering::Relaxed),
        "videoCount": sink_metrics.video_count.load(Ordering::Relaxed),
    });

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
    let result = json!({
        "test": "rtmp-sink-flaps-surface-output-instability",
        "passed": passed,
        "baselineVideo": baseline_video,
        "firstRetrying": first_retry.status_visible,
        "firstHealthRetrying": first_retry.health_visible,
        "firstRetryError": first_retry.has_error,
        "firstRecovered": first_recovered,
        "firstRecoverySink": first_recovery_sink,
        "firstRecoveryOutput": {
            "status": first_recovery_output.status.unwrap_or(Value::Null),
            "health": first_recovery_output.health,
        },
        "secondRetrying": second_retry.status_visible,
        "secondHealthRetrying": second_retry.health_visible,
        "secondRetryError": second_retry.has_error,
        "secondRecovered": second_recovered,
        "secondRecoverySink": second_recovery_sink,
        "finalStatusRunning": final_output.running,
        "finalRetrying": final_output.retrying,
        "finalErrorCleared": final_output.error_cleared,
        "finalRecentFailureCount": final_output.recent_failure_count,
        "finalFlapping": final_output.flapping,
        "healthRecentFailureCount": final_output.health_recent_failure_count,
        "healthFlapping": final_output.health_flapping,
        "finalStatus": final_output.status,
        "finalHealthOutput": final_output.health,
    });

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
    stop_child(&mut pub_child).await;
    if let Some(server) = sink_server.take() {
        stop_generalized_sink_server(server);
    }

    Ok(result)
}

/// Removes and recreates the SRT ingest pipeline an SRT output publishes
/// into, twice. Expects the same retry/recovery/flapping contract as the RTMP
/// case, and writes owner-timeline diagnostics when it fails.
pub(crate) async fn srt_sink_flaps_surface_instability(
    api: &mut RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    timeout: Duration,
    work_dir: &Path,
) -> Result<Value, String> {
    let restream_log = work_dir.join("restream.log");

    let pid = create_pipeline(api, "fault-srt-sink-flap").await?;
    let sink_stream_key = "fault-srt-sink-flap-target";
    let mut sink_pid =
        create_pipeline_with_stream_key(api, "srt-sink-flap-target-1", sink_stream_key).await?;
    let mut sink_pipeline_ids = vec![sink_pid.clone()];

    let oid = create_output(
        api,
        &pid,
        "srt-sink-flap",
        &harness_srt_output_url(ports.srt, sink_stream_key, HarnessSrtMode::Publish),
        "source",
    )
    .await?;

    let publisher_log = work_dir.join("srt-sink-flap-publisher.log");
    let mut pub_child = spawn_publisher_with_selection(
        fixture_h264,
        &harness_srt_ffmpeg_url(
            ports.srt,
            "fault-srt-sink-flap",
            HarnessSrtMode::Publish,
            None,
        ),
        "mpegts",
        PublishTrackSelection::AllStreams,
        Some(&publisher_log),
    )?;
    wait_for_api_input_live(api, &pid, timeout).await?;
    start_output(api, &pid, &oid).await?;

    if let Err(first_failure_reason) =
        wait_for_api_input_media_ready(api, &sink_pid, Duration::from_secs(25)).await
    {
        let publisher_exit_status = match pub_child.try_wait() {
            Ok(Some(status)) => format!("exited: {status}"),
            Ok(None) => "running".to_string(),
            Err(error) => format!("status unavailable: {error}"),
        };
        let publisher_stderr = read_log_or_error(&publisher_log);
        let owner_timeline = srt_owner_timeline(&restream_log, &pid, &sink_pipeline_ids);
        let health = api.get_json("/api/v1/engine/health").await.ok();
        let source_input_snapshot = health
            .as_ref()
            .map(|health| health["pipelines"][&pid]["input"].clone())
            .unwrap_or(Value::Null);
        let sink_input_snapshot = health
            .as_ref()
            .map(|health| health["pipelines"][&sink_pid]["input"].clone())
            .unwrap_or(Value::Null);
        let output_state = observe_final_output(api, &pid, &oid).await;
        let diagnostics_path = work_dir.join("srt-sink-flaps-failure.json");
        let detail = json!({
            "test": "srt-sink-flaps-surface-output-instability",
            "firstFailurePhase": "initial-sink-media-ready",
            "firstFailureReason": &first_failure_reason,
            "sourcePipelineId": &pid,
            "sinkPipelineIds": &sink_pipeline_ids,
            "outputId": &oid,
            "sourceInputSnapshot": source_input_snapshot,
            "sinkInputSnapshot": sink_input_snapshot,
            "publisherExitStatus": &publisher_exit_status,
            "publisherLogPath": publisher_log.display().to_string(),
            "publisherStderr": publisher_stderr,
            "ownerTimeline": owner_timeline,
            "outputState": {
                "running": output_state.running,
                "retrying": output_state.retrying,
                "errorCleared": output_state.error_cleared,
                "recentFailureCount": output_state.recent_failure_count,
                "flapping": output_state.flapping,
                "status": &output_state.status,
                "health": &output_state.health,
            },
        });
        write_failure_diagnostic(
            &diagnostics_path,
            "SRT sink flap initial media failure",
            &detail,
        );
        let result = json!({
            "test": "srt-sink-flaps-surface-output-instability",
            "passed": false,
            "firstFailurePhase": "initial-sink-media-ready",
            "firstFailureReason": &first_failure_reason,
            "sourcePipelineId": &pid,
            "sinkPipelineIds": &sink_pipeline_ids,
            "outputId": &oid,
            "publisherExitStatus": &publisher_exit_status,
            "publisherLogPath": publisher_log.display().to_string(),
            "failureDiagnostics": diagnostics_path.display().to_string(),
        });
        stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
        stop_child(&mut pub_child).await;
        let _ = delete_pipeline_v1(api, &sink_pid).await;
        return Ok(result);
    }

    delete_pipeline_v1(api, &sink_pid).await?;
    let first_retry =
        wait_for_output_retry_observation(api, &pid, &oid, Duration::from_secs(12)).await;

    sink_pid =
        create_pipeline_with_stream_key(api, "srt-sink-flap-target-2", sink_stream_key).await?;
    sink_pipeline_ids.push(sink_pid.clone());
    let first_recovery_ready =
        wait_for_api_input_media_ready(api, &sink_pid, Duration::from_secs(25)).await;
    let first_recovered = wait_for_output_running(api, &pid, &oid, Duration::from_secs(25)).await;

    delete_pipeline_v1(api, &sink_pid).await?;
    let second_retry =
        wait_for_output_retry_observation(api, &pid, &oid, Duration::from_secs(12)).await;

    sink_pid =
        create_pipeline_with_stream_key(api, "srt-sink-flap-target-3", sink_stream_key).await?;
    sink_pipeline_ids.push(sink_pid.clone());
    let second_recovery_ready =
        wait_for_api_input_media_ready(api, &sink_pid, Duration::from_secs(25)).await;
    let second_recovered = wait_for_output_running(api, &pid, &oid, Duration::from_secs(25)).await;
    let first_recovery_ready_error = first_recovery_ready.as_ref().err().cloned();
    let second_recovery_ready_error = second_recovery_ready.as_ref().err().cloned();

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
    let publisher_exit_status = match pub_child.try_wait() {
        Ok(Some(status)) => format!("exited: {status}"),
        Ok(None) => "running".to_string(),
        Err(error) => format!("status unavailable: {error}"),
    };
    let diagnostics_path = work_dir.join("srt-sink-flaps-failure.json");
    if !passed {
        let publisher_stderr = read_log_or_error(&publisher_log);
        let owner_timeline = srt_owner_timeline(&restream_log, &pid, &sink_pipeline_ids);
        let detail = json!({
            "test": "srt-sink-flaps-surface-output-instability",
            "sourcePipelineId": &pid,
            "sinkPipelineIds": &sink_pipeline_ids,
            "outputId": &oid,
            "firstRecoveryReady": first_recovery_ready.is_ok(),
            "firstRecoveryReadyError": &first_recovery_ready_error,
            "firstRecovered": first_recovered,
            "secondRecoveryReady": second_recovery_ready.is_ok(),
            "secondRecoveryReadyError": &second_recovery_ready_error,
            "secondRecovered": second_recovered,
            "firstRetry": {
                "statusVisible": first_retry.status_visible,
                "healthVisible": first_retry.health_visible,
                "hasError": first_retry.has_error,
                "failurePhase": &first_retry.failure_phase,
                "lastError": &first_retry.last_error,
                "attempts": first_retry.attempts,
                "backoffMs": first_retry.backoff_ms,
            },
            "secondRetry": {
                "statusVisible": second_retry.status_visible,
                "healthVisible": second_retry.health_visible,
                "hasError": second_retry.has_error,
                "failurePhase": &second_retry.failure_phase,
                "lastError": &second_retry.last_error,
                "attempts": second_retry.attempts,
                "backoffMs": second_retry.backoff_ms,
            },
            "finalOutput": {
                "running": final_output.running,
                "retrying": final_output.retrying,
                "errorCleared": final_output.error_cleared,
                "recentFailureCount": final_output.recent_failure_count,
                "flapping": final_output.flapping,
                "healthRecentFailureCount": final_output.health_recent_failure_count,
                "healthFlapping": final_output.health_flapping,
                "status": &final_output.status,
                "health": &final_output.health,
            },
            "publisherExitStatus": &publisher_exit_status,
            "publisherLogPath": publisher_log.display().to_string(),
            "publisherStderr": publisher_stderr,
            "ownerTimeline": owner_timeline,
        });
        write_failure_diagnostic(&diagnostics_path, "SRT sink flap failure", &detail);
    }
    println!(
        "[fault] SRT sink flaps surface recovered-output instability: {} (firstRetrying={} secondRetrying={} firstRecovered={} secondRecovered={} firstRecoveryError={:?} secondRecoveryError={:?} publisherExit={} finalRetrying={} finalFlapping={} recentFailureCount={})",
        if passed { "PASS" } else { "FAIL" },
        first_retry.status_visible,
        second_retry.status_visible,
        first_recovered,
        second_recovered,
        first_recovery_ready_error,
        second_recovery_ready_error,
        publisher_exit_status,
        final_output.retrying,
        final_output.flapping,
        final_output.recent_failure_count,
    );
    let result = json!({
        "test": "srt-sink-flaps-surface-output-instability",
        "passed": passed,
        "sourcePipelineId": &pid,
        "sinkPipelineIds": &sink_pipeline_ids,
        "outputId": &oid,
        "firstRetrying": first_retry.status_visible,
        "firstHealthRetrying": first_retry.health_visible,
        "firstRetryError": first_retry.has_error,
        "firstRecoveryReady": first_recovery_ready.is_ok(),
        "firstRecoveryReadyError": first_recovery_ready_error,
        "firstRecovered": first_recovered,
        "secondRetrying": second_retry.status_visible,
        "secondHealthRetrying": second_retry.health_visible,
        "secondRetryError": second_retry.has_error,
        "secondRecoveryReady": second_recovery_ready.is_ok(),
        "secondRecoveryReadyError": second_recovery_ready_error,
        "secondRecovered": second_recovered,
        "publisherExitStatus": publisher_exit_status,
        "publisherLogPath": publisher_log.display().to_string(),
        "failureDiagnostics": (!passed).then(|| diagnostics_path.display().to_string()),
        "finalStatusRunning": final_output.running,
        "finalRetrying": final_output.retrying,
        "finalErrorCleared": final_output.error_cleared,
        "finalRecentFailureCount": final_output.recent_failure_count,
        "finalFlapping": final_output.flapping,
        "healthRecentFailureCount": final_output.health_recent_failure_count,
        "healthFlapping": final_output.health_flapping,
        "finalStatus": final_output.status,
        "finalHealthOutput": final_output.health,
    });

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
    stop_child(&mut pub_child).await;
    let _ = delete_pipeline_v1(api, &sink_pid).await;

    Ok(result)
}

#[cfg(test)]
mod tests {
    #[test]
    fn srt_owner_timeline_keeps_owner_attachment_diagnostics() {
        let path = std::env::temp_dir().join(format!(
            "restream-srt-owner-timeline-{}.log",
            std::process::id()
        ));
        let _ = std::fs::remove_file(&path);
        std::fs::write(
            &path,
            "srt ingress owner attached rx_ring_entries=512\nunrelated event\n",
        )
        .unwrap();

        let timeline = super::srt_owner_timeline(&path, "source-pipeline", &[]);
        std::fs::remove_file(&path).unwrap();

        assert!(
            timeline
                .iter()
                .any(|line| line.contains("rx_ring_entries=512")),
            "Owner startup configuration was missing from {timeline:?}"
        );
        assert_eq!(timeline.len(), 1);
    }
}
