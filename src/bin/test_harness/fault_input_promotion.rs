use super::*;

pub(crate) struct HarnessInput {
    pub(crate) id: String,
    pub(crate) stream_key: String,
}

pub(crate) async fn create_backup_input(
    api: &RampApi,
    pipeline_id: &str,
) -> Result<HarnessInput, String> {
    let response = api
        .post_json(
            &format!("/api/v1/pipelines/{pipeline_id}/inputs"),
            json!({"label": "Warm standby"}),
        )
        .await?;
    let input = &response["input"];
    Ok(HarnessInput {
        id: input["id"]
            .as_str()
            .ok_or_else(|| "created backup input is missing id".to_string())?
            .to_string(),
        stream_key: input["streamKey"]
            .as_str()
            .ok_or_else(|| "created backup input is missing stream key".to_string())?
            .to_string(),
    })
}

pub(crate) async fn wait_for_input_state(
    api: &RampApi,
    pipeline_id: &str,
    input_id: &str,
    state: &str,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    while Instant::now() < deadline {
        let response = api
            .get_json(&format!("/api/v1/pipelines/{pipeline_id}/inputs"))
            .await?;
        if let Some(input) = response["inputs"].as_array().and_then(|inputs| {
            inputs
                .iter()
                .find(|input| input["id"].as_str() == Some(input_id))
        }) && input["runtime"]["connected"] == true
            && input["runtime"]["forwardingState"].as_str() == Some(state)
            && input["runtime"]["bytesReceived"].as_u64().unwrap_or(0) > 0
        {
            return Ok(input.clone());
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    Err(format!(
        "input {input_id} did not reach connected {state} state"
    ))
}

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

pub(crate) async fn run_input_promotion_case(
    api: &RampApi,
    ports: &TestPorts,
    fixture: &Path,
    sink_port: u16,
    timeout: Duration,
    case: &InputPromotionCase,
) -> Result<Value, String> {
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
    let mut standby = spawn_long_gop_publisher(
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
    wait_for_input_preview(api, &backup.id, Duration::from_secs(20)).await?;
    tokio::time::sleep(Duration::from_secs(1)).await;

    start_output(api, &pipeline_id, &output_id).await?;
    let warm = wait_for_sink_video_above(&metrics, RECOVERY_WARM_VIDEO_MIN - 1, timeout).await;
    let baseline_video = metrics.video_count.load(Ordering::Relaxed);
    let baseline_connections = metrics.connections.load(Ordering::Relaxed);

    stop_child(&mut primary).await;
    let promotion = api
        .post_empty(&format!(
            "/api/v1/pipelines/{pipeline_id}/inputs/{}/promote",
            backup.id
        ))
        .await?;

    let promotion_started = Instant::now();
    let deadline = promotion_started + Duration::from_secs(5);
    let mut progressed = false;
    let mut saw_retrying = false;
    let mut saw_missing = false;
    let mut saw_non_running = false;
    while Instant::now() < deadline {
        match api.get_output_status(&pipeline_id, &output_id).await {
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

    let standby_after = wait_for_input_state(
        api,
        &pipeline_id,
        &backup.id,
        "active",
        Duration::from_secs(5),
    )
    .await?;
    let inputs = api
        .get_json(&format!("/api/v1/pipelines/{pipeline_id}/inputs"))
        .await?;
    let final_status = api.get_output_status(&pipeline_id, &output_id).await.ok();
    let final_connections = metrics.connections.load(Ordering::Relaxed);
    let promotion_elapsed_ms = promotion_started.elapsed().as_millis() as u64;
    let passed = warm
        && standby_before["runtime"]["forwardingState"] == "standby"
        && promotion["connected"] == true
        && standby_after["selected"] == true
        && inputs["selectedInputId"].as_str() == Some(backup.id.as_str())
        && progressed
        && baseline_connections == 1
        && final_connections == baseline_connections
        && !saw_retrying
        && !saw_missing
        && !saw_non_running
        && final_status
            .as_ref()
            .is_some_and(|(status, _)| status.status == "running" && !status.retrying);

    println!(
        "[fault] {}: {} (progressed={} connections={} retrying={} missing={} nonRunning={})",
        case.test_name,
        if passed { "PASS" } else { "FAIL" },
        progressed,
        final_connections,
        saw_retrying,
        saw_missing,
        saw_non_running,
    );

    stop_mixed_outputs(api, &pipeline_id, std::slice::from_ref(&output_id)).await;
    stop_child(&mut standby).await;
    stop_generalized_sink_server(sink);

    Ok(json!({
        "test": case.test_name,
        "passed": passed,
        "protocol": case.protocol.as_str(),
        "standbyPreviewReady": true,
        "baselineVideo": baseline_video,
        "progressed": progressed,
        "promotionElapsedMs": promotion_elapsed_ms,
        "promotionDeadlineMs": 5_000,
        "baselineConnections": baseline_connections,
        "finalConnections": final_connections,
        "sawRetrying": saw_retrying,
        "sawMissing": saw_missing,
        "sawNonRunning": saw_non_running,
        "selectedInputId": inputs["selectedInputId"],
        "promotedInput": standby_after,
    }))
}
