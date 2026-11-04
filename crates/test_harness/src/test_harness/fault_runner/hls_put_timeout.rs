//! A hung HLS PUT sink: uploads time out, the output retries, and it
//! recovers after the sink is replaced by a responsive one.

use super::super::*;

/// Starts an HLS PUT output against a sink that never answers. Expects a
/// visible timeout on an upload phase, then playlist and segment uploads with
/// correct content types, and a running output once a real sink returns.
pub(crate) async fn hls_put_timeout_recovers_after_restart(
    api: &mut RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    hls_put_port: u16,
    timeout: Duration,
) -> Result<Value, String> {
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

    let retry = wait_for_output_retry_observation(api, &pid, &oid, Duration::from_secs(20)).await;

    hang_cancel.cancel();
    let _ = hang_handle.await;

    let (sink_cancel, sink_handle) = start_hls_put_sink(hls_put_port, sink_dir.clone()).await?;
    let artifacts = wait_for_hls_put_artifacts(&sink_dir, Duration::from_secs(30)).await;
    let requests = read_hls_put_requests(&sink_dir).ok();
    let content_types_ok = requests.as_ref().is_some_and(|requests| {
        request_seen(requests, |r| {
            r["file"] == "out.m3u8" && r["contentType"] == "application/vnd.apple.mpegurl"
        }) && request_seen(requests, |r| {
            r["file"].as_str().is_some_and(is_segment_file) && r["contentType"] == "video/mp2t"
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
    let result = json!({
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
    });

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
    stop_child(&mut pub_child).await;
    sink_cancel.cancel();
    let _ = sink_handle.await;

    Ok(result)
}
