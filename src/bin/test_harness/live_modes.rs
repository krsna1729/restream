use super::*;

const FILE_LIVE_EDGE_MIN_DURATION_DRIFT_SECS: f64 = 0.75;
const FILE_LIVE_EDGE_STOP_LATENCY_DRIFT_SECS: f64 = 0.75;

pub(crate) fn file_live_edge_max_duration_drift_secs(target_gop_seconds: u32) -> f64 {
    FILE_LIVE_EDGE_MIN_DURATION_DRIFT_SECS
        .max(target_gop_seconds as f64 + FILE_LIVE_EDGE_STOP_LATENCY_DRIFT_SECS)
}

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

/// One ramp-family input/output profile.
#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct RampConfig {
    pub(crate) name: &'static str,
    ingest_proto: &'static str,
    out_proto: &'static str,
    encoding: &'static str,
}

static RAMP_CONFIGS_FROM_DSL: OnceLock<Vec<RampConfig>> = OnceLock::new();

pub(crate) fn ramp_configs() -> &'static [RampConfig] {
    RAMP_CONFIGS_FROM_DSL.get_or_init(|| {
        serde_json::from_str::<Vec<RampConfig>>(include_str!("ramp_configs.json"))
            .expect("embedded ramp_configs.json should define valid ramp rows")
    })
}

/// Runtime configuration and artifact paths for ramp-family runs.
struct RampEnv {
    work_dir: PathBuf,
    scale_log: PathBuf,
    summary_log: PathBuf,
    restream_log: PathBuf,
    mediamtx_log: PathBuf,
    mediamtx_config: PathBuf,
    restream_bin: PathBuf,
    restream_db_path: PathBuf,
    restream_http: u16,
    restream_rtmp: u16,
    restream_srt: u16,
    mtx_rtmp: u16,
    mtx_srt: u16,
    mtx_api: u16,
    n_outputs: usize,
    snap_every: usize,
    snapshot_sleep: Duration,
    cleanup_sleep: Duration,
}

