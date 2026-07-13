use super::*;

fn parse_bitrate_specs(name: &str, default: &str) -> Result<Vec<BitrateSpec>, String> {
    let mut out = Vec::new();
    for part in std::env::var(name)
        .unwrap_or_else(|_| default.to_string())
        .split(',')
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        let normalized = part.to_ascii_uppercase();
        let mbps = if let Some(value) = normalized.strip_suffix('M') {
            value
                .parse::<f64>()
                .map_err(|_| format!("invalid Mbps bitrate {part:?}"))?
        } else if let Some(value) = normalized.strip_suffix('K') {
            value
                .parse::<f64>()
                .map_err(|_| format!("invalid Kbps bitrate {part:?}"))?
                / 1000.0
        } else {
            normalized
                .parse::<f64>()
                .map_err(|_| format!("invalid bitrate {part:?}"))?
        };
        out.push(BitrateSpec {
            label: part.to_string(),
            mbps,
        });
    }
    if out.is_empty() {
        return Err(format!("{name} produced no bitrate values"));
    }
    Ok(out)
}

struct BitrateSweepEnv {
    work_dir: PathBuf,
    summary_json: PathBuf,
    summary_csv: PathBuf,
    samples_jsonl: PathBuf,
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
    stabilize_secs: u64,
    sample_interval_secs: u64,
    output_groups: usize,
    no_cleanup: bool,
    bitrates: Vec<BitrateSpec>,
    configs: Vec<SweepConfig>,
}

impl BitrateSweepEnv {
    fn from_env() -> Result<Self, String> {
        let work_dir = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(".local/artifacts/bitrate-sweep"));
        let ports = harness_port_defaults();
        Ok(Self {
            summary_json: work_dir.join("bitrate-sweep-results.json"),
            summary_csv: work_dir.join("bitrate-sweep-results.csv"),
            samples_jsonl: work_dir.join("bitrate-sweep-samples.jsonl"),
            restream_log: work_dir.join("restream.log"),
            mediamtx_log: work_dir.join("mediamtx.log"),
            mediamtx_config: work_dir.join("mediamtx.yml"),
            restream_bin: default_restream_bin(),
            restream_db_path: std::env::var_os("RESTREAM_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_work_db_path(&work_dir, "bitrate-sweep.db")),
            restream_http: ports.restream_http,
            restream_rtmp: ports.restream_rtmp,
            restream_srt: ports.restream_srt,
            mtx_rtmp: ports.mtx_rtmp,
            mtx_srt: ports.mtx_srt,
            mtx_api: ports.mtx_api,
            stabilize_secs: env_secs("BITRATE_SWEEP_STABILIZE_SECS", 30),
            sample_interval_secs: env_secs("BITRATE_SWEEP_SAMPLE_INTERVAL_SECS", 5).max(1),
            output_groups: env_usize("BITRATE_SWEEP_OUTPUT_GROUPS", 1).max(1),
            no_cleanup: std::env::var("BITRATE_SWEEP_NO_CLEANUP")
                .ok()
                .is_some_and(|v| v == "1"),
            bitrates: parse_bitrate_specs("BITRATE_SWEEP_BITRATES", "1.5M,4M,8M")?,
            configs: parse_sweep_configs("BITRATE_SWEEP_CONFIGS")?,
            work_dir,
        })
    }
}

/// One target bitrate value in a bitrate sweep.
#[derive(Clone)]
struct BitrateSpec {
    label: String,
    mbps: f64,
}

/// One periodic resource sample captured during a bitrate-sweep case.
#[derive(Clone)]
struct BitrateSweepSample {
    config: String,
    bitrate_label: String,
    bitrate_mbps: f64,
    elapsed_secs: u64,
    restream_cpu_pct: f64,
    ffmpeg_cpu_pct: f64,
    total_cpu_pct: f64,
    restream_rss_kb: u64,
    ffmpeg_count: u64,
    ffmpeg_rss_kb: u64,
    total_rss_kb: u64,
    retained_payload_kb: u64,
    source_ring_kb: u64,
    transcoder_ring_kb: u64,
    tsmux_ring_kb: u64,
    avio_len_kb: u64,
    avio_hwm_kb: u64,
    overflow_count: u64,
}

