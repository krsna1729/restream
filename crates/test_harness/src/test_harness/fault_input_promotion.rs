use super::*;

const LONG_GOP_PREVIEW_TIMEOUT: Duration = Duration::from_secs(30);
const BUFFERED_PROMOTION_DEADLINE: Duration = Duration::from_secs(5);

async fn wait_for_input_preview(
    api: &RampApi,
    input_id: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let (status, body) = api
            .get_text_response(&format!("/hls/inputs/{input_id}/master.m3u8"))
            .await?;
        if status.is_success() && body.contains("#EXTM3U") {
            return Ok(());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(format!("input {input_id} preview did not become ready"))
}

/// Started resources for one input-promotion case.
struct InputPromotionSetup {
    pipeline_id: String,
    backup: HarnessInput,
    metrics: Arc<GeneralizedSinkMetrics>,
    sink: GeneralizedSinkServer,
    output_id: String,
    standby: Child,
    standby_before: Value,
    warm: bool,
    baseline_video: u64,
    baseline_connections: usize,
}

/// Start the pipeline, both publishers and the output, and warm the sink.
async fn start_input_promotion(
    api: &RampApi,
    ports: &TestPorts,
    fixture: &Path,
    sink_port: u16,
    timeout: Duration,
    case: &InputPromotionCase,
) -> Result<InputPromotionSetup, String> {
    let pipeline_id = create_pipeline_with_stream_key(api, &case.pipeline, &case.pipeline).await?;
    let backup = create_backup_input(api, &pipeline_id).await?;
    let metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink = start_generalized_sink_server(sink_port, metrics.clone()).await?;
    let output_id = create_output(
        api,
        &pipeline_id,
        &case.output_name,
        &format!("rtmp://127.0.0.1:{sink_port}/live/{}", case.sink_stream),
        "source",
    )
    .await?;

    let mut primary = spawn_publisher(
        fixture,
        &case.protocol.publish_url(ports, &case.pipeline),
        case.protocol.ffmpeg_format(),
        case.protocol.map_all_streams(),
    )
    .await?;
    wait_for_api_input_live(api, &pipeline_id, timeout).await?;
    let standby = spawn_long_gop_publisher(
        fixture,
        &case.protocol.publish_url(ports, &backup.stream_key),
        case.protocol.ffmpeg_format(),
    )
    .await?;
    let standby_before = wait_for_input_state(
        api,
        &pipeline_id,
        &backup.id,
        "standby",
        Duration::from_secs(20),
    )
    .await?;
    wait_for_input_preview(api, &backup.id, LONG_GOP_PREVIEW_TIMEOUT).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    start_output(api, &pipeline_id, &output_id).await?;
    let warm = wait_for_sink_video_above(&metrics, RECOVERY_WARM_VIDEO_MIN - 1, timeout).await;
    let baseline_video = metrics.video_count.load(Ordering::Relaxed);
    let baseline_connections = metrics.connections.load(Ordering::Relaxed);

    stop_child(&mut primary).await;
    Ok(InputPromotionSetup {
        pipeline_id,
        backup,
        metrics,
        sink,
        output_id,
        standby,
        standby_before,
        warm,
        baseline_video,
        baseline_connections,
    })
}

/// The promote poll's settlement flags, plus when the poll started.
struct PromotionObservation {
    promotion_started: Instant,
    progressed: bool,
    saw_retrying: bool,
    saw_missing: bool,
    saw_non_running: bool,
}

/// Promote the standby input and poll until the output progresses or settles.
async fn promote_standby_input(
    api: &RampApi,
    pipeline_id: &str,
    backup_id: &str,
    output_id: &str,
    metrics: &GeneralizedSinkMetrics,
    baseline_video: u64,
) -> Result<(Value, PromotionObservation), String> {
    let promotion = api
        .post_empty(&format!(
            "/api/v1/pipelines/{pipeline_id}/inputs/{backup_id}/promote"
        ))
        .await?;

    let promotion_started = Instant::now();
    let deadline = promotion_started + BUFFERED_PROMOTION_DEADLINE;
    let mut progressed = false;
    let mut saw_retrying = false;
    let mut saw_missing = false;
    let mut saw_non_running = false;
    while Instant::now() < deadline {
        match api.get_output_status(pipeline_id, output_id).await {
            Ok((status, _)) => {
                saw_retrying |= status.retrying;
                saw_non_running |= status.status != "running";
            }
            Err(_) => saw_missing = true,
        }
        if metrics.video_count.load(Ordering::Relaxed) > baseline_video + 10 {
            progressed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Ok((
        promotion,
        PromotionObservation {
            promotion_started,
            progressed,
            saw_retrying,
            saw_missing,
            saw_non_running,
        },
    ))
}

pub(crate) async fn run_input_promotion_case(
    api: &RampApi,
    ports: &TestPorts,
    fixture: &Path,
    sink_port: u16,
    timeout: Duration,
    case: &InputPromotionCase,
) -> Result<Value, String> {
    let mut setup = start_input_promotion(api, ports, fixture, sink_port, timeout, case).await?;
    let (promotion, observation) = promote_standby_input(
        api,
        &setup.pipeline_id,
        &setup.backup.id,
        &setup.output_id,
        &setup.metrics,
        setup.baseline_video,
    )
    .await?;

    let standby_after = wait_for_input_state(
        api,
        &setup.pipeline_id,
        &setup.backup.id,
        "active",
        Duration::from_secs(5),
    )
    .await?;
    let inputs = api
        .get_json(&format!("/api/v1/pipelines/{}/inputs", setup.pipeline_id))
        .await?;
    let final_status = api
        .get_output_status(&setup.pipeline_id, &setup.output_id)
        .await
        .ok();
    let final_connections = setup.metrics.connections.load(Ordering::Relaxed);
    let promotion_elapsed_ms = observation.promotion_started.elapsed().as_millis() as u64;
    let passed = setup.warm
        && setup.standby_before["runtime"]["forwardingState"] == "standby"
        && promotion["connected"] == true
        && standby_after["selected"] == true
        && inputs["selectedInputId"].as_str() == Some(setup.backup.id.as_str())
        && observation.progressed
        && setup.baseline_connections == 1
        && final_connections == setup.baseline_connections
        && !observation.saw_retrying
        && !observation.saw_missing
        && !observation.saw_non_running
        && final_status
            .as_ref()
            .is_some_and(|(status, _)| status.status == "running" && !status.retrying);

    println!(
        "[fault] {}: {} (progressed={} connections={} retrying={} missing={} nonRunning={})",
        case.test_name,
        if passed { "PASS" } else { "FAIL" },
        observation.progressed,
        final_connections,
        observation.saw_retrying,
        observation.saw_missing,
        observation.saw_non_running,
    );

    stop_mixed_outputs(
        api,
        &setup.pipeline_id,
        std::slice::from_ref(&setup.output_id),
    )
    .await;
    stop_child(&mut setup.standby).await;
    stop_generalized_sink_server(setup.sink);

    Ok(json!({
        "test": case.test_name,
        "passed": passed,
        "protocol": case.protocol.as_str(),
        "standbyPreviewReady": true,
        "baselineVideo": setup.baseline_video,
        "progressed": observation.progressed,
        "promotionElapsedMs": promotion_elapsed_ms,
        "promotionDeadlineMs": 5_000,
        "baselineConnections": setup.baseline_connections,
        "finalConnections": final_connections,
        "sawRetrying": observation.saw_retrying,
        "sawMissing": observation.saw_missing,
        "sawNonRunning": observation.saw_non_running,
        "selectedInputId": inputs["selectedInputId"],
        "promotedInput": standby_after,
    }))
}
