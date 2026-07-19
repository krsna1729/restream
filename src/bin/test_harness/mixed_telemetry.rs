//! Resource sampling and stage-sharing telemetry helpers for mixed scenarios.

use super::*;
use std::path::Path;

pub(crate) struct MixedRssReport {
    pub(crate) delta_kb: u64,
    pub(crate) per_output_kb: u64,
    pub(crate) ffmpeg: FfmpegStats,
}

pub(crate) async fn record_mixed_rss_delta(
    env: &MixedEnv,
    cfg: &str,
    restream_pid: u32,
    rss_baseline: u64,
    total_outputs: usize,
    audio_tracks: Option<usize>,
) -> Result<MixedRssReport, String> {
    let rss_final = process_rss_kb(restream_pid).await.unwrap_or(0);
    let ffmpeg = ffmpeg_pipe1_stats().await;
    let delta_kb = rss_final.saturating_sub(rss_baseline);
    let per_output_kb = delta_kb / total_outputs.max(1) as u64;
    append_line(
        &env.rss_summary,
        &format!(
            "{cfg},rss_delta_kb={delta_kb},per_output_kb={per_output_kb},ext_ffmpeg_n={},ext_ffmpeg_rss_kb={}\n",
            ffmpeg.count, ffmpeg.rss_kb
        ),
    )?;
    if let Some(path) = std::env::var_os("SAVE_RSS_BASELINE") {
        append_line(
            Path::new(&path),
            &format!(
                "{cfg},per_output_kb={per_output_kb},ext_ffmpeg_n={},ext_ffmpeg_rss_kb={}\n",
                ffmpeg.count, ffmpeg.rss_kb
            ),
        )?;
    }
    if let Some(path) = std::env::var_os("RSS_BASELINE") {
        check_rss_baseline(Path::new(&path), cfg, per_output_kb)?;
    }
    if !env.skip_load && env.check_selected("load") {
        let mut details = json!({
            "rss_delta_kb": delta_kb,
            "per_output_kb": per_output_kb,
            "ext_ffmpeg_n": ffmpeg.count,
            "ext_ffmpeg_rss_kb": ffmpeg.rss_kb,
        });
        if let Some(audio_tracks) = audio_tracks {
            details["audio_tracks"] = json!(audio_tracks);
        }
        emit_mixed_result(
            env,
            cfg,
            &mixed_scenario_check_id(cfg, "load_delta_per_output"),
            "pass",
            Duration::ZERO,
            Some(details),
        )?;
    }
    Ok(MixedRssReport {
        delta_kb,
        per_output_kb,
        ffmpeg,
    })
}

fn parse_per_output_kb(line: &str) -> Option<(String, u64)> {
    let mut parts = line.split(',');
    let cfg = parts.next()?.trim();
    if cfg.is_empty() {
        return None;
    }
    for part in parts {
        let part = part.trim();
        if let Some(value) = part.strip_prefix("per_output_kb=")
            && let Ok(parsed) = value.parse::<u64>()
        {
            return Some((cfg.to_string(), parsed));
        }
    }
    None
}

fn check_rss_baseline(path: &Path, cfg: &str, current_per_output_kb: u64) -> Result<(), String> {
    let baseline = std::fs::read_to_string(path)
        .map_err(|error| format!("failed to read RSS_BASELINE {}: {error}", path.display()))?;
    let baseline_per_output_kb = baseline
        .lines()
        .filter_map(parse_per_output_kb)
        .find_map(|(baseline_cfg, value)| (baseline_cfg == cfg).then_some(value))
        .ok_or_else(|| {
            format!(
                "RSS baseline {} does not contain a per_output_kb row for {}",
                path.display(),
                cfg
            )
        })?;
    let threshold_pct = std::env::var("RSS_BASELINE_THRESHOLD_PCT")
        .ok()
        .and_then(|value| value.parse::<f64>().ok())
        .unwrap_or(5.0)
        .max(0.0);
    let allowed = ((baseline_per_output_kb as f64) * (1.0 + threshold_pct / 100.0)).ceil() as u64;
    if current_per_output_kb > allowed {
        return Err(format!(
            "RSS baseline regression for {cfg}: current per_output_kb={} exceeds allowed {} (baseline {} + {}%)",
            current_per_output_kb, allowed, baseline_per_output_kb, threshold_pct
        ));
    }
    Ok(())
}

