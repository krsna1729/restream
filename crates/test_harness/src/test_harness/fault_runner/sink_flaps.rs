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

/// Stable handles for one SRT sink-flap case, shared across its phases.
struct SrtFlapHandles<'a> {
    pipeline_id: &'a str,
    sink_stream_key: &'a str,
    output_id: &'a str,
    restream_log: &'a Path,
    publisher_log: &'a Path,
    work_dir: &'a Path,
}

/// Retry evidence recorded for one sink loss.
struct SrtFlapRetry {
    status_visible: bool,
    health_visible: bool,
    has_error: bool,
    failure_phase: String,
    last_error: String,
    attempts: Option<u64>,
    backoff_ms: Option<u64>,
}

/// Evidence from both sink loss/recreate cycles.
struct SrtFlapCycles {
    first_retry: SrtFlapRetry,
    first_recovery_ready: Result<Value, String>,
    first_recovered: bool,
    second_retry: SrtFlapRetry,
    second_recovery_ready: Result<Value, String>,
    second_recovered: bool,
}

/// Fields of the settled output copied out of the observation.
struct SrtFlapFinal {
    running: bool,
    retrying: bool,
    error_cleared: bool,
    recent_failure_count: u64,
    flapping: bool,
    health_recent_failure_count: u64,
    health_flapping: bool,
    status: Option<Value>,
    health: Value,
}

/// Describe the publisher process's exit state.
fn publisher_exit_status(child: &mut Child) -> String {
    match child.try_wait() {
        Ok(Some(status)) => format!("exited: {status}"),
        Ok(None) => "running".to_string(),
        Err(error) => format!("status unavailable: {error}"),
    }
}

/// Diagnose and report a failure before the sink ever becomes media-ready.
async fn srt_flap_initial_failure(
    api: &RampApi,
    handles: &SrtFlapHandles<'_>,
    pub_child: &mut Child,
    sink_pid: &str,
    sink_pipeline_ids: &[String],
    first_failure_reason: String,
) -> Result<Value, String> {
    let publisher_exit_status = publisher_exit_status(pub_child);
    let publisher_stderr = read_log_or_error(handles.publisher_log);
    let owner_timeline =
        srt_owner_timeline(handles.restream_log, handles.pipeline_id, sink_pipeline_ids);
    let health = api.get_json("/api/v1/engine/health").await.ok();
    let source_input_snapshot = health
        .as_ref()
        .map(|health| health["pipelines"][handles.pipeline_id]["input"].clone())
        .unwrap_or(Value::Null);
    let sink_input_snapshot = health
        .as_ref()
        .map(|health| health["pipelines"][sink_pid]["input"].clone())
        .unwrap_or(Value::Null);
    let output_state = observe_final_output(api, handles.pipeline_id, handles.output_id).await;
    let diagnostics_path = handles.work_dir.join("srt-sink-flaps-failure.json");
    let detail = json!({
        "test": "srt-sink-flaps-surface-output-instability",
        "firstFailurePhase": "initial-sink-media-ready",
        "firstFailureReason": &first_failure_reason,
        "sourcePipelineId": handles.pipeline_id,
        "sinkPipelineIds": sink_pipeline_ids,
        "outputId": handles.output_id,
        "sourceInputSnapshot": source_input_snapshot,
        "sinkInputSnapshot": sink_input_snapshot,
        "publisherExitStatus": &publisher_exit_status,
        "publisherLogPath": handles.publisher_log.display().to_string(),
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
        "sourcePipelineId": handles.pipeline_id,
        "sinkPipelineIds": sink_pipeline_ids,
        "outputId": handles.output_id,
        "publisherExitStatus": &publisher_exit_status,
        "publisherLogPath": handles.publisher_log.display().to_string(),
        "failureDiagnostics": diagnostics_path.display().to_string(),
    });
    stop_mixed_outputs(api, handles.pipeline_id, &[handles.output_id.to_string()]).await;
    stop_child(pub_child).await;
    let _ = delete_pipeline_v1(api, sink_pid).await;
    Ok(result)
}

