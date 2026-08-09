use super::*;

/// Recording start is intentionally backdated: `Reader::new_with_keyframe_preroll`
/// (src/media/recording/mod.rs) snaps to the most recently buffered keyframe
/// (up to ~one GOP old) and then backs up a further fixed preroll, so decoding
/// starts cleanly on an IDR. This is correct, by-design behavior — do not
/// shrink this budget to "fix" a duration assertion.
const FILE_LIVE_EDGE_START_KEYFRAME_PREROLL_DRIFT_SECS: f64 = 1.0;

/// The recording drain loop (src/media/recording/mod.rs) checks cancellation
/// between bursts, so a stop request is delayed by at most one in-flight
/// `MEDIA_PULL_BURST_PACKETS` burst plus ordinary scheduler jitter under CI
/// contention. Before that loop was made cooperative with cancellation,
/// CI overruns from an unbounded backlog drain ranged up to 4.02s; this
/// budget covers the bounded post-fix case with margin while staying tight
/// enough to catch a regression back to unbounded draining.
const FILE_LIVE_EDGE_STOP_DRAIN_BOUND_DRIFT_SECS: f64 = 1.5;

const FILE_LIVE_EDGE_MIN_DURATION_DRIFT_SECS: f64 = 1.5;

pub(crate) fn file_live_edge_max_duration_drift_secs(target_gop_seconds: u32) -> f64 {
    let budget = target_gop_seconds as f64
        + FILE_LIVE_EDGE_START_KEYFRAME_PREROLL_DRIFT_SECS
        + FILE_LIVE_EDGE_STOP_DRAIN_BOUND_DRIFT_SECS;
    FILE_LIVE_EDGE_MIN_DURATION_DRIFT_SECS.max(budget)
}

pub(crate) async fn wait_for_api_recording_state(
    api: &RampApi,
    pipeline_id: &str,
    expected_active: bool,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let health = api.get_json("/api/v1/engine/health").await?;
        let recording = &health["pipelines"][pipeline_id]["recording"];
        let enabled = recording["enabled"].as_bool().unwrap_or(false);
        let active = recording["active"].as_bool().unwrap_or(false);
        if active == expected_active {
            return Ok(json!({
                "enabled": enabled,
                "active": active,
            }));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "recording state for pipeline {pipeline_id} did not reach active={expected_active}; enabled={enabled} active={active}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(crate) async fn wait_for_api_hls_preview_state(
    api: &RampApi,
    pipeline_id: &str,
    expected_active: bool,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let health = api.get_json("/api/v1/engine/health").await?;
        let preview = &health["pipelines"][pipeline_id]["hlsPreview"];
        let active = preview["active"].as_bool().unwrap_or(false);
        if active == expected_active {
            return Ok(preview.clone());
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "HLS preview state for pipeline {pipeline_id} did not reach active={expected_active}; preview={preview}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(crate) async fn wait_for_pipeline_file_ingest_running_state(
    api: &RampApi,
    pipeline_id: &str,
    expected_running: bool,
    timeout: Duration,
) -> Result<Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        let ingest = api
            .get_json(&format!("/api/v1/pipelines/{pipeline_id}/file-ingest"))
            .await?;
        let running = ingest["running"].as_bool().unwrap_or(false);
        if running == expected_running {
            return Ok(ingest);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "file ingest state for pipeline {pipeline_id} did not reach running={expected_running}; ingest={ingest}"
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

pub(crate) async fn wait_for_hls_playlist_ready(
    api: &RampApi,
    pipeline_id: &str,
    timeout: Duration,
) -> Result<(reqwest::StatusCode, String), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let (status, body) = api
            .get_text_response(&format!("/hls/{pipeline_id}/master.m3u8"))
            .await?;
        if status.is_success() && body.contains("#EXTM3U") {
            return Ok((status, body));
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "HLS playlist for pipeline {pipeline_id} did not become ready within {}s; last_status={} body={body}",
                timeout.as_secs(),
                status
            ));
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }
}