/// Aggregated result for one bitrate/config/output-count case.
struct BitrateSweepCase {
    config: String,
    ingest_proto: String,
    video_codec: String,
    multi_audio: bool,
    bitrate_label: String,
    bitrate_mbps: f64,
    output_groups: usize,
    outputs_total: usize,
    restream_rss_base_kb: u64,
    restream_rss_final_kb: u64,
    restream_rss_delta_kb: u64,
    restream_rss_peak_kb: u64,
    ffmpeg_count_peak: u64,
    ffmpeg_rss_peak_kb: u64,
    total_rss_peak_kb: u64,
    restream_cpu_avg_pct: f64,
    restream_cpu_peak_pct: f64,
    ffmpeg_cpu_avg_pct: f64,
    ffmpeg_cpu_peak_pct: f64,
    total_cpu_avg_pct: f64,
    total_cpu_peak_pct: f64,
    retained_payload_min_kb: u64,
    retained_payload_max_kb: u64,
    retained_payload_final_kb: u64,
    retained_growth_kb_per_min: f64,
    source_ring_peak_kb: u64,
    transcoder_ring_peak_kb: u64,
    tsmux_ring_peak_kb: u64,
    avio_len_peak_kb: u64,
    avio_hwm_peak_kb: u64,
    overflow_count_final: u64,
    correctness_ok: bool,
    correctness_failures: Vec<String>,
}

pub(crate) async fn bitrate_sweep() -> Result<Value, String> {
    let env = BitrateSweepEnv::from_env()?;
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.samples_jsonl);

    let mut rows = Vec::new();
    for config in &env.configs {
        for bitrate in &env.bitrates {
            let row = run_bitrate_case(&env, *config, bitrate).await?;
            rows.push(row);
        }
    }

    write_bitrate_sweep_csv(&env.summary_csv, &rows)?;
    let result = json!({
        "mode": "bitrate-sweep",
        "artifacts": {
            "summaryJson": env.summary_json,
            "summaryCsv": env.summary_csv,
            "samplesJsonl": env.samples_jsonl,
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
        },
        "cases": rows.iter().map(bitrate_sweep_case_json).collect::<Vec<_>>(),
    });
    std::fs::write(
        &env.summary_json,
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .map_err(|e| e.to_string())?;
    Ok(result)
}

async fn run_bitrate_case(
    env: &BitrateSweepEnv,
    config: SweepConfig,
    bitrate: &BitrateSpec,
) -> Result<BitrateSweepCase, String> {
    let mut stack = start_bitrate_sweep_stack(env).await?;
    let stream_key = format!(
        "bitrate-{}-{}",
        config.name,
        bitrate.label.to_ascii_lowercase().replace('.', "_")
    );
    let pipeline_id = create_resource_pipeline(&stack.api, config.name, &stream_key).await?;
    let srt_crypto = harness_srt_crypto_from_env();
    let mut publisher = spawn_resource_publisher_with_bitrate(
        env.restream_rtmp,
        env.restream_srt,
        &env.work_dir,
        &srt_crypto,
        config,
        &stream_key,
        &bitrate.label,
    )?;
    wait_for_api_input_live(&stack.api, &pipeline_id, Duration::from_secs(45)).await?;
    let restream_rss_base_kb =
        read_proc_status_kb_checked(stack.restream_pid, "VmRSS", &env.restream_log)?;

    let mut output_ids = Vec::new();
    let mut probe_specs = Vec::new();
    for index in 1..=env.output_groups {
        let names = bitrate_case_output_names(config.name, &bitrate.label, index);
        for (kind, name, expected) in [
            (SweepOutputKind::RtmpSource, names.rtmp_source, "1920x1080"),
            (SweepOutputKind::Rtmp720p, names.rtmp_720p, "1280x720"),
            (SweepOutputKind::SrtSource, names.srt_source, "1920x1080"),
            (SweepOutputKind::Srt720p, names.srt_720p, "1280x720"),
        ] {
            let (url, encoding) = bitrate_output_url(env, config, kind, &name);
            let output_id = create_output(&stack.api, &pipeline_id, &name, &url, &encoding).await?;
            start_output(&stack.api, &pipeline_id, &output_id).await?;
            output_ids.push(output_id);
            probe_specs.push((kind, name, expected.to_string()));
        }
    }
    wait_for_outputs_progress(
        &stack.api,
        &pipeline_id,
        &output_ids,
        Duration::from_secs(45),
    )
    .await?;

    let samples = sample_bitrate_window(env, &mut stack, config, bitrate, &pipeline_id).await?;
    let mut correctness_ok = true;
    let mut correctness_failures = Vec::new();
    for (kind, name, expected) in &probe_specs {
        let url = bitrate_probe_url(env, *kind, name);
        if let Some(observed) =
            check_bitrate_stream(name, &url, expected, Duration::from_secs(20)).await?
        {
            correctness_ok = false;
            correctness_failures.push(format!("{name}: expected {expected}, observed {observed}"));
        }
    }

    let restream_rss_final_kb =
        read_proc_status_kb_checked(stack.restream_pid, "VmRSS", &env.restream_log).unwrap_or(0);
    let ffmpeg = ffmpeg_children_stats(stack.restream_pid)?;

    stop_child(&mut publisher).await;
    delete_resource_pipeline(&stack.api, &pipeline_id).await;
    if !env.no_cleanup {
        stop_child(&mut stack.restream).await;
        stop_child(&mut stack.mediamtx).await;
    }

    summarize_bitrate_case(
        config,
        bitrate,
        env.output_groups,
        restream_rss_base_kb,
        restream_rss_final_kb,
        ffmpeg,
        correctness_ok,
        correctness_failures,
        &samples,
    )
}

