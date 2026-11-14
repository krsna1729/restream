//! The shared stalled RTMP destination case, usable on an already live pipeline.
use super::super::*;

pub(super) async fn rtmp_destination_stalls(
    api: &RampApi,
    pipeline: &str,
    port: u16,
) -> Result<Value, String> {
    let output = create_output(
        api,
        pipeline,
        "rtmp-stall-sink",
        &format!("rtmp://127.0.0.1:{port}/live/fault-egress-rtmp-stall-sink"),
        "source",
    )
    .await?;
    let sink = start_stalled_rtmp_sink_server(port).await?;
    // Always stop the listener, including on errors in start/status requests.
    let result = async {
        start_output(api, pipeline, &output).await?;
        let deadline = Instant::now() + Duration::from_secs(10);
        while Instant::now() < deadline && !sink.publish_accepted.load(Ordering::Relaxed) {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        if !sink.publish_accepted.load(Ordering::Relaxed) { return Err("stalled destination never accepted publish".into()); }
        let (status, health) = wait_for_output_stalled_status(api, pipeline, &output, Duration::from_secs(45)).await?;
        Ok(json!({"test": "rtmp-egress-sink-stalls", "passed": true, "publishAccepted": true, "status": status, "healthOutput": health}))
    }.await;
    stop_mixed_outputs(api, pipeline, std::slice::from_ref(&output)).await;
    stop_stalled_rtmp_sink_server(sink);
    result
}