#[derive(Clone, Debug)]
struct HlsPlaylistSnapshot {
    media_sequence: Option<u64>,
    last_segment: Option<String>,
}

fn parse_hls_playlist_snapshot(body: &str) -> HlsPlaylistSnapshot {
    let media_sequence = body
        .lines()
        .find_map(|line| line.strip_prefix("#EXT-X-MEDIA-SEQUENCE:"))
        .and_then(|value| value.trim().parse::<u64>().ok());
    let last_segment = body
        .lines()
        .rev()
        .find(|line| !line.trim().is_empty() && !line.starts_with('#'))
        .map(|line| line.trim().to_string());

    HlsPlaylistSnapshot {
        media_sequence,
        last_segment,
    }
}

async fn run_file_live_edge_case(
    api: &mut RampApi,
    ports: &TestPorts,
    media_dir: &Path,
    fixture: &Path,
    case_id: &str,
    live_optimized: bool,
    target_gop_seconds: u32,
) -> Result<Value, String> {
    let fixture_name = format!(
        "{case_id}-{}",
        fixture
            .file_name()
            .ok_or("fixture missing file name")?
            .to_string_lossy()
    );
    let media_dest = media_dir.join(&fixture_name);
    std::fs::copy(fixture, &media_dest).map_err(|e| e.to_string())?;

    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": case_id, "streamKey": case_id}),
        )
        .await?;
    let pipeline_id = pipeline["pipeline"]["id"]
        .as_str()
        .ok_or("pipeline create response missing pipeline.id")?
        .to_string();

    api.put_json(
        &format!("/api/v1/pipelines/{pipeline_id}/file-ingest"),
        json!({
            "filename": fixture_name,
            "loop": true,
            "liveOptimized": live_optimized,
            "targetGopSeconds": target_gop_seconds,
        }),
    )
    .await?;

    let source_analysis = api
        .get_json(&format!("/api/v1/media/{}/analysis", fixture_name))
        .await?;

    let ingest = api
        .get_json(&format!("/api/v1/pipelines/{pipeline_id}/file-ingest"))
        .await?;
    let ingest_id = ingest["id"]
        .as_str()
        .ok_or("pipeline file ingest missing id")?
        .to_string();

    api.post_empty(&format!("/api/v1/ingests/{ingest_id}/start"))
        .await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(30)).await?;
    wait_for_pipeline_file_ingest_running_state(api, &pipeline_id, true, Duration::from_secs(10))
        .await?;

    let playlist_url = format!(
        "http://127.0.0.1:{}/hls/{pipeline_id}/master.m3u8",
        ports.http
    );
    let (_playlist_status, playlist_body) =
        wait_for_hls_playlist_ready(api, &pipeline_id, Duration::from_secs(20)).await?;
    let hls_preview =
        wait_for_api_hls_preview_state(api, &pipeline_id, true, Duration::from_secs(10)).await?;
    let hls_probe = probe_dims_ramp_with_cookie(&playlist_url, api.cookie.as_deref()).await;
    let hls_progress_wait_secs = 5.0;
    let hls_playlist_progress = {
        let (_, playlist_before) = api
            .get_text_response(&format!("/hls/{pipeline_id}/index.m3u8"))
            .await?;
        let before = parse_hls_playlist_snapshot(&playlist_before);
        tokio::time::sleep(Duration::from_secs_f64(hls_progress_wait_secs)).await;
        let (_, playlist_after) = api
            .get_text_response(&format!("/hls/{pipeline_id}/index.m3u8"))
            .await?;
        let after = parse_hls_playlist_snapshot(&playlist_after);
        let segment_changed = before.last_segment != after.last_segment;
        let media_sequence_delta = match (before.media_sequence, after.media_sequence) {
            (Some(before), Some(after)) => Some(after.saturating_sub(before)),
            _ => None,
        };
        json!({
            "passed": segment_changed,
            "waitSecs": hls_progress_wait_secs,
            "before": {
                "mediaSequence": before.media_sequence,
                "lastSegment": before.last_segment,
            },
            "after": {
                "mediaSequence": after.media_sequence,
                "lastSegment": after.last_segment,
            },
            "segmentChanged": segment_changed,
            "mediaSequenceDelta": media_sequence_delta,
        })
    };

    let before_files = media_dir_entries(media_dir)?;
    api.post_empty(&format!("/api/v1/pipelines/{pipeline_id}/recording/start"))
        .await?;
    wait_for_api_recording_state(api, &pipeline_id, true, Duration::from_secs(10)).await?;

    // capture_target_secs (8.0s) is close to the passthrough fixture's duration
    // (correctness-h264.ts, 8.021334s per ffprobe) — each capture window roughly
    // spans one file-ingest loop iteration. Ingest looping is PTS-continuous
    // (verified: no discontinuity/reset at the loop boundary), so this is a
    // coincidence, not a bug dependency.
    let capture_target_secs = 8.0;
    let recording_started = Instant::now();
    tokio::time::sleep(Duration::from_secs_f64(capture_target_secs)).await;

    api.post_empty(&format!("/api/v1/pipelines/{pipeline_id}/recording/stop"))
        .await?;
    let capture_elapsed_secs = recording_started.elapsed().as_secs_f64();
    wait_for_api_recording_state(api, &pipeline_id, false, Duration::from_secs(20)).await?;

    let recording_mp4 =
        wait_for_new_media_file(media_dir, &before_files, ".mp4", Duration::from_secs(30)).await?;
    let recorded_analysis = restream::media::file_analysis::analyze_media_file(&recording_mp4)?;

    let expected_source_ts = recording_mp4.with_extension("ts");
    let source_retained = expected_source_ts.exists();

    api.post_empty(&format!("/api/v1/ingests/{ingest_id}/stop"))
        .await?;
    wait_for_pipeline_file_ingest_running_state(api, &pipeline_id, false, Duration::from_secs(10))
        .await?;
    wait_for_api_input_off(api, &pipeline_id, Duration::from_secs(20)).await?;

    let recorded_duration_secs = recorded_analysis.duration_sec.ok_or_else(|| {
        format!(
            "recorded output {} has no duration",
            recording_mp4.display()
        )
    })?;
    let duration_delta_secs = absolute_delta_secs(recorded_duration_secs, capture_elapsed_secs);
    let max_duration_drift_secs = file_live_edge_max_duration_drift_secs(target_gop_seconds);
    let duration_ok = duration_delta_secs <= max_duration_drift_secs;
    let hls_ok = playlist_body.contains("#EXTM3U")
        && hls_probe.is_ok()
        && hls_playlist_progress["passed"] == true;
    let live_optimized_gop_ok = if live_optimized {
        recorded_analysis
            .max_keyframe_interval_sec
            .is_some_and(|value| value <= target_gop_seconds as f64 + 0.6)
    } else {
        true
    };

    Ok(json!({
        "case": case_id,
        "passed": duration_ok && hls_ok && live_optimized_gop_ok && !source_retained,
        "liveOptimized": live_optimized,
        "targetGopSeconds": target_gop_seconds,
        "captureElapsedSecs": capture_elapsed_secs,
        "recordedDurationSecs": recorded_duration_secs,
        "durationDeltaSecs": duration_delta_secs,
        "maxAllowedDurationDriftSecs": max_duration_drift_secs,
        "durationOk": duration_ok,
        "sourceAnalysis": source_analysis,
        "recordedAnalysis": recorded_analysis,
        "hlsPreview": hls_preview,
        "hlsPlaylistReady": playlist_body.contains("#EXTM3U"),
        "hlsProbe": match hls_probe {
            Ok(dimensions) => json!({"passed": true, "dimensions": dimensions}),
            Err(error) => json!({"passed": false, "error": error}),
        },
        "hlsPlaylistProgress": hls_playlist_progress,
        "liveOptimizedGopOk": live_optimized_gop_ok,
        "sourceRetained": source_retained,
        "recordingFile": recording_mp4,
    }))
}