impl RampEnv {
    fn from_env() -> Self {
        let work_dir = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".local/artifacts/ramp"));
        let ports = harness_port_defaults();
        Self {
            scale_log: std::env::var_os("SCALE_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("scale.csv")),
            summary_log: std::env::var_os("SUMMARY_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("summary.txt")),
            restream_log: std::env::var_os("RAMP_RESTREAM_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("restream.log")),
            mediamtx_log: std::env::var_os("RAMP_MEDIAMTX_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("mediamtx.log")),
            mediamtx_config: std::env::var_os("RAMP_MEDIAMTX_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("mediamtx.yml")),
            restream_bin: default_restream_bin(),
            restream_db_path: std::env::var_os("RESTREAM_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_work_db_path(&work_dir, "ramp.db")),
            restream_http: ports.restream_http,
            restream_rtmp: ports.restream_rtmp,
            restream_srt: ports.restream_srt,
            mtx_rtmp: ports.mtx_rtmp,
            mtx_srt: ports.mtx_srt,
            mtx_api: ports.mtx_api,
            n_outputs: env_usize("N_OUTPUTS", 10),
            snap_every: env_usize("SNAP_EVERY", 1).max(1),
            snapshot_sleep: Duration::from_secs(env_secs("SNAPSHOT_SLEEP_SECS", 3)),
            cleanup_sleep: Duration::from_secs(env_secs("RAMP_CONFIG_CLEANUP_SECS", 8)),
            work_dir,
        }
    }
}

// ── api-smoke (Phase 3) ─────────────────────────────────────────────────────
//
// Lightweight live test for the API/DB/lifecycle layer. No media — just spin up
// the binary, walk the API (auth, pipeline/output CRUD, start/stop), restart
// the child, and assert pipelines survived (DB persistence).

pub(crate) async fn ramp_family_correctness() -> Result<Value, String> {
    let env = RampEnv::from_env();
    if env.n_outputs == 0 {
        return Err("N_OUTPUTS must be greater than zero".to_string());
    }
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    ensure_ramp_artifacts(&env)?;

    let configs = selected_ramp_configs();
    if configs.is_empty() {
        return Err("RAMP_FAMILY_CONFIGS selected no ramp-family configs".to_string());
    }

    let mut mediamtx = start_ramp_mediamtx(&env).await?;
    let mut restream = start_ramp_restream(&env).await?;
    let mut api = RampApi::new(env.restream_http);
    api.login().await?;

    let mut case_results = Vec::with_capacity(configs.len());
    for config in configs {
        case_results.push(run_ramp_config(config, &env, &api, restream.id().unwrap_or(0)).await?);
    }

    stop_child(&mut restream).await;
    stop_child(&mut mediamtx).await;

    Ok(json!({
        "passed": true,
        "mode": "ramp-family",
        "configs": case_results,
        "artifacts": {
            "scaleCsv": env.scale_log,
            "summary": env.summary_log,
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
        }
    }))
}

fn selected_ramp_configs() -> Vec<RampConfig> {
    let allow = std::env::var("RAMP_FAMILY_CONFIGS").ok().map(|value| {
        value
            .split_whitespace()
            .map(str::to_string)
            .collect::<Vec<_>>()
    });
    ramp_configs()
        .iter()
        .copied()
        .filter(|config| {
            allow
                .as_ref()
                .is_none_or(|items| items.iter().any(|item| item == config.name))
        })
        .collect()
}

fn ensure_ramp_artifacts(env: &RampEnv) -> Result<(), String> {
    if !env.scale_log.exists() {
        std::fs::write(
            &env.scale_log,
            "config,step,label,cpu_pct,rss_kb,ffmpeg_n,ffmpeg_rss_kb,total_rss_kb\n",
        )
        .map_err(|e| e.to_string())?;
    }
    if !env.summary_log.exists() {
        std::fs::write(&env.summary_log, "").map_err(|e| e.to_string())?;
    }
    Ok(())
}

async fn start_ramp_restream(env: &RampEnv) -> Result<Child, String> {
    start_restream_child(
        &env.restream_bin,
        &TestPorts {
            http: env.restream_http,
            rtmp: env.restream_rtmp,
            srt: env.restream_srt,
        },
        &env.restream_db_path,
        &env.restream_log,
    )
    .await
}

pub(crate) fn cleanup_ramp_db(path: &Path) {
    let path_string = path.to_string_lossy();
    let db_path = path_string
        .strip_prefix("sqlite:")
        .unwrap_or(path_string.as_ref())
        .split('?')
        .next()
        .unwrap_or("data.db");
    let db_path = PathBuf::from(db_path);
    let _ = std::fs::remove_file(&db_path);
    let _ = std::fs::remove_file(format!("{}-shm", db_path.display()));
    let _ = std::fs::remove_file(format!("{}-wal", db_path.display()));
}

async fn start_ramp_mediamtx(env: &RampEnv) -> Result<Child, String> {
    std::fs::write(
        &env.mediamtx_config,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: no\nwebrtc: no\nmoq: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            env.mtx_rtmp, env.mtx_srt, env.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(&env.mediamtx_log).map_err(|e| e.to_string())?;
    let stderr_log = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new("mediamtx");
    let mut child = remove_mediamtx_config_env(&mut command)
        .arg(&env.mediamtx_config)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", env.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut child).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }
    Ok(child)
}

async fn start_local_mediamtx(
    config_path: &Path,
    log_path: &Path,
    ports: HarnessPortDefaults,
) -> Result<Child, String> {
    std::fs::write(
        config_path,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: yes\nhlsAddress: :{}\nhlsPartDuration: 200ms\nhlsSegmentDuration: 2s\nwebrtc: no\nmoq: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            ports.mtx_rtmp, ports.mtx_srt, ports.mtx_hls, ports.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(log_path).map_err(|e| e.to_string())?;
    let stderr_log = log.try_clone().map_err(|e| e.to_string())?;
    let mut command = Command::new("mediamtx");
    let mut child = remove_mediamtx_config_env(&mut command)
        .arg(config_path)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", ports.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut child).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }
    Ok(child)
}

async fn run_ramp_config(
    config: RampConfig,
    env: &RampEnv,
    api: &RampApi,
    restream_pid: u32,
) -> Result<Value, String> {
    println!(
        "\n[ramp-family] {} {} ingest -> {} {} x{} outputs",
        config.name, config.ingest_proto, config.out_proto, config.encoding, env.n_outputs
    );
    let stream_key = format!("sk-{}", config.name);
    let pipeline_id = create_resource_pipeline(api, config.name, &stream_key).await?;

    let mut publisher = spawn_ramp_publisher(config, env, &stream_key).await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(45)).await?;
    let baseline_snapshot = snapshot_ramp(env, restream_pid, config.name, 0, "baseline").await?;
    let rss_baseline = process_rss_kb(restream_pid).await.unwrap_or(0);

    let mut output_ids = Vec::with_capacity(env.n_outputs);
    for n in 1..=env.n_outputs {
        let url = match config.out_proto {
            "rtmp" => format!("rtmp://127.0.0.1:{}/live/{}-{n}", env.mtx_rtmp, config.name),
            "srt" => harness_srt_output_url(
                env.mtx_srt,
                &format!("{}-{n}", config.name),
                HarnessSrtMode::Publish,
            ),
            other => return Err(format!("unsupported ramp output protocol {other}")),
        };
        let output_id =
            create_output(api, &pipeline_id, &format!("out{n}"), &url, config.encoding).await?;
        start_output(api, &pipeline_id, &output_id).await?;
        output_ids.push(output_id);
        if n == 1 || n % env.snap_every == 0 {
            snapshot_ramp(env, restream_pid, config.name, n, &format!("out{n}")).await?;
        }
    }

    let rss_final = process_rss_kb(restream_pid).await.unwrap_or(0);
    let ffmpeg = ffmpeg_pipe1_stats().await;
    let rss_delta = rss_final.saturating_sub(rss_baseline);
    let per_output = rss_delta / env.n_outputs as u64;
    append_line(
        &env.summary_log,
        &format!(
            "{},rss_delta_kb={},per_output_kb={},ffmpeg_n={},ffmpeg_rss_kb={}\n",
            config.name, rss_delta, per_output, ffmpeg.count, ffmpeg.rss_kb
        ),
    )?;

    let expected = if config.encoding == "source" {
        "1920x1080"
    } else {
        "1280x720"
    };
    let first_url = read_url(config, env, 1);
    let last_url = read_url(config, env, env.n_outputs);
    let first_dims = check_ramp_stream("out1", &first_url, expected, 10).await;
    let last_dims =
        check_ramp_stream(&format!("out{}", env.n_outputs), &last_url, expected, 10).await;

    stop_child(&mut publisher).await;
    for output_id in &output_ids {
        let _ = api
            .post_null(&format!(
                "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"
            ))
            .await;
    }
    tokio::time::sleep(env.cleanup_sleep).await;

    Ok(json!({
        "config": config.name,
        "pipelineId": pipeline_id,
        "outputs": output_ids.len(),
        "baseline": baseline_snapshot,
        "rssDeltaKb": rss_delta,
        "perOutputKb": per_output,
        "ffmpegCount": ffmpeg.count,
        "ffmpegRssKb": ffmpeg.rss_kb,
        "spotChecks": {
            "first": {"expected": expected, "got": first_dims},
            "last": {"expected": expected, "got": last_dims},
        }
    }))
}

async fn spawn_ramp_publisher(
    config: RampConfig,
    env: &RampEnv,
    stream_key: &str,
) -> Result<Child, String> {
    let fixture = ramp_fixture()?;
    let (url, format) = match config.ingest_proto {
        "rtmp" => (
            format!("rtmp://127.0.0.1:{}/live/{stream_key}", env.restream_rtmp),
            "flv",
        ),
        "srt" => (
            harness_srt_ffmpeg_url(env.restream_srt, stream_key, HarnessSrtMode::Publish, None),
            "mpegts",
        ),
        other => return Err(format!("unsupported ramp ingest protocol {other}")),
    };
    spawn_publisher_with_selection(
        &fixture,
        &url,
        format,
        PublishTrackSelection::PrimaryAv,
        None,
    )
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

async fn install_bframe_transcode_profiles(api: &RampApi) -> Result<(), String> {
    let settings = api.get_json("/api/v1/settings").await?;
    let mut profiles: restream::domain::transcode_profile::TranscodeProfiles =
        serde_json::from_value(settings["transcodeProfiles"].clone())
            .map_err(|error| format!("parse transcode profiles: {error}"))?;

    for (name, bframes) in [("h264_bf0", 0usize), ("h264_bf2", 2usize)] {
        profiles.insert(
            name.to_string(),
            restream::domain::transcode_profile::TranscodeProfile {
                preset: "veryfast".to_string(),
                tune: String::new(),
                crf: 23,
                gop: 60,
                bframes,
                bitrate: 0,
                max_bitrate: 0,
                width: 0,
                height: 0,
            },
        );
    }

    api.patch_json("/api/v1/settings", json!({ "transcodeProfiles": profiles }))
        .await?;
    Ok(())
}

/// Expected presence of B-frame signal in a generated/probed stream.
#[derive(Clone, Copy)]
enum ExpectedBframeSignal {
    None,
    Present,
}

async fn run_transcode_bframe_probe_case(
    api: &RampApi,
    pipeline_id: &str,
    work_dir: &Path,
    mediamtx_rtmp_port: u16,
    label: &str,
    encoding: &str,
    expected_signal: ExpectedBframeSignal,
) -> Result<Value, String> {
    let stream_name = format!("e2e-bframe-{label}");
    let publish_url = format!("rtmp://127.0.0.1:{mediamtx_rtmp_port}/live/{stream_name}");
    let output_id = create_output(api, pipeline_id, label, &publish_url, encoding).await?;
    if let Err(error) = start_output(api, pipeline_id, &output_id).await {
        stop_mixed_outputs(api, pipeline_id, std::slice::from_ref(&output_id)).await;
        return Err(format!("{label}: start output failed: {error}"));
    }

    let probe = wait_for_probe_shape(
        label,
        &publish_url,
        None,
        "h264",
        1,
        Duration::from_secs(30),
    )
    .await;
    let packet_path = work_dir.join(format!("{label}-packets.json"));
    let packet_probe = ffprobe_video_packets(&publish_url, &packet_path).await;
    stop_mixed_outputs(api, pipeline_id, std::slice::from_ref(&output_id)).await;

    let probe = probe?;
    let packet_probe = packet_probe?;
    let packet_count = count_video_packets(&packet_probe);
    let bframe_count = count_bframe_packets(&packet_probe);
    let dts_monotone = video_dts_monotone(&packet_probe);
    let bframe_signal_ok = match expected_signal {
        ExpectedBframeSignal::None => bframe_count == 0,
        ExpectedBframeSignal::Present => bframe_count > 0,
    };
    let passed = packet_count >= 30 && dts_monotone && bframe_signal_ok;

    let mut result = json!({
        "passed": passed,
        "encoding": encoding,
        "readUrl": publish_url,
        "packetArtifact": packet_path,
        "packetCount": packet_count,
        "bframeCount": bframe_count,
        "dtsMonotone": dts_monotone,
        "expectedBframes": match expected_signal {
            ExpectedBframeSignal::None => 0,
            ExpectedBframeSignal::Present => 2,
        },
        "probe": probe,
    });
    if packet_count < 30 {
        result["error"] = json!(format!(
            "{label}: expected at least 30 video packets, got {packet_count}"
        ));
    } else if !bframe_signal_ok {
        result["error"] = match expected_signal {
            ExpectedBframeSignal::None => {
                json!(format!("{label}: expected no packets with PTS > DTS"))
            }
            ExpectedBframeSignal::Present => {
                json!(format!("{label}: expected packets with PTS > DTS"))
            }
        };
    } else if !dts_monotone {
        result["error"] = json!(format!("{label}: DTS values are not monotone"));
    }

    if passed {
        Ok(result)
    } else {
        Err(format!("{label}: transcode B-frame probe failed: {result}"))
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

/// One ramp-family resource snapshot written to CSV and JSON summaries.
struct RampSnapshot {
    cpu_pct: String,
    rss_kb: u64,
    ffmpeg_count: u64,
    ffmpeg_rss_kb: u64,
}

async fn snapshot_ramp(
    env: &RampEnv,
    restream_pid: u32,
    config: &str,
    step: usize,
    label: &str,
) -> Result<Value, String> {
    if !env.snapshot_sleep.is_zero() {
        tokio::time::sleep(env.snapshot_sleep).await;
    }
    let ffmpeg = ffmpeg_pipe1_stats().await;
    let snapshot = RampSnapshot {
        cpu_pct: process_cpu_pct(restream_pid)
            .await
            .unwrap_or_else(|| "0".to_string()),
        rss_kb: process_rss_kb(restream_pid).await.unwrap_or(0),
        ffmpeg_count: ffmpeg.count,
        ffmpeg_rss_kb: ffmpeg.rss_kb,
    };
    let total = snapshot.rss_kb + snapshot.ffmpeg_rss_kb;
    append_line(
        &env.scale_log,
        &format!(
            "{config},{step},\"{label}\",{},{},{},{},{}\n",
            snapshot.cpu_pct, snapshot.rss_kb, snapshot.ffmpeg_count, snapshot.ffmpeg_rss_kb, total
        ),
    )?;
    println!(
        "  {step:<4} {label:<20} cpu={} rss={} KB ffmpeg#={} ffmpeg_rss={} KB total={} KB",
        snapshot.cpu_pct, snapshot.rss_kb, snapshot.ffmpeg_count, snapshot.ffmpeg_rss_kb, total
    );
    Ok(json!({
        "step": step,
        "label": label,
        "cpuPct": snapshot.cpu_pct,
        "rssKb": snapshot.rss_kb,
        "ffmpegCount": snapshot.ffmpeg_count,
        "ffmpegRssKb": snapshot.ffmpeg_rss_kb,
        "totalRssKb": total,
    }))
}

fn read_url(config: RampConfig, env: &RampEnv, output_index: usize) -> String {
    match config.out_proto {
        "rtmp" => format!(
            "rtmp://127.0.0.1:{}/live/{}-{output_index}",
            env.mtx_rtmp, config.name
        ),
        "srt" => harness_srt_output_url(
            env.mtx_srt,
            &format!("{}-{output_index}", config.name),
            HarnessSrtMode::Read,
        ),
        _ => String::new(),
    }
}

async fn check_ramp_stream(
    label: &str,
    url: &str,
    expected: &str,
    retries: usize,
) -> Option<String> {
    let mut last = None;
    for _ in 0..retries {
        if let Ok(dimensions) = probe_dims_ramp(url).await {
            if dimensions == expected {
                println!("  ok   {label:<45} -> {dimensions}");
                return Some(dimensions);
            }
            if !dimensions.is_empty() {
                last = Some(dimensions);
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    println!(
        "  FAIL {label:<45} expected={expected} got={}",
        last.as_deref().unwrap_or("none")
    );
    last
}

pub(crate) async fn probe_dims_ramp(url: &str) -> Result<String, String> {
    probe_dims_ramp_with_cookie(url, None).await
}

/// Minimal HLS playlist progress marker used by live-edge checks.
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

fn srt_publish_url(port: u16, stream_key: &str, crypto: Option<(&str, u32)>) -> String {
    harness_srt_ffmpeg_url(port, stream_key, HarnessSrtMode::Publish, crypto)
}

fn srt_read_url(port: u16, stream_key: &str, crypto: Option<(&str, u32)>) -> String {
    harness_srt_ffmpeg_url(port, stream_key, HarnessSrtMode::Read, crypto)
}

async fn expect_ingest_rejected(
    api: &RampApi,
    pipeline_id: &str,
    fixture: &Path,
    publish_url: &str,
    label: &str,
) -> Result<Value, String> {
    let mut publisher = spawn_publisher(fixture, publish_url, "mpegts", true).await?;
    tokio::time::sleep(Duration::from_secs(4)).await;
    let live = wait_for_api_input_live(api, pipeline_id, Duration::from_secs(1))
        .await
        .is_ok();
    stop_child(&mut publisher).await;
    if live {
        return Err(format!("{label}: ingest unexpectedly went live"));
    }
    wait_for_api_input_off(api, pipeline_id, Duration::from_secs(5)).await?;
    Ok(json!({"passed": true, "label": label}))
}

async fn expect_srt_read_failure(url: &str, label: &str) -> Result<Value, String> {
    match ffprobe(url).await {
        Ok(probe) => Err(format!("{label}: read unexpectedly succeeded: {probe}")),
        Err(error) => Ok(json!({"passed": true, "label": label, "error": error})),
    }
}

async fn create_srt_policy_pipeline(
    api: &RampApi,
    name: &str,
    policy: Value,
) -> Result<String, String> {
    create_srt_policy_pipeline_with_key(api, name, name, policy).await
}

async fn create_srt_policy_pipeline_with_key(
    api: &RampApi,
    name: &str,
    stream_key: &str,
    policy: Value,
) -> Result<String, String> {
    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": name, "streamKey": stream_key, "srtIngestPolicy": policy}),
        )
        .await?;
    pipeline["pipeline"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| format!("{name} pipeline id missing"))
}

pub(crate) async fn srt_policy_correctness() -> Result<Value, String> {
    let work_dir = artifact_path("srt.policy");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let ports = TestPorts::from_env();

    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let fixture = checked_h264_fixture()?;

    let mut results = serde_json::Map::new();

    api.patch_json(
        "/api/v1/settings",
        json!({"srtIngest": {"mode": "plaintext", "pbkeylen": 16, "passphrase": null}}),
    )
    .await?;
    let plain_inherit_id =
        create_srt_policy_pipeline(&api, "policy-plain-inherit", json!({"mode": "inherit"}))
            .await?;
    let mut plain_pub = spawn_publisher(
        &fixture,
        &srt_publish_url(ports.srt, "policy-plain-inherit", None),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &plain_inherit_id, Duration::from_secs(15)).await?;
    let plain_read_probe = ffprobe(&srt_read_url(ports.srt, "policy-plain-inherit", None)).await?;
    assert_media_only(&plain_read_probe, "plain inherit read")?;
    stop_child(&mut plain_pub).await;
    wait_for_api_input_off(&api, &plain_inherit_id, Duration::from_secs(10)).await?;
    results.insert(
        "globalPlaintextInherit".to_string(),
        json!({"passed": true, "readProbe": plain_read_probe}),
    );

    api.patch_json(
        "/api/v1/settings",
        json!({"srtIngest": {"mode": "encrypted", "passphrase": "globalpass123", "pbkeylen": 16}}),
    )
    .await?;
    let global_enc_id =
        create_srt_policy_pipeline(&api, "policy-global-enc", json!({"mode": "inherit"})).await?;
    let mut global_enc_pub = spawn_publisher(
        &fixture,
        &srt_publish_url(ports.srt, "policy-global-enc", Some(("globalpass123", 16))),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &global_enc_id, Duration::from_secs(15)).await?;
    let global_enc_read = ffprobe(&srt_read_url(
        ports.srt,
        "policy-global-enc",
        Some(("globalpass123", 16)),
    ))
    .await?;
    assert_media_only(&global_enc_read, "global encrypted read")?;
    let global_enc_read_fail = expect_srt_read_failure(
        &srt_read_url(ports.srt, "policy-global-enc", None),
        "global encrypted plaintext read",
    )
    .await?;
    stop_child(&mut global_enc_pub).await;
    wait_for_api_input_off(&api, &global_enc_id, Duration::from_secs(10)).await?;
    let global_enc_publish_fail = expect_ingest_rejected(
        &api,
        &global_enc_id,
        &fixture,
        &srt_publish_url(ports.srt, "policy-global-enc", None),
        "global encrypted plaintext publish",
    )
    .await?;
    results.insert(
        "globalEncrypted16Inherit".to_string(),
        json!({
            "passed": true,
            "readProbe": global_enc_read,
            "plaintextReadRejected": global_enc_read_fail,
            "plaintextPublishRejected": global_enc_publish_fail,
        }),
    );

    let plain_override_id =
        create_srt_policy_pipeline(&api, "policy-plain-override", json!({"mode": "plaintext"}))
            .await?;
    let mut plain_override_pub = spawn_publisher(
        &fixture,
        &srt_publish_url(ports.srt, "policy-plain-override", None),
        "mpegts",
        true,
    )
    .await?;
    wait_for_api_input_live(&api, &plain_override_id, Duration::from_secs(15)).await?;
    let plain_override_read =
        ffprobe(&srt_read_url(ports.srt, "policy-plain-override", None)).await?;
    assert_media_only(&plain_override_read, "plain override read")?;
    stop_child(&mut plain_override_pub).await;
    wait_for_api_input_off(&api, &plain_override_id, Duration::from_secs(10)).await?;
    results.insert(
        "globalEncrypted16PipelinePlaintext".to_string(),
        json!({"passed": true, "readProbe": plain_override_read}),
    );

    api.patch_json(
        "/api/v1/settings",
        json!({"srtIngest": {"mode": "plaintext", "pbkeylen": 16, "passphrase": null}}),
    )
    .await?;
    for (label, stream_key, passphrase, pbkeylen) in [
        (
            "pipelineEncrypted24",
            "policy-enc-24",
            "pipepass1234",
            24u32,
        ),
        (
            "pipelineEncrypted32",
            "policy-enc-32",
            "pipepass12345",
            32u32,
        ),
    ] {
        let pipeline_id = create_srt_policy_pipeline_with_key(
            &api,
            label,
            stream_key,
            json!({"mode": "encrypted", "passphrase": passphrase, "pbkeylen": pbkeylen}),
        )
        .await?;
        let mut pub_ok = spawn_publisher(
            &fixture,
            &srt_publish_url(ports.srt, stream_key, Some((passphrase, pbkeylen))),
            "mpegts",
            true,
        )
        .await?;
        wait_for_api_input_live(&api, &pipeline_id, Duration::from_secs(15)).await?;
        let read_ok = ffprobe(&srt_read_url(
            ports.srt,
            stream_key,
            Some((passphrase, pbkeylen)),
        ))
        .await?;
        assert_media_only(&read_ok, label)?;
        let read_plain_fail = expect_srt_read_failure(
            &srt_read_url(ports.srt, stream_key, None),
            &format!("{label} plaintext read"),
        )
        .await?;
        let read_wrong_pass_fail = expect_srt_read_failure(
            &srt_read_url(ports.srt, stream_key, Some(("wrongpass123", pbkeylen))),
            &format!("{label} wrong passphrase read"),
        )
        .await?;
        stop_child(&mut pub_ok).await;
        wait_for_api_input_off(&api, &pipeline_id, Duration::from_secs(10)).await?;
        let publish_plain_fail = expect_ingest_rejected(
            &api,
            &pipeline_id,
            &fixture,
            &srt_publish_url(ports.srt, stream_key, None),
            &format!("{label} plaintext publish"),
        )
        .await?;
        results.insert(
            label.to_string(),
            json!({
                "passed": true,
                "readProbe": read_ok,
                "plaintextReadRejected": read_plain_fail,
                "wrongPassphraseReadRejected": read_wrong_pass_fail,
                "plaintextPublishRejected": publish_plain_fail,
            }),
        );
    }

    stop_child(&mut child).await;
    let value = Value::Object(results);
    let path = work_dir.join("results.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&value).unwrap());
    Ok(value)
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

/// Test: RTMP B-frame ingest -> RTMP egress timestamp round-trip.
///
/// Publishes B-frame H.264/AAC over RTMP, sends egress to the generalized
/// harness sink, and verifies ffprobe observes composition offsets (PTS > DTS)
/// while DTS stays monotone.
pub(crate) async fn bframe_rtmp_correctness() -> Result<Value, String> {
    let work_dir = artifact_path("timestamp.bframe");
    std::fs::create_dir_all(&work_dir).map_err(|e| e.to_string())?;

    let restream_bin = default_restream_bin();
    let db_path = work_dir.join("data.sqlite");
    let log_path = work_dir.join("restream.log");
    let mediamtx_config = work_dir.join("mediamtx.yml");
    let mediamtx_log = work_dir.join("mediamtx.log");
    let all_ports = harness_port_defaults();
    let sink_port = harness_port_defaults().sink;
    let ports = TestPorts::from_env();

    let mut mediamtx = start_local_mediamtx(&mediamtx_config, &mediamtx_log, all_ports).await?;
    let (mut child, api) = start_restream_api(&restream_bin, &ports, &db_path, &log_path).await?;

    let pipeline_id =
        create_pipeline_with_stream_key(&api, "B-frame RTMP source", "e2e-bframe-src").await?;

    // Create RTMP egress output pointed at the harness sink
    let sink_url = format!("rtmp://127.0.0.1:{sink_port}/live/e2e-bframe-sink");
    let output_id = create_output(&api, &pipeline_id, "bframe-sink", &sink_url, "source").await?;

    // Start generalized sink
    let sink_metrics = Arc::new(GeneralizedSinkMetrics::default());
    let sink_server = start_generalized_sink_server(sink_port, sink_metrics.clone()).await?;

    let fixture = checked_h264_fixture()?;

    let mut publisher = spawn_publisher(
        &fixture,
        &format!("rtmp://127.0.0.1:{}/live/e2e-bframe-src", ports.rtmp),
        "flv",
        false,
    )
    .await?;
    wait_for_api_input_live(&api, &pipeline_id, Duration::from_secs(15)).await?;
    println!("[timestamp.bframe] Source ingest established");

    // Start the output
    start_output(&api, &pipeline_id, &output_id).await?;

    // Wait for sink to accumulate packets
    let deadline = Instant::now() + Duration::from_secs(15);
    while sink_metrics.video_count.load(Ordering::Relaxed) < 30 {
        if Instant::now() >= deadline {
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    tokio::time::sleep(Duration::from_secs(2)).await;

    // Also probe via ffprobe for B-frame packet-level analysis
    let packets_path = work_dir.join("bframe-packets.json");
    let read_url = format!("rtmp://127.0.0.1:{}/live/e2e-bframe-src", ports.rtmp);
    let packet_probe = ffprobe_video_packets(&read_url, &packets_path).await?;
    let packet_count = count_video_packets(&packet_probe);
    let bframe_count = count_bframe_packets(&packet_probe);
    let ffprobe_dts_monotone = video_dts_monotone(&packet_probe);

    let sink_dts_monotone = sink_metrics.dts_monotone();
    let video_count = sink_metrics.video_count.load(Ordering::Relaxed);
    let sink_summary = sink_metrics.summary();

    let source_passed =
        packet_count >= 30 && bframe_count > 0 && ffprobe_dts_monotone && sink_dts_monotone;
    let mut source_results = json!({
        "passed": source_passed,
        "packetCount": packet_count,
        "bframeCount": bframe_count,
        "ffprobeDtsMonotone": ffprobe_dts_monotone,
        "sinkDtsMonotone": sink_dts_monotone,
        "sinkVideoCount": video_count,
        "sink": sink_summary,
    });
    if packet_count < 30 {
        source_results["error"] = json!(format!(
            "expected at least 30 video packets, got {packet_count}"
        ));
    } else if bframe_count == 0 {
        source_results["error"] = json!("RTMP egress did not expose any packets with PTS > DTS");
    } else if !ffprobe_dts_monotone || !sink_dts_monotone {
        source_results["error"] = json!("RTMP egress DTS values are not monotone");
    }

    install_bframe_transcode_profiles(&api).await?;
    let transcode_bframes_0 = run_transcode_bframe_probe_case(
        &api,
        &pipeline_id,
        &work_dir,
        all_ports.mtx_rtmp,
        "h264-bf0",
        "h264_bf0",
        ExpectedBframeSignal::None,
    )
    .await?;
    let transcode_bframes_2 = run_transcode_bframe_probe_case(
        &api,
        &pipeline_id,
        &work_dir,
        all_ports.mtx_rtmp,
        "h264-bf2",
        "h264_bf2",
        ExpectedBframeSignal::Present,
    )
    .await?;

    stop_child(&mut publisher).await;
    stop_generalized_sink_server(sink_server);
    stop_child(&mut child).await;
    stop_child(&mut mediamtx).await;

    let passed = source_passed
        && transcode_bframes_0["passed"].as_bool().unwrap_or(false)
        && transcode_bframes_2["passed"].as_bool().unwrap_or(false);
    let results = json!({
        "passed": passed,
        "sourcePassthrough": source_results,
        "transcodeBframes0": transcode_bframes_0,
        "transcodeBframes2": transcode_bframes_2,
    });

    let path = work_dir.join("results.json");
    std::fs::write(&path, serde_json::to_vec_pretty(&results).unwrap())
        .map_err(|e| e.to_string())?;
    println!("{}", serde_json::to_string_pretty(&results).unwrap());
    if passed {
        Ok(results)
    } else {
        Err(format!("RTMP B-frame round-trip failed: {results}"))
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
    // Recording start/stop follows live media timestamps and keyframe/GOP
    // boundaries, not the wall-clock sleep edge in this harness. Bound the
    // drift to one target GOP window plus a small hosted-runner stop latency
    // allowance so the test still catches runaway recording duration without
    // failing normal live-edge alignment.
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