pub(crate) async fn snapshot_mixed(
    env: &MixedEnv,
    restream_pid: u32,
    cfg: &str,
    label: &str,
) -> Result<(), String> {
    if !env.snapshot_sleep.is_zero() {
        tokio::time::sleep(env.snapshot_sleep).await;
    }
    let cpu = process_cpu_pct(restream_pid)
        .await
        .unwrap_or_else(|| "0".to_string());
    let rss = process_rss_kb(restream_pid).await.unwrap_or(0);
    let ffmpeg = ffmpeg_pipe1_stats().await;
    append_line(
        &env.scale_log,
        &format!(
            "{cfg},\"{label}\",{cpu},{rss},{},{}\n",
            ffmpeg.count, ffmpeg.rss_kb
        ),
    )?;
    println!(
        "  {label:<45} cpu={cpu}% rss={rss} KB ext_ffmpeg#={} ext_ffmpeg_rss={} KB",
        ffmpeg.count, ffmpeg.rss_kb
    );
    Ok(())
}

pub(crate) async fn verify_optional_mixed_adaptive_ring(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    resume: &mut MixedResume,
) -> Result<(), String> {
    // Two-audio-track SRT streams exceed the default small-ring shape; keep
    // this as a source-side verb so the live runner remains matrix-driven.
    let ring_check_id = mixed_scenario_check_id(cfg, "adaptive_source_ring");
    if !env.check_selected("ffprobe") && !resume.allows(&ring_check_id) {
        return Ok(());
    }

    let started = Instant::now();
    let telem_path = format!("/api/v1/pipelines/{pipeline_id}/telemetry");
    let deadline = Instant::now() + Duration::from_secs(6);
    let mut last_error = None;
    loop {
        match api.get_json(&telem_path).await {
            Ok(telem) => {
                let snapshot = mixed_adaptive_ring_snapshot(&telem);
                if snapshot.passed || Instant::now() >= deadline {
                    let passed = snapshot.passed;
                    emit_mixed_timing(
                        env,
                        cfg,
                        "input.adaptive_ring",
                        if passed { "pass" } else { "fail" },
                        started.elapsed(),
                        Some(snapshot.to_json()),
                    )?;
                    emit_mixed_result(
                        env,
                        cfg,
                        &ring_check_id,
                        if passed { "pass" } else { "fail" },
                        started.elapsed(),
                        Some(json!({
                            "ringCapacity": snapshot.capacity,
                            "bufferDepthSecs": snapshot.depth_secs,
                            "ringResized": snapshot.resized,
                            "adequate": snapshot.adequate,
                            "overflows": snapshot.overflows,
                        })),
                    )?;
                    if passed {
                        log_mixed_ok(
                            env,
                            &format!(
                                "adaptive-ring {cfg}: cap={} depth={:.1}s overflows={}{}",
                                snapshot.capacity,
                                snapshot.depth_secs,
                                snapshot.overflows,
                                if snapshot.resized { " [resized]" } else { "" }
                            ),
                        )?;
                        return Ok(());
                    }
                    return Err(format!(
                        "adaptive ring check failed for {cfg}: cap={} depth={:.1}s overflows={}",
                        snapshot.capacity, snapshot.depth_secs, snapshot.overflows
                    ));
                }
            }
            Err(error) => {
                last_error = Some(error);
            }
        }
        if Instant::now() >= deadline {
            let error = last_error.unwrap_or_else(|| "telemetry never became ready".to_string());
            emit_mixed_result(
                env,
                cfg,
                &ring_check_id,
                "fail",
                started.elapsed(),
                Some(json!({"error": error})),
            )?;
            emit_mixed_timing(
                env,
                cfg,
                "input.adaptive_ring",
                "fail",
                started.elapsed(),
                Some(json!({"error": error})),
            )?;
            return Err(format!("adaptive ring check failed for {cfg}: {error}"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

fn mixed_stage_count_from_graph(graph: &Value) -> MixedStageCount {
    MixedStageCount {
        video: graph_active_node_count(graph, "transcoder"),
        audio: graph_active_node_count(graph, "audio_filter"),
        codec_edge: graph_active_node_count(graph, "codec_edge"),
    }
}

pub(crate) async fn verify_mixed_graph_stage_sharing(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    case: MixedInputCase,
    output_cases: &[MixedOutputCase],
    resume: &mut MixedResume,
) -> Result<(), String> {
    if !env.check_selected("stage-sharing") {
        return Ok(());
    }
    let id = mixed_scenario_check_id(cfg, "stage_sharing");
    if !resume.allows(&id) {
        return Ok(());
    }
    let started = Instant::now();
    let expected = expected_mixed_stage_count_for_outputs(case, output_cases);
    let graph_path = format!("/api/v1/pipelines/{pipeline_id}/graph");
    // A stage-sharing-only run creates outputs but does not necessarily attach
    // every protocol reader. Audio-route stages are cheap and may be lazy until
    // the selected-track output is consumed, so this live check treats the
    // matrix audio count as an upper bound. The expensive HEVC->H.264
    // codec-edge count must be exact; that is the regression this check exists
    // to catch when N_PER_GROUP grows.
    let stage_counts_match = |got: MixedStageCount| {
        got.video == expected.video
            && got.codec_edge == expected.codec_edge
            && got.audio <= expected.audio
    };
    let deadline = Instant::now() + Duration::from_secs(12);
    let (graph, got) = loop {
        let graph = api.get_json(&graph_path).await?;
        let got = mixed_stage_count_from_graph(&graph);
        if stage_counts_match(got) || Instant::now() >= deadline {
            break (graph, got);
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    };
    let passed = stage_counts_match(got);
    emit_mixed_result(
        env,
        cfg,
        &id,
        if passed { "pass" } else { "fail" },
        started.elapsed(),
        Some(json!({
            "expected": {
                "video": expected.video,
                "audio": expected.audio,
                "codecEdge": expected.codec_edge,
            },
            "got": {
                "video": got.video,
                "audio": got.audio,
                "codecEdge": got.codec_edge,
            },
            "audioUpperBound": expected.audio,
            "exactCounts": ["video", "codecEdge"],
            "nPerGroup": env.n_per_group,
            "outputMatrix": mixed_output_matrix_json(output_cases),
            "graph": graph,
        })),
    )?;
    emit_mixed_timing(
        env,
        cfg,
        "check.stage_sharing",
        if passed { "pass" } else { "fail" },
        started.elapsed(),
        Some(json!({
            "expected": {
                "video": expected.video,
                "audio": expected.audio,
                "codecEdge": expected.codec_edge,
            },
            "got": {
                "video": got.video,
                "audio": got.audio,
                "codecEdge": got.codec_edge,
            },
            "nPerGroup": env.n_per_group,
        })),
    )?;
    if passed {
        log_mixed_ok(
            env,
            &format!(
                "stage-sharing {cfg}: video={} audio={}/{} codec_edge={} with N={}",
                got.video, got.audio, expected.audio, got.codec_edge, env.n_per_group
            ),
        )?;
        Ok(())
    } else {
        Err(format!(
            "{cfg}: expected stage counts video={} codec_edge={} and audio<={}, got video={} audio={} codec_edge={}",
            expected.video,
            expected.codec_edge,
            expected.audio,
            got.video,
            got.audio,
            got.codec_edge
        ))
    }
}