/// Run one sink loss/recreate cycle and wait for retry, media-ready, recovery.
async fn srt_flap_cycle(
    api: &RampApi,
    handles: &SrtFlapHandles<'_>,
    sink_pid: &mut String,
    sink_pipeline_ids: &mut Vec<String>,
    name: &str,
) -> Result<(SrtFlapRetry, Result<Value, String>, bool), String> {
    delete_pipeline_v1(api, sink_pid).await?;
    let retry = wait_for_output_retry_observation(
        api,
        handles.pipeline_id,
        handles.output_id,
        Duration::from_secs(12),
    )
    .await;

    *sink_pid = create_pipeline_with_stream_key(api, name, handles.sink_stream_key).await?;
    sink_pipeline_ids.push(sink_pid.clone());
    let recovery_ready =
        wait_for_api_input_media_ready(api, sink_pid, Duration::from_secs(25)).await;
    let recovered = wait_for_output_running(
        api,
        handles.pipeline_id,
        handles.output_id,
        Duration::from_secs(25),
    )
    .await;
    let retry_row = SrtFlapRetry {
        status_visible: retry.status_visible,
        health_visible: retry.health_visible,
        has_error: retry.has_error,
        failure_phase: retry.failure_phase,
        last_error: retry.last_error,
        attempts: retry.attempts,
        backoff_ms: retry.backoff_ms,
    };
    Ok((retry_row, recovery_ready, recovered))
}