pub(crate) async fn file_live_edge() -> Result<Value, String> {
    let work_dir = artifact_path("file.live-edge");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let media_dir = work_dir.join("media");
    std::fs::create_dir_all(&media_dir).map_err(|e| e.to_string())?;

    let ports = TestPorts::from_env();
    let mut child =
        start_restream_child_in_media_dir(&restream_bin, &ports, &db_path, &log_path, &media_dir)
            .await?;
    let mut api = login_api(&ports).await?;

    let passthrough = run_file_live_edge_case(
        &mut api,
        &ports,
        &media_dir,
        &checked_h264_fixture()?,
        "file-live-edge-passthrough",
        false,
        2,
    )
    .await?;

    let live_optimized = run_file_live_edge_case(
        &mut api,
        &ports,
        &media_dir,
        &restream::test_fixtures::sparse_gop_mp4_fixture()?,
        "file-live-edge-optimized",
        true,
        2,
    )
    .await?;

    stop_child(&mut child).await;

    let cases = vec![passthrough, live_optimized];
    let passed = cases.iter().all(|case| case["passed"] == true);
    let results = json!({
        "mode": "file.live-edge",
        "passed": passed,
        "cases": cases,
        "mediaDir": media_dir,
        "logPath": log_path,
    });
    if passed {
        Ok(results)
    } else {
        Err(format!("file.live-edge: not all cases passed: {results}"))
    }
}

