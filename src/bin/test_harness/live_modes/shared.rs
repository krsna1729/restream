use super::*;

pub(crate) async fn run_burst_graph_check(
    api: &RampApi,
    pipeline_id: &str,
) -> Result<(bool, Value), String> {
    let graph = api
        .get_json(&format!("/api/v1/pipelines/{pipeline_id}/graph"))
        .await?;
    let readers = graph_ring_readers(&graph);
    let burst_ok = readers
        .iter()
        .filter(|r| {
            r["burstCount"].as_u64().unwrap_or(0) > 0
                && r["avgBurstSize"].as_f64().unwrap_or(0.0) > 0.0
        })
        .count();
    let passed = !readers.is_empty() && burst_ok == readers.len();
    let summary = json!({
        "readerCount": readers.len(),
        "burstOk": burst_ok,
    });
    Ok((passed, summary))
}

pub(crate) async fn wait_for_api_input_live(
    api: &RampApi,
    pipeline_id: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    let started = Instant::now();
    let mut next_log = started + Duration::from_secs(10);
    println!(
        "[harness-progress] input-live start pipeline={pipeline_id} timeout={}s",
        timeout.as_secs()
    );
    loop {
        if let Ok(health) = api.get_json("/api/v1/engine/health").await
            && health["pipelines"][pipeline_id]["input"]["status"] == "on"
            && health["pipelines"][pipeline_id]["input"]["bytesReceived"]
                .as_u64()
                .unwrap_or(0)
                > 0
        {
            println!(
                "[harness-progress] input-live pass pipeline={pipeline_id} elapsed={}s",
                started.elapsed().as_secs()
            );
            return Ok(());
        }
        if Instant::now() >= next_log {
            println!(
                "[harness-progress] input-live wait pipeline={pipeline_id} elapsed={}s remaining={}s",
                started.elapsed().as_secs(),
                deadline.saturating_duration_since(Instant::now()).as_secs()
            );
            next_log += Duration::from_secs(10);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{pipeline_id}: ingest did not go live within {}s",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub(crate) async fn wait_for_api_input_media_ready(
    api: &RampApi,
    pipeline_id: &str,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let started = Instant::now();
    let mut next_log = started + Duration::from_secs(10);
    let mut last_snapshot = Value::Null;
    println!(
        "[harness-progress] input-media-ready start pipeline={pipeline_id} timeout={}s",
        timeout.as_secs()
    );

    loop {
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            let snapshot = health["pipelines"][pipeline_id].clone();
            if !snapshot.is_null() {
                last_snapshot = snapshot.clone();
                let input = &snapshot["input"];
                let input_live =
                    input["status"] == "on" && input["bytesReceived"].as_u64().unwrap_or(0) > 0;
                let has_video = !input["video"].is_null();
                let has_audio = input["audioTracks"]
                    .as_array()
                    .map(|tracks| !tracks.is_empty())
                    .unwrap_or(false);
                if input_live && has_video && has_audio {
                    println!(
                        "[harness-progress] input-media-ready pass pipeline={pipeline_id} elapsed={}s",
                        started.elapsed().as_secs()
                    );
                    return Ok(snapshot);
                }
            }
        }
        if Instant::now() >= next_log {
            let input = &last_snapshot["input"];
            println!(
                "[harness-progress] input-media-ready wait pipeline={pipeline_id} elapsed={}s remaining={}s status={} bytes={} video={} audioTracks={}",
                started.elapsed().as_secs(),
                deadline.saturating_duration_since(Instant::now()).as_secs(),
                input["status"].as_str().unwrap_or("unknown"),
                input["bytesReceived"].as_u64().unwrap_or(0),
                !input["video"].is_null(),
                input["audioTracks"]
                    .as_array()
                    .map(|tracks| tracks.len())
                    .unwrap_or(0)
            );
            next_log += Duration::from_secs(10);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{pipeline_id}: ingest went live but media probe was incomplete within {}s; last snapshot={}",
                timeout.as_secs(),
                last_snapshot
            ));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub(crate) async fn wait_for_output_stalled_status(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
    timeout: Duration,
) -> Result<(Value, Value), String> {
    let deadline = Instant::now() + timeout;
    let mut last_status = Value::Null;
    let mut last_health = Value::Null;

    loop {
        if let Ok((status_row, status)) = api.get_output_status(pipeline_id, output_id).await {
            last_status = status.clone();
            if let Ok(health) = api.get_json("/api/v1/engine/health").await
                && let Some(output) = health["pipelines"][pipeline_id]["outputs"]
                    .as_object()
                    .and_then(|outputs| outputs.get(output_id).cloned())
            {
                last_health = output.clone();
                let health_row = ApiOutputStatus::from_value(output_id, &output)?;
                let stalled_visible = status_row.status == "stalled"
                    && health_row.status == "stalled"
                    && status_row.raw_status == "running"
                    && health_row.raw_status == "running"
                    && !status_row.retrying
                    && !health_row.retrying
                    && status_row.last_error.is_none()
                    && health_row.last_error.is_none()
                    && status_row.failure_phase.is_none()
                    && health_row.failure_phase.is_none()
                    && status_row.started_at.is_some()
                    && health_row.started_at == status_row.started_at
                    && health_row.target_addr == status_row.target_addr
                    && health_row.total_size == status_row.total_size;
                let stale_age_visible = match status_row.last_progress_age_ms {
                    Some(age_ms) => age_ms >= 10_000,
                    None => status["lastProgressAt"].is_null(),
                };
                if stalled_visible && stale_age_visible {
                    return Ok((status, output));
                }
            }
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "{pipeline_id}/{output_id}: output status did not surface stalled state within {}s; last_status={} last_health={}",
                timeout.as_secs(),
                last_status,
                last_health
            ));
        }

        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

pub(crate) async fn wait_for_api_input_off(
    api: &RampApi,
    pipeline_id: &str,
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(health) = api.get_json("/api/v1/engine/health").await {
            let status = health["pipelines"][pipeline_id]["input"]["status"]
                .as_str()
                .unwrap_or("unknown");
            if status == "off" {
                return Ok(());
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "{pipeline_id}: ingest did not go off within {}s",
                timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(crate) async fn probe_dims_ramp(url: &str) -> Result<String, String> {
    probe_dims_ramp_with_cookie(url, None).await
}

pub(crate) async fn probe_dims_ramp_with_cookie(
    url: &str,
    cookie: Option<&str>,
) -> Result<String, String> {
    let mut command = Command::new("ffprobe");
    command.args([
        "-v",
        "error",
        "-probesize",
        "10000000",
        "-analyzeduration",
        "10000000",
        "-select_streams",
        "v:0",
        "-show_entries",
        "stream=width,height",
        "-of",
        "csv=p=0",
    ]);
    if let Some(cookie) = cookie {
        command.args(["-headers", &format!("Cookie: {cookie}\r\n")]);
    }
    let child = command
        .arg(url)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let output = tokio::time::timeout(Duration::from_secs(20), child.wait_with_output())
        .await
        .map_err(|_| format!("ffprobe timed out: {url}"))?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(format!("ffprobe failed: {url}: {}", stderr.trim()));
    }
    Ok(String::from_utf8_lossy(&output.stdout)
        .lines()
        .next()
        .unwrap_or("")
        .trim()
        .replace(',', "x"))
}

pub(crate) fn probe_audio_track_count(probe: &Value) -> usize {
    probe["streams"]
        .as_array()
        .map(|streams| {
            streams
                .iter()
                .filter(|s| s["codec_type"] == "audio")
                .count()
        })
        .unwrap_or(0)
}

pub(crate) fn video_dimensions(probe: &Value) -> Option<String> {
    let stream = probe["streams"]
        .as_array()?
        .iter()
        .find(|stream| stream["codec_type"] == "video")?;
    Some(format!(
        "{}x{}",
        stream["width"].as_i64()?,
        stream["height"].as_i64()?
    ))
}

fn video_codec_name(probe: &Value) -> Option<String> {
    probe["streams"]
        .as_array()?
        .iter()
        .find(|stream| stream["codec_type"] == "video")?["codec_name"]
        .as_str()
        .map(str::to_string)
}

pub(crate) fn graph_ring_readers(graph: &Value) -> Vec<Value> {
    graph["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["type"] == "ring_buffer")
        .flat_map(|node| {
            node["details"]["readers"]
                .as_array()
                .cloned()
                .unwrap_or_default()
        })
        .collect()
}

pub(crate) fn graph_active_node_count(graph: &Value, node_type: &str) -> usize {
    graph["nodes"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|node| node["type"] == node_type && node["active"].as_bool().unwrap_or(false))
        .count()
}

pub(crate) async fn wait_for_probe_shape(
    label: &str,
    url: &str,
    expected_dimensions: Option<&str>,
    expected_video_codec: &str,
    expected_audio_tracks: usize,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    let mut last_probe = json!({});
    let mut last_error = String::new();
    loop {
        match ffprobe(url).await {
            Ok(probe) => {
                let dimensions = video_dimensions(&probe).unwrap_or_default();
                let codec = video_codec_name(&probe).unwrap_or_default();
                let audio_tracks = probe_audio_track_count(&probe);
                let dimensions_ok =
                    expected_dimensions.is_none_or(|expected| dimensions == expected);
                if dimensions_ok
                    && codec == expected_video_codec
                    && audio_tracks == expected_audio_tracks
                {
                    return Ok(probe);
                }
                last_probe = json!({
                    "dimensions": dimensions,
                    "videoCodec": codec,
                    "audioTracks": audio_tracks,
                    "probe": probe,
                });
            }
            Err(error) => {
                last_error = error;
            }
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "{label}: expected codec={expected_video_codec} audio_tracks={expected_audio_tracks} dimensions={:?}; last_probe={last_probe}; last_error={last_error}",
                expected_dimensions
            ));
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
}