/// Write the owner-timeline diagnostics for a failed sink-flap run.
fn write_srt_flap_failure(
    handles: &SrtFlapHandles<'_>,
    cycles: &SrtFlapCycles,
    sink_pipeline_ids: &[String],
    final_output: &SrtFlapFinal,
    publisher_exit_status: &str,
    diagnostics_path: &Path,
) {
    let publisher_stderr = read_log_or_error(handles.publisher_log);
    let owner_timeline =
        srt_owner_timeline(handles.restream_log, handles.pipeline_id, sink_pipeline_ids);
    let first_recovery_ready_error = cycles.first_recovery_ready.as_ref().err();
    let second_recovery_ready_error = cycles.second_recovery_ready.as_ref().err();
    let detail = json!({
        "test": "srt-sink-flaps-surface-output-instability",
        "sourcePipelineId": handles.pipeline_id,
        "sinkPipelineIds": sink_pipeline_ids,
        "outputId": handles.output_id,
        "firstRecoveryReady": cycles.first_recovery_ready.is_ok(),
        "firstRecoveryReadyError": first_recovery_ready_error,
        "firstRecovered": cycles.first_recovered,
        "secondRecoveryReady": cycles.second_recovery_ready.is_ok(),
        "secondRecoveryReadyError": second_recovery_ready_error,
        "secondRecovered": cycles.second_recovered,
        "firstRetry": {
            "statusVisible": cycles.first_retry.status_visible,
            "healthVisible": cycles.first_retry.health_visible,
            "hasError": cycles.first_retry.has_error,
            "failurePhase": &cycles.first_retry.failure_phase,
            "lastError": &cycles.first_retry.last_error,
            "attempts": cycles.first_retry.attempts,
            "backoffMs": cycles.first_retry.backoff_ms,
        },
        "secondRetry": {
            "statusVisible": cycles.second_retry.status_visible,
            "healthVisible": cycles.second_retry.health_visible,
            "hasError": cycles.second_retry.has_error,
            "failurePhase": &cycles.second_retry.failure_phase,
            "lastError": &cycles.second_retry.last_error,
            "attempts": cycles.second_retry.attempts,
            "backoffMs": cycles.second_retry.backoff_ms,
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
        "publisherExitStatus": publisher_exit_status,
        "publisherLogPath": handles.publisher_log.display().to_string(),
        "publisherStderr": publisher_stderr,
        "ownerTimeline": owner_timeline,
    });
    write_failure_diagnostic(diagnostics_path, "SRT sink flap failure", &detail);
}

/// Decide the flap verdict, write failure diagnostics and build the row.
fn srt_flap_result(
    handles: &SrtFlapHandles<'_>,
    cycles: &SrtFlapCycles,
    sink_pipeline_ids: &[String],
    final_output: &SrtFlapFinal,
    publisher_exit_status: &str,
    diagnostics_path: &Path,
) -> Value {
    let passed = cycles.first_retry.status_visible
        && cycles.first_retry.health_visible
        && cycles.first_retry.has_error
        && cycles.first_recovery_ready.is_ok()
        && cycles.first_recovered
        && cycles.second_retry.status_visible
        && cycles.second_retry.health_visible
        && cycles.second_retry.has_error
        && cycles.second_recovery_ready.is_ok()
        && cycles.second_recovered
        && final_output.running
        && !final_output.retrying
        && final_output.error_cleared
        && final_output.recent_failure_count >= 2
        && final_output.flapping
        && final_output.health_recent_failure_count >= 2
        && final_output.health_flapping;
    if !passed {
        write_srt_flap_failure(
            handles,
            cycles,
            sink_pipeline_ids,
            final_output,
            publisher_exit_status,
            diagnostics_path,
        );
    }
    let first_recovery_ready_error = cycles.first_recovery_ready.as_ref().err();
    let second_recovery_ready_error = cycles.second_recovery_ready.as_ref().err();
    println!(
        "[fault] SRT sink flaps surface recovered-output instability: {} (firstRetrying={} secondRetrying={} firstRecovered={} secondRecovered={} firstRecoveryError={:?} secondRecoveryError={:?} publisherExit={} finalRetrying={} finalFlapping={} recentFailureCount={})",
        if passed { "PASS" } else { "FAIL" },
        cycles.first_retry.status_visible,
        cycles.second_retry.status_visible,
        cycles.first_recovered,
        cycles.second_recovered,
        first_recovery_ready_error,
        second_recovery_ready_error,
        publisher_exit_status,
        final_output.retrying,
        final_output.flapping,
        final_output.recent_failure_count,
    );
    json!({
        "test": "srt-sink-flaps-surface-output-instability",
        "passed": passed,
        "sourcePipelineId": handles.pipeline_id,
        "sinkPipelineIds": sink_pipeline_ids,
        "outputId": handles.output_id,
        "firstRetrying": cycles.first_retry.status_visible,
        "firstHealthRetrying": cycles.first_retry.health_visible,
        "firstRetryError": cycles.first_retry.has_error,
        "firstRecoveryReady": cycles.first_recovery_ready.is_ok(),
        "firstRecoveryReadyError": first_recovery_ready_error,
        "firstRecovered": cycles.first_recovered,
        "secondRetrying": cycles.second_retry.status_visible,
        "secondHealthRetrying": cycles.second_retry.health_visible,
        "secondRetryError": cycles.second_retry.has_error,
        "secondRecoveryReady": cycles.second_recovery_ready.is_ok(),
        "secondRecoveryReadyError": second_recovery_ready_error,
        "secondRecovered": cycles.second_recovered,
        "publisherExitStatus": publisher_exit_status,
        "publisherLogPath": handles.publisher_log.display().to_string(),
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
    })
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

    let handles = SrtFlapHandles {
        pipeline_id: &pid,
        sink_stream_key,
        output_id: &oid,
        restream_log: restream_log.as_path(),
        publisher_log: publisher_log.as_path(),
        work_dir,
    };

    if let Err(first_failure_reason) =
        wait_for_api_input_media_ready(api, &sink_pid, Duration::from_secs(25)).await
    {
        return srt_flap_initial_failure(
            api,
            &handles,
            &mut pub_child,
            &sink_pid,
            &sink_pipeline_ids,
            first_failure_reason,
        )
        .await;
    }

    let (first_retry, first_recovery_ready, first_recovered) = srt_flap_cycle(
        api,
        &handles,
        &mut sink_pid,
        &mut sink_pipeline_ids,
        "srt-sink-flap-target-2",
    )
    .await?;
    let (second_retry, second_recovery_ready, second_recovered) = srt_flap_cycle(
        api,
        &handles,
        &mut sink_pid,
        &mut sink_pipeline_ids,
        "srt-sink-flap-target-3",
    )
    .await?;
    let cycles = SrtFlapCycles {
        first_retry,
        first_recovery_ready,
        first_recovered,
        second_retry,
        second_recovery_ready,
        second_recovered,
    };
    let final_raw = observe_final_output(api, &pid, &oid).await;
    let final_output = SrtFlapFinal {
        running: final_raw.running,
        retrying: final_raw.retrying,
        error_cleared: final_raw.error_cleared,
        recent_failure_count: final_raw.recent_failure_count,
        flapping: final_raw.flapping,
        health_recent_failure_count: final_raw.health_recent_failure_count,
        health_flapping: final_raw.health_flapping,
        status: final_raw.status,
        health: final_raw.health,
    };
    let publisher_exit_status = publisher_exit_status(&mut pub_child);
    let diagnostics_path = work_dir.join("srt-sink-flaps-failure.json");
    let result = srt_flap_result(
        &handles,
        &cycles,
        &sink_pipeline_ids,
        &final_output,
        &publisher_exit_status,
        &diagnostics_path,
    );

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