pub(crate) async fn signal_control() -> Result<Value, String> {
    let work_dir = artifact_path("signal.control");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;
    let env = MixedEnv::from_env_with_default_work_dir("signal.control", work_dir.clone());
    let duration = env.av_signal_seconds;
    let cases = [
        ("h264-single-source", "h264", false, false),
        ("h264-single-720p", "h264", false, true),
        ("h265-single-source", "h265", false, false),
        ("h265-single-720p", "h265", false, true),
        ("h264-multi-source", "h264", true, false),
        ("h265-multi-source", "h265", true, false),
    ];
    let mut results = Vec::new();
    for (name, codec, multi_audio, transcode_720p) in cases {
        let fixture = restream::test_fixtures::av_marker_transport_fixture(codec, multi_audio)?;
        let capture_path = work_dir.join(format!("{name}.signal.mkv"));
        ffmpeg_control_capture(&fixture, &capture_path, duration, transcode_720p).await?;
        let started = Instant::now();
        validate_signal_capture_artifact(
            &env,
            "signal.control",
            &format!("SC-{name}"),
            name,
            &fixture.to_string_lossy(),
            &capture_path,
            duration,
            started,
        )
        .await?;
        results.push(json!({
            "name": name,
            "fixture": fixture,
            "capture": capture_path,
            "transcode720p": transcode_720p,
            "passed": true,
        }));
    }
    Ok(json!({
        "mode": "signal.control",
        "passed": true,
        "durationSecs": duration,
        "workDir": work_dir,
        "cases": results,
    }))
}

pub(crate) async fn ffmpeg_control_capture(
    fixture: &Path,
    capture_path: &Path,
    duration: u64,
    transcode_720p: bool,
) -> Result<(), String> {
    let duration_s = duration.to_string();
    let fixture_s = fixture.to_string_lossy().to_string();
    let mut command = Command::new("ffmpeg");
    command.args([
        "-y",
        "-nostdin",
        "-hide_banner",
        "-v",
        "warning",
        "-stream_loop",
        "-1",
        "-i",
        &fixture_s,
        "-t",
        &duration_s,
        "-map",
        "0:v:0",
        "-map",
        "0:a:0",
    ]);
    if transcode_720p {
        command.args([
            "-vf",
            "scale=1280:720",
            "-c:v",
            "libx264",
            "-preset",
            "veryfast",
            "-g",
            "60",
            "-c:a",
            "copy",
        ]);
    } else {
        command.args(["-c", "copy"]);
    }
    command.args(["-f", "matroska"]).arg(capture_path);
    let child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let output = tokio::time::timeout(Duration::from_secs(duration + 60), child.wait_with_output())
        .await
        .map_err(|_| format!("signal control capture timed out: {}", fixture.display()))?
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "signal control capture failed for {}: {}",
            fixture.display(),
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}
