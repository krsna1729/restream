use super::*;

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
        serde_json::from_str::<Vec<RampConfig>>(include_str!("../ramp_configs.json"))
            .expect("embedded ramp_configs.json should define valid ramp rows")
    })
}

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