async fn start_bitrate_sweep_stack(env: &BitrateSweepEnv) -> Result<ResourceSweepStack, String> {
    if !env.restream_bin.exists() {
        return Err(format!(
            "restream binary not found at {}",
            env.restream_bin.display()
        ));
    }
    std::fs::create_dir_all(env.work_dir.join("logs")).map_err(|e| e.to_string())?;
    cleanup_ramp_db(&env.restream_db_path);
    let mediamtx_log = std::fs::File::create(&env.mediamtx_log).map_err(|e| e.to_string())?;
    let mediamtx_err = mediamtx_log.try_clone().map_err(|e| e.to_string())?;
    std::fs::write(
        &env.mediamtx_config,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: no\nwebrtc: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            env.mtx_rtmp, env.mtx_srt, env.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let mut mediamtx_command = Command::new("mediamtx");
    let mut mediamtx = remove_mediamtx_config_env(&mut mediamtx_command)
        .arg(&env.mediamtx_config)
        .stdout(Stdio::from(mediamtx_log))
        .stderr(Stdio::from(mediamtx_err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", env.mtx_api),
        Duration::from_secs(60),
    )
    .await
    {
        stop_child(&mut mediamtx).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }

    let restream_log = std::fs::File::create(&env.restream_log).map_err(|e| e.to_string())?;
    let restream_err = restream_log.try_clone().map_err(|e| e.to_string())?;
    let mut restream_cmd = Command::new(&env.restream_bin);
    restream_cmd
        .env("RESTREAM_HTTP_PORT", env.restream_http.to_string())
        .env("RESTREAM_RTMP_PORT", env.restream_rtmp.to_string())
        .env("RESTREAM_SRT_PORT", env.restream_srt.to_string())
        .env("RESTREAM_INITIAL_ADMIN_PASSWORD", harness_admin_password())
        .env("RESTREAM_LOG_DIR", env.work_dir.join("logs"))
        .env(
            "RESTREAM_DB_PATH",
            env.restream_db_path.to_string_lossy().to_string(),
        )
        .stdout(Stdio::from(restream_log))
        .stderr(Stdio::from(restream_err))
        .kill_on_drop(true);
    apply_harness_srt_listener_env(&mut restream_cmd);
    let mut restream = restream_cmd.spawn().map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/healthz", env.restream_http),
        Duration::from_secs(60),
    )
    .await
    {
        stop_child(&mut restream).await;
        stop_child(&mut mediamtx).await;
        return Err(format!("restream did not become ready: {err}"));
    }
    let mut api = RampApi::new(env.restream_http);
    api.login().await?;
    let restream_pid = restream.id().ok_or("restream pid missing")?;
    Ok(ResourceSweepStack {
        mediamtx,
        restream,
        api,
        restream_pid,
    })
}

/// Output names allocated for one bitrate-sweep case.
struct BitrateOutputNames {
    rtmp_source: String,
    rtmp_720p: String,
    srt_source: String,
    srt_720p: String,
}

fn bitrate_case_output_names(
    config_name: &str,
    bitrate_label: &str,
    index: usize,
) -> BitrateOutputNames {
    let suffix = bitrate_label.to_ascii_lowercase().replace('.', "_");
    BitrateOutputNames {
        rtmp_source: format!("{config_name}-{suffix}-rtmp-src-{index}"),
        rtmp_720p: format!("{config_name}-{suffix}-rtmp-720p-{index}"),
        srt_source: format!("{config_name}-{suffix}-srt-src-{index}"),
        srt_720p: format!("{config_name}-{suffix}-srt-720p-{index}"),
    }
}

fn bitrate_output_url(
    env: &BitrateSweepEnv,
    config: SweepConfig,
    kind: SweepOutputKind,
    name: &str,
) -> (String, String) {
    (
        kind.publish_url(env.mtx_rtmp, env.mtx_srt, name),
        kind.encoding(config.multi_audio).to_string(),
    )
}

fn bitrate_probe_url(env: &BitrateSweepEnv, kind: SweepOutputKind, name: &str) -> String {
    kind.read_url(env.mtx_rtmp, env.mtx_srt, name)
}

async fn sample_bitrate_window(
    env: &BitrateSweepEnv,
    stack: &mut ResourceSweepStack,
    config: SweepConfig,
    bitrate: &BitrateSpec,
    pipeline_id: &str,
) -> Result<Vec<BitrateSweepSample>, String> {
    let mut samples = Vec::new();
    let mut prev_ticks = read_proc_stat_ticks(stack.restream_pid)?;
    let mut prev_ffmpeg_ticks: HashMap<u32, u64> = HashMap::new();
    let mut prev_instant = Instant::now();
    let mut elapsed_secs = 0u64;
    let deadline = Instant::now() + Duration::from_secs(env.stabilize_secs);
    while Instant::now() < deadline {
        tokio::time::sleep(Duration::from_secs(env.sample_interval_secs)).await;
        elapsed_secs += env.sample_interval_secs;
        let ffmpeg = ffmpeg_children_stats(stack.restream_pid)?;
        let ticks = read_proc_stat_ticks(stack.restream_pid)?;
        let interval_secs = prev_instant.elapsed().as_secs_f64().max(0.001);
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) as f64 };
        let restream_cpu_pct =
            100.0 * (ticks.saturating_sub(prev_ticks)) as f64 / clk_tck / interval_secs;
        let mut ffmpeg_delta_ticks = 0u64;
        let mut next_ffmpeg_ticks = HashMap::new();
        for pid in &ffmpeg.pids {
            if let Ok(current_ticks) = read_proc_stat_ticks(*pid) {
                let previous_ticks = prev_ffmpeg_ticks.get(pid).copied().unwrap_or(current_ticks);
                ffmpeg_delta_ticks += current_ticks.saturating_sub(previous_ticks);
                next_ffmpeg_ticks.insert(*pid, current_ticks);
            }
        }
        let ffmpeg_cpu_pct = 100.0 * ffmpeg_delta_ticks as f64 / clk_tck / interval_secs;
        let total_cpu_pct = restream_cpu_pct + ffmpeg_cpu_pct;
        prev_ticks = ticks;
        prev_ffmpeg_ticks = next_ffmpeg_ticks;
        prev_instant = Instant::now();

        let telemetry = stack.api.get_json("/api/v1/engine/telemetry").await?;
        let pipeline_telemetry = stack
            .api
            .get_json(&format!("/api/v1/pipelines/{pipeline_id}/telemetry"))
            .await?;
        let accounting = &telemetry["memoryAccounting"];
        let avio = &accounting["avioQueues"];
        let overflow_count = pipeline_telemetry["sourceRing"]["readers"]
            .as_array()
            .map(|readers| {
                readers
                    .iter()
                    .map(|reader| reader["overflowCount"].as_u64().unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
        let sample = BitrateSweepSample {
            config: config.name.to_string(),
            bitrate_label: bitrate.label.clone(),
            bitrate_mbps: bitrate.mbps,
            elapsed_secs,
            restream_cpu_pct,
            ffmpeg_cpu_pct,
            total_cpu_pct,
            restream_rss_kb: read_proc_status_kb_checked(
                stack.restream_pid,
                "VmRSS",
                &env.restream_log,
            )?,
            ffmpeg_count: ffmpeg.count,
            ffmpeg_rss_kb: ffmpeg.rss_kb,
            total_rss_kb: read_proc_status_kb_checked(
                stack.restream_pid,
                "VmRSS",
                &env.restream_log,
            )? + ffmpeg.rss_kb,
            retained_payload_kb: accounting["retainedPayloadBytes"].as_u64().unwrap_or(0) / 1024,
            source_ring_kb: accounting["sourceRings"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
                .sum::<u64>()
                / 1024,
            transcoder_ring_kb: accounting["transcoderRings"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
                .sum::<u64>()
                / 1024,
            tsmux_ring_kb: accounting["tsMuxerRings"]
                .as_array()
                .unwrap_or(&Vec::new())
                .iter()
                .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap_or(0))
                .sum::<u64>()
                / 1024,
            avio_len_kb: avio["totalLenBytes"].as_u64().unwrap_or(0) / 1024,
            avio_hwm_kb: avio["inputQueues"]
                .as_array()
                .into_iter()
                .flatten()
                .chain(avio["egressQueues"].as_array().into_iter().flatten())
                .map(|queue| queue["highWaterBytes"].as_u64().unwrap_or(0))
                .sum::<u64>()
                / 1024,
            overflow_count,
        };
        append_line(
            &env.samples_jsonl,
            &format!(
                "{}\n",
                serde_json::to_string(&bitrate_sweep_sample_json(&sample)).unwrap()
            ),
        )?;
        samples.push(sample);
    }
    Ok(samples)
}

async fn check_bitrate_stream(
    label: &str,
    url: &str,
    expected: &str,
    timeout: Duration,
) -> Result<Option<String>, String> {
    let deadline = Instant::now() + timeout;
    let mut last_observed = None;
    let mut last_error = None;
    while Instant::now() < deadline {
        match probe_dims_ramp(url).await {
            Ok(dimensions) if dimensions == expected => return Ok(None),
            Ok(dimensions) if !dimensions.is_empty() => last_observed = Some(dimensions),
            Ok(_) => {}
            Err(error) => last_error = Some(error),
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
    let observed = last_observed
        .or(last_error)
        .unwrap_or_else(|| "none".to_string());
    println!("[bitrate-sweep] probe mismatch {label}: expected {expected}, observed {observed}");
    Ok(Some(observed))
}

#[allow(clippy::too_many_arguments)]
fn summarize_bitrate_case(
    config: SweepConfig,
    bitrate: &BitrateSpec,
    output_groups: usize,
    restream_rss_base_kb: u64,
    restream_rss_final_kb: u64,
    ffmpeg: FfmpegStats,
    correctness_ok: bool,
    correctness_failures: Vec<String>,
    samples: &[BitrateSweepSample],
) -> Result<BitrateSweepCase, String> {
    if samples.is_empty() {
        return Err("bitrate sweep produced no samples".to_string());
    }
    let retained_min_kb = samples
        .iter()
        .map(|sample| sample.retained_payload_kb)
        .min()
        .unwrap_or(0);
    let retained_max_kb = samples
        .iter()
        .map(|sample| sample.retained_payload_kb)
        .max()
        .unwrap_or(0);
    let retained_final_kb = samples
        .last()
        .map(|sample| sample.retained_payload_kb)
        .unwrap_or(0);
    let elapsed_min = (samples
        .last()
        .map(|sample| sample.elapsed_secs)
        .unwrap_or(0) as f64)
        / 60.0;
    Ok(BitrateSweepCase {
        config: config.name.to_string(),
        ingest_proto: config.ingest_proto.to_string(),
        video_codec: config.video_codec.to_string(),
        multi_audio: config.multi_audio,
        bitrate_label: bitrate.label.clone(),
        bitrate_mbps: bitrate.mbps,
        output_groups,
        outputs_total: output_groups * 4,
        restream_rss_base_kb,
        restream_rss_final_kb,
        restream_rss_delta_kb: restream_rss_final_kb.saturating_sub(restream_rss_base_kb),
        restream_rss_peak_kb: samples
            .iter()
            .map(|sample| sample.restream_rss_kb)
            .max()
            .unwrap_or(0),
        ffmpeg_count_peak: samples
            .iter()
            .map(|sample| sample.ffmpeg_count)
            .max()
            .unwrap_or(ffmpeg.count),
        ffmpeg_rss_peak_kb: samples
            .iter()
            .map(|sample| sample.ffmpeg_rss_kb)
            .max()
            .unwrap_or(ffmpeg.rss_kb),
        total_rss_peak_kb: samples
            .iter()
            .map(|sample| sample.total_rss_kb)
            .max()
            .unwrap_or(restream_rss_final_kb + ffmpeg.rss_kb),
        restream_cpu_avg_pct: round2(
            samples
                .iter()
                .map(|sample| sample.restream_cpu_pct)
                .sum::<f64>()
                / samples.len() as f64,
        ),
        restream_cpu_peak_pct: round2(
            samples
                .iter()
                .map(|sample| sample.restream_cpu_pct)
                .fold(0.0, f64::max),
        ),
        ffmpeg_cpu_avg_pct: round2(
            samples
                .iter()
                .map(|sample| sample.ffmpeg_cpu_pct)
                .sum::<f64>()
                / samples.len() as f64,
        ),
        ffmpeg_cpu_peak_pct: round2(
            samples
                .iter()
                .map(|sample| sample.ffmpeg_cpu_pct)
                .fold(0.0, f64::max),
        ),
        total_cpu_avg_pct: round2(
            samples
                .iter()
                .map(|sample| sample.total_cpu_pct)
                .sum::<f64>()
                / samples.len() as f64,
        ),
        total_cpu_peak_pct: round2(
            samples
                .iter()
                .map(|sample| sample.total_cpu_pct)
                .fold(0.0, f64::max),
        ),
        retained_payload_min_kb: retained_min_kb,
        retained_payload_max_kb: retained_max_kb,
        retained_payload_final_kb: retained_final_kb,
        retained_growth_kb_per_min: if elapsed_min > 0.0 {
            round2((retained_final_kb.saturating_sub(retained_min_kb)) as f64 / elapsed_min)
        } else {
            0.0
        },
        source_ring_peak_kb: samples
            .iter()
            .map(|sample| sample.source_ring_kb)
            .max()
            .unwrap_or(0),
        transcoder_ring_peak_kb: samples
            .iter()
            .map(|sample| sample.transcoder_ring_kb)
            .max()
            .unwrap_or(0),
        tsmux_ring_peak_kb: samples
            .iter()
            .map(|sample| sample.tsmux_ring_kb)
            .max()
            .unwrap_or(0),
        avio_len_peak_kb: samples
            .iter()
            .map(|sample| sample.avio_len_kb)
            .max()
            .unwrap_or(0),
        avio_hwm_peak_kb: samples
            .iter()
            .map(|sample| sample.avio_hwm_kb)
            .max()
            .unwrap_or(0),
        overflow_count_final: samples
            .last()
            .map(|sample| sample.overflow_count)
            .unwrap_or(0),
        correctness_ok,
        correctness_failures,
    })
}

fn bitrate_sweep_sample_json(sample: &BitrateSweepSample) -> Value {
    json!({
        "config": sample.config,
        "bitrateLabel": sample.bitrate_label,
        "bitrateMbps": sample.bitrate_mbps,
        "elapsedSecs": sample.elapsed_secs,
        "restreamCpuPct": sample.restream_cpu_pct,
        "ffmpegCpuPct": sample.ffmpeg_cpu_pct,
        "totalCpuPct": sample.total_cpu_pct,
        "restreamRssKb": sample.restream_rss_kb,
        "ffmpegCount": sample.ffmpeg_count,
        "ffmpegRssKb": sample.ffmpeg_rss_kb,
        "totalRssKb": sample.total_rss_kb,
        "retainedPayloadKb": sample.retained_payload_kb,
        "sourceRingKb": sample.source_ring_kb,
        "transcoderRingKb": sample.transcoder_ring_kb,
        "tsmuxRingKb": sample.tsmux_ring_kb,
        "avioLenKb": sample.avio_len_kb,
        "avioHwmKb": sample.avio_hwm_kb,
        "overflowCount": sample.overflow_count,
    })
}

fn bitrate_sweep_case_json(case: &BitrateSweepCase) -> Value {
    json!({
        "config": case.config,
        "ingestProto": case.ingest_proto,
        "videoCodec": case.video_codec,
        "multiAudio": case.multi_audio,
        "bitrateLabel": case.bitrate_label,
        "bitrateMbps": case.bitrate_mbps,
        "outputGroups": case.output_groups,
        "outputsTotal": case.outputs_total,
        "restreamRssBaseKb": case.restream_rss_base_kb,
        "restreamRssFinalKb": case.restream_rss_final_kb,
        "restreamRssDeltaKb": case.restream_rss_delta_kb,
        "restreamRssPeakKb": case.restream_rss_peak_kb,
        "ffmpegCountPeak": case.ffmpeg_count_peak,
        "ffmpegRssPeakKb": case.ffmpeg_rss_peak_kb,
        "totalRssPeakKb": case.total_rss_peak_kb,
        "restreamCpuAvgPct": case.restream_cpu_avg_pct,
        "restreamCpuPeakPct": case.restream_cpu_peak_pct,
        "ffmpegCpuAvgPct": case.ffmpeg_cpu_avg_pct,
        "ffmpegCpuPeakPct": case.ffmpeg_cpu_peak_pct,
        "totalCpuAvgPct": case.total_cpu_avg_pct,
        "totalCpuPeakPct": case.total_cpu_peak_pct,
        "retainedPayloadMinKb": case.retained_payload_min_kb,
        "retainedPayloadMaxKb": case.retained_payload_max_kb,
        "retainedPayloadFinalKb": case.retained_payload_final_kb,
        "retainedGrowthKbPerMin": case.retained_growth_kb_per_min,
        "sourceRingPeakKb": case.source_ring_peak_kb,
        "transcoderRingPeakKb": case.transcoder_ring_peak_kb,
        "tsmuxRingPeakKb": case.tsmux_ring_peak_kb,
        "avioLenPeakKb": case.avio_len_peak_kb,
        "avioHwmPeakKb": case.avio_hwm_peak_kb,
        "overflowCountFinal": case.overflow_count_final,
        "correctnessOk": case.correctness_ok,
        "correctnessFailures": case.correctness_failures,
    })
}

fn write_bitrate_sweep_csv(path: &Path, rows: &[BitrateSweepCase]) -> Result<(), String> {
    let mut text = String::from(
        "config,ingest_proto,video_codec,multi_audio,bitrate_label,bitrate_mbps,output_groups,outputs_total,restream_rss_base_kb,restream_rss_final_kb,restream_rss_delta_kb,restream_rss_peak_kb,ffmpeg_count_peak,ffmpeg_rss_peak_kb,total_rss_peak_kb,restream_cpu_avg_pct,restream_cpu_peak_pct,ffmpeg_cpu_avg_pct,ffmpeg_cpu_peak_pct,total_cpu_avg_pct,total_cpu_peak_pct,retained_payload_min_kb,retained_payload_max_kb,retained_payload_final_kb,retained_growth_kb_per_min,source_ring_peak_kb,transcoder_ring_peak_kb,tsmux_ring_peak_kb,avio_len_peak_kb,avio_hwm_peak_kb,overflow_count_final,correctness_ok\n",
    );
    for row in rows {
        text.push_str(&format!(
            "{},{},{},{},{},{:.2},{},{},{},{},{},{},{},{},{},{:.2},{:.2},{:.2},{:.2},{:.2},{:.2},{},{},{},{:.2},{},{},{},{},{},{},{}\n",
            csv_escape(&row.config),
            csv_escape(&row.ingest_proto),
            csv_escape(&row.video_codec),
            row.multi_audio,
            csv_escape(&row.bitrate_label),
            row.bitrate_mbps,
            row.output_groups,
            row.outputs_total,
            row.restream_rss_base_kb,
            row.restream_rss_final_kb,
            row.restream_rss_delta_kb,
            row.restream_rss_peak_kb,
            row.ffmpeg_count_peak,
            row.ffmpeg_rss_peak_kb,
            row.total_rss_peak_kb,
            row.restream_cpu_avg_pct,
            row.restream_cpu_peak_pct,
            row.ffmpeg_cpu_avg_pct,
            row.ffmpeg_cpu_peak_pct,
            row.total_cpu_avg_pct,
            row.total_cpu_peak_pct,
            row.retained_payload_min_kb,
            row.retained_payload_max_kb,
            row.retained_payload_final_kb,
            row.retained_growth_kb_per_min,
            row.source_ring_peak_kb,
            row.transcoder_ring_peak_kb,
            row.tsmux_ring_peak_kb,
            row.avio_len_peak_kb,
            row.avio_hwm_peak_kb,
            row.overflow_count_final,
            row.correctness_ok,
        ));
    }
    std::fs::write(path, text).map_err(|e| e.to_string())
}
