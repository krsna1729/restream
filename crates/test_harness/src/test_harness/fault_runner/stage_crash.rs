//! A transcoder stage whose FFmpeg dies mid-stream: the outputs reading it
//! end their attempt, retry, and deliver again from a new stage.

use super::super::*;

/// Kills the external FFmpeg of a 720p stage feeding an RTMP output and
/// expects the output to recover: delivery resumes past the baseline, the
/// stage runs under a new process, and the output reports running without a
/// retry left pending.
pub(crate) async fn transcoder_crash_recovers(
    api: &mut RampApi,
    ports: &TestPorts,
    fixture_h264: &Path,
    sink_port: u16,
) -> Result<Value, String> {
    let pid = create_pipeline(api, "fault-stage-crash").await?;
    let metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server = start_generalized_sink_server(sink_port, metrics.clone()).await?;
    let oid = create_output(
        api,
        &pid,
        "stage-crash-720p",
        &format!("rtmp://127.0.0.1:{sink_port}/live/fault-stage-crash"),
        "720p",
    )
    .await?;
    let mut pub_child = spawn_publisher(
        fixture_h264,
        &format!("rtmp://127.0.0.1:{}/live/fault-stage-crash", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(api, &pid, Duration::from_secs(15)).await?;
    start_output(api, &pid, &oid).await?;
    let warmed = wait_for_sink_video_above(
        &metrics,
        RECOVERY_WARM_VIDEO_MIN - 1,
        Duration::from_secs(30),
    )
    .await;

    let killed_pid = stage_backend_pid(api, &pid).await;
    if let Some(stage_pid) = killed_pid {
        // SIGKILL: the stage gets no chance to shut down cleanly.
        let _ = std::process::Command::new("kill")
            .args(["-9", &stage_pid.to_string()])
            .status();
    }
    tokio::time::sleep(Duration::from_secs(2)).await;
    let baseline_video = metrics.video_count.load(Ordering::Relaxed);

    let recovered = wait_for_output_running_and_sink_video_above(
        api,
        &pid,
        &oid,
        &metrics,
        baseline_video + RECOVERY_WARM_VIDEO_MIN,
        Duration::from_secs(45),
    )
    .await;
    let new_pid = stage_backend_pid(api, &pid).await;
    let final_output = observe_final_output(api, &pid, &oid).await;
    let passed = warmed
        && killed_pid.is_some()
        && recovered
        && new_pid.is_some()
        && new_pid != killed_pid
        && final_output.running
        && !final_output.retrying;
    println!(
        "[fault] Transcoder stage crash recovers: {} (warmed={warmed} killedPid={killed_pid:?} newPid={new_pid:?} recovered={recovered} baselineVideo={baseline_video} finalVideo={})",
        if passed { "PASS" } else { "FAIL" },
        metrics.video_count.load(Ordering::Relaxed),
    );
    let result = json!({
        "test": "transcoder-stage-crash-recovers",
        "passed": passed,
        "warmed": warmed,
        "killedStagePid": killed_pid,
        "newStagePid": new_pid,
        "recovered": recovered,
        "baselineVideo": baseline_video,
        "finalVideo": metrics.video_count.load(Ordering::Relaxed),
        "finalStatusRunning": final_output.running,
        "finalRetrying": final_output.retrying,
        "finalStatus": final_output.status,
    });

    stop_mixed_outputs(api, &pid, std::slice::from_ref(&oid)).await;
    stop_child(&mut pub_child).await;
    stop_generalized_sink_server(sink_server);
    Ok(result)
}

/// The OS pid of the pipeline's running transcoder stage, if one runs.
async fn stage_backend_pid(api: &RampApi, pipeline_id: &str) -> Option<u64> {
    let telemetry = api.get_json("/api/v1/engine/telemetry").await.ok()?;
    telemetry["stages"].as_array()?.iter().find_map(|stage| {
        (stage["pipelineId"] == pipeline_id)
            .then(|| stage["lifecycle"]["backendPid"].as_u64())
            .flatten()
    })
}
