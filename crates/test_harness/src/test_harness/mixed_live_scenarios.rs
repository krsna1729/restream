//! Live and anchor mixed-scenario execution.

use super::*;
use crate::mixed_runner::mixed_telemetry::MixedRssReport;

pub(in super::super) async fn run_mixed_anchor_config(
    env: &MixedEnv,
    api: &RampApi,
    restream_pid: u32,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<Value, String> {
    let cfg = case.scenario_id();
    let n = env.n_per_group;
    let output_cases = mixed_output_cases_for_input(case);
    let total = n * output_cases.len();
    let AnchorSetup {
        pipeline_id,
        mut publisher,
        mut output_ids,
        hls_preview,
        recording,
        rss,
    } = anchor_setup(env, api, restream_pid, case, resume).await?;

    verify_anchor_dimensions_or_lifecycle(env, api, cfg, output_cases, resume).await?;
    verify_anchor_hls_streams(env, api, cfg, &pipeline_id, resume).await?;

    // Live egress sink probe: assert DTS monotonicity, video+audio presence,
    // and keyframe cadence on the live egress.
    let (sink_probe_failure, probe_extras) =
        anchor_probes(env, api, cfg, &pipeline_id, &mut output_ids, resume).await?;

    stop_anchor_run(
        env,
        api,
        cfg,
        &pipeline_id,
        &mut publisher,
        &output_ids,
        resume,
    )
    .await?;

    if env.check_selected("runtime-log") {
        let runtime_log_started = Instant::now();
        verify_mixed_runtime_log_hygiene(env, cfg, &pipeline_id, runtime_log_started.elapsed())?;
    }

    if let Some(error) = sink_probe_failure {
        return Err(error);
    }

    write_mixed_artifact_index(env)?;
    let mut result = json!({
        "scenario": cfg,
        "pipelineId": pipeline_id,
        "nPerGroup": n,
        "totalOutputs": total,
        "rssDeltaKb": rss.delta_kb,
        "perOutputKb": rss.per_output_kb,
        "extFfmpegCount": rss.ffmpeg.count,
        "extFfmpegRssKb": rss.ffmpeg.rss_kb,
        "recording": recording,
        "outputMatrix": mixed_output_matrix_json(output_cases),
        "artifacts": {
            "outputsJson": env.outputs_json_path(),
            "artifactIndexJson": env.artifact_index_path(),
        },
        "outputs": env.output_registry_json(),
    });
    if let Value::Object(map) = &mut result {
        if let Some(summary) = hls_preview {
            map.insert("hlsPreview".to_string(), summary);
        }
        map.extend(probe_extras);
    }
    Ok(result)
}

/// One anchor run's live handles and the measurements taken while it ran.
struct AnchorSetup {
    pipeline_id: String,
    publisher: Child,
    output_ids: Vec<String>,
    hls_preview: Option<Value>,
    recording: Value,
    rss: MixedRssReport,
}

/// Start the source ingest and every output, then take the RSS delta.
async fn anchor_setup(
    env: &MixedEnv,
    api: &RampApi,
    restream_pid: u32,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<AnchorSetup, String> {
    let cfg = case.scenario_id();
    let n = env.n_per_group;
    let output_cases = mixed_output_cases_for_input(case);
    let total = n * output_cases.len();
    let (source_output_case, scaled_output_cases) = output_cases
        .split_first()
        .ok_or("mixed anchor output matrix must contain a source row")?;
    preflight_mixed_rtmps_capabilities(api, output_cases).await?;
    let (pipeline_id, stream_key) = create_mixed_pipeline(api, cfg).await?;

    let publisher = spawn_mixed_live_publisher(env, case, &stream_key).await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(45)).await?;
    let hls_preview =
        verify_optional_mixed_hls_preview(env, api, cfg, &pipeline_id, case, resume).await?;
    let recording = verify_mixed_recording(env, api, cfg, &pipeline_id, case, resume).await?;
    let rss_baseline = process_rss_kb(restream_pid).await.unwrap_or(0);
    if !env.skip_load {
        snapshot_mixed(env, restream_pid, cfg, "baseline (input live, 0 outputs)").await?;
    }

    let mut output_ids = Vec::with_capacity(total + 1);
    let hls_output =
        start_anchor_hls_preview_output(env, api, cfg, &pipeline_id, &mut output_ids).await?;

    add_mixed_output_matrix_rows(
        env,
        api,
        &pipeline_id,
        restream_pid,
        cfg,
        std::slice::from_ref(source_output_case),
        &mut output_ids,
    )
    .await?;

    emit_anchor_no_early_transcoder_check(env, cfg, resume)?;

    add_mixed_output_matrix_rows(
        env,
        api,
        &pipeline_id,
        restream_pid,
        cfg,
        scaled_output_cases,
        &mut output_ids,
    )
    .await?;
    if !env.skip_load {
        snapshot_mixed(
            env,
            restream_pid,
            cfg,
            &format!("after all {total} outputs"),
        )
        .await?;
    }
    verify_mixed_graph_stage_sharing(env, api, cfg, &pipeline_id, case, output_cases, resume)
        .await?;
    gate_anchor_output_progress(env, api, &pipeline_id, case, &hls_output, &output_ids).await?;
    let rss = record_mixed_rss_delta(env, cfg, restream_pid, rss_baseline, total, None).await?;
    Ok(AnchorSetup {
        pipeline_id,
        publisher,
        output_ids,
        hls_preview,
        recording,
        rss,
    })
}

/// Run the live sink, HLS PUT and burst probes; returns any sink-probe
/// failure and the report fields the probes produced.
async fn anchor_probes(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    output_ids: &mut Vec<String>,
    resume: &mut MixedResume,
) -> Result<(Option<String>, serde_json::Map<String, Value>), String> {
    let (sink_probe_result, sink_probe_failure) =
        run_optional_mixed_sink_probe(env, api, pipeline_id, cfg, output_ids, resume).await?;
    let hls_put_probe_result =
        run_anchor_hls_put_probe(env, api, cfg, pipeline_id, output_ids, resume).await?;
    let burst_graph_result = run_anchor_burst_graph(env, api, cfg, pipeline_id, resume).await?;

    let mut extra = serde_json::Map::new();
    if let Some(probe) = sink_probe_result {
        extra.insert("sinkProbe".to_string(), probe.summary);
        extra.insert("sinkProbePassed".to_string(), json!(probe.passed));
    }
    if let Some(probe) = hls_put_probe_result {
        extra.insert("hlsPutProbe".to_string(), probe.summary);
        extra.insert("hlsPutProbePassed".to_string(), json!(probe.passed));
    }
    if let Some((passed, summary)) = burst_graph_result {
        extra.insert("burstGraph".to_string(), summary);
        extra.insert("burstGraphPassed".to_string(), json!(passed));
    }
    Ok((sink_probe_failure, extra))
}

async fn start_anchor_hls_preview_output(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    output_ids: &mut Vec<String>,
) -> Result<String, String> {
    let hls_output = create_output(
        api,
        pipeline_id,
        "hls-preview",
        &format!("hls://{cfg}-preview"),
        "source",
    )
    .await?;
    start_output(api, pipeline_id, &hls_output).await?;
    output_ids.push(hls_output.clone());
    env.register_output_cell(HarnessOutputCell {
        scenario_id: cfg.to_string(),
        batch_group: "hls-preview".to_string(),
        wave: 0,
        pipeline_id: pipeline_id.to_string(),
        output_id: hls_output.clone(),
        output_name: "hls-preview".to_string(),
        cell_id: "hls-preview".to_string(),
        duplicate_index: 1,
        protocol: "hls".to_string(),
        encoding: "source".to_string(),
        rtmp_mode: None,
        selected_audio_track: None,
        publish_url: format!("hls://{cfg}-preview"),
        read_url: None,
        expected_dimensions: None,
        expected_audio_tracks: None,
        terminal_stage: None,
    })?;
    Ok(hls_output)
}

/// Smoke check that the external transcoder stays idle for source outputs.
fn emit_anchor_no_early_transcoder_check(
    env: &MixedEnv,
    cfg: &str,
    resume: &mut MixedResume,
) -> Result<(), String> {
    if !env.check_selected("smoke")
        || !resume.allows(&mixed_scenario_check_id(
            cfg,
            "no_early_external_transcoder",
        ))
    {
        return Ok(());
    }
    let started = Instant::now();
    let launches = count_log_matches(&env.restream_log, "[external-transcoder] Launching ffmpeg");
    if launches != 0 {
        emit_mixed_result(
            env,
            cfg,
            &mixed_scenario_check_id(cfg, "no_early_external_transcoder"),
            "fail",
            started.elapsed(),
            Some(json!({
                "message": format!("smoke: external transcoder fired before 720p outputs ({launches} launches)"),
                "external_transcoder_launches": launches,
            })),
        )?;
        return Err(format!(
            "smoke: external transcoder fired before 720p outputs ({launches} launches)"
        ));
    }
    emit_mixed_result(
        env,
        cfg,
        &mixed_scenario_check_id(cfg, "no_early_external_transcoder"),
        "pass",
        started.elapsed(),
        Some(json!({
            "external_transcoder_launches": launches,
        })),
    )?;
    log_mixed_ok(env, "smoke: no external transcoder for source outputs")
}

/// Gates external reads on the RTMP/SRT egress rows (helper HLS output excluded).
async fn gate_anchor_output_progress(
    env: &MixedEnv,
    api: &RampApi,
    pipeline_id: &str,
    case: MixedInputCase,
    hls_output: &str,
    output_ids: &[String],
) -> Result<(), String> {
    if !env.needs_live_output_progress_gate() {
        return Ok(());
    }
    // The live-anchor matrix adds an HLS helper output ahead of the probe
    // rows. Gate the external reads on the actual RTMP/SRT egress rows so
    // the first ffprobe does not race outputs that are still starting up.
    let progress_output_ids = mixed_progress_output_ids(output_ids, hls_output);
    wait_for_outputs_progress_with_env(
        api,
        pipeline_id,
        &progress_output_ids,
        mixed_output_progress_timeout_for_case(case, progress_output_ids.len()),
        Some(env),
    )
    .await?;
    Ok(())
}

/// Probe row dimensions, or warm the first 720p read under the lifecycle check.
async fn verify_anchor_dimensions_or_lifecycle(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    output_cases: &[MixedOutputCase],
    resume: &mut MixedResume,
) -> Result<(), String> {
    let n = env.n_per_group;
    if env.check_selected("ffprobe") {
        verify_mixed_output_dimensions(env, api, cfg, output_cases, resume).await?;
    } else if env.check_selected("lifecycle") {
        warm_mixed_stream(
            &format!("rtmp.720p.a0 out{n} lifecycle warmup"),
            &format!(
                "rtmp://127.0.0.1:{}/live/{cfg}-rtmp.720p.a0-{n}",
                env.mtx_rtmp
            ),
            "1280x720",
            None,
        )
        .await;
    }
    Ok(())
}

/// Probes the MediaMTX and Restream HLS transports for the anchor matrix.
async fn verify_anchor_hls_streams(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    resume: &mut MixedResume,
) -> Result<(), String> {
    if !env.check_selected("hls") {
        return Ok(());
    }
    let n = env.n_per_group;
    verify_mixed_stream(
        env,
        api,
        MixedProbeSpec {
            cfg,
            id: mixed_scenario_check_id(cfg, "hls_transport_mtx"),
            label: "HLS/mtx",
            url: &format!(
                "http://127.0.0.1:{}/live/{cfg}-rtmp.src.a0-{n}/index.m3u8",
                env.mtx_hls
            ),
            expected: "1920x1080",
            expected_video_codec: None,
            mediamtx_api: None,
            cookie: None,
            cell: None,
        },
        resume,
    )
    .await?;
    verify_mixed_stream(
        env,
        api,
        MixedProbeSpec {
            cfg,
            id: mixed_scenario_check_id(cfg, "hls_transport_restream"),
            label: "HLS/restream",
            url: &format!(
                "http://127.0.0.1:{}/hls/{pipeline_id}/index.m3u8",
                env.restream_http
            ),
            expected: "1920x1080",
            expected_video_codec: None,
            mediamtx_api: None,
            cookie: api.cookie.as_deref(),
            cell: None,
        },
        resume,
    )
    .await?;
    Ok(())
}

/// Runs the optional synthetic HLS PUT probe and registers its output cell.
async fn run_anchor_hls_put_probe(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    output_ids: &mut Vec<String>,
    resume: &mut MixedResume,
) -> Result<Option<HlsPutProbeResult>, String> {
    if !env.check_selected("hls-put-probe")
        || !resume.allows(&mixed_scenario_check_id(cfg, "hls_put"))
    {
        return Ok(None);
    }
    let started = Instant::now();
    let put_port = harness_port_defaults()
        .hls_put
        .checked_add(env.sink_port_offset as u16)
        .ok_or("mixed hls-put probe port overflowed")?;
    match run_hls_put_probe(api, pipeline_id, cfg, put_port).await {
        Ok(probe) => {
            let status = if probe.passed { "pass" } else { "fail" };
            emit_mixed_result(
                env,
                cfg,
                &mixed_scenario_check_id(cfg, "hls_put"),
                status,
                started.elapsed(),
                Some(probe.summary.clone()),
            )?;
            output_ids.push(probe.output_id.clone());
            env.register_output_cell(HarnessOutputCell {
                scenario_id: cfg.to_string(),
                batch_group: "hls-put".to_string(),
                wave: 0,
                pipeline_id: pipeline_id.to_string(),
                output_id: probe.output_id.clone(),
                output_name: format!("hls-put-{cfg}"),
                cell_id: "hls-put".to_string(),
                duplicate_index: 1,
                protocol: "http".to_string(),
                encoding: "source".to_string(),
                rtmp_mode: None,
                selected_audio_track: None,
                publish_url: format!(
                    "http://127.0.0.1:{put_port}/upload?cid=probe-{cfg}&copy=0&file=out.m3u8"
                ),
                read_url: None,
                expected_dimensions: None,
                expected_audio_tracks: None,
                terminal_stage: None,
            })?;
            Ok(Some(probe))
        }
        Err(e) => {
            emit_mixed_result(
                env,
                cfg,
                &mixed_scenario_check_id(cfg, "hls_put"),
                "fail",
                started.elapsed(),
                Some(json!({"error": e})),
            )?;
            Ok(None)
        }
    }
}

/// Runs the optional burst-graph check.
async fn run_anchor_burst_graph(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    resume: &mut MixedResume,
) -> Result<Option<(bool, Value)>, String> {
    if !env.check_selected("burst-graph")
        || !resume.allows(&mixed_scenario_check_id(cfg, "burst_graph"))
    {
        return Ok(None);
    }
    let started = Instant::now();
    match run_burst_graph_check(api, pipeline_id).await {
        Ok((passed, summary)) => {
            let status = if passed { "pass" } else { "fail" };
            emit_mixed_result(
                env,
                cfg,
                &mixed_scenario_check_id(cfg, "burst_graph"),
                status,
                started.elapsed(),
                Some(summary.clone()),
            )?;
            Ok(Some((passed, summary)))
        }
        Err(e) => {
            emit_mixed_result(
                env,
                cfg,
                &mixed_scenario_check_id(cfg, "burst_graph"),
                "fail",
                started.elapsed(),
                Some(json!({"error": e})),
            )?;
            Ok(None)
        }
    }
}

/// Stops the publisher and outputs, then verifies deletion and lifecycle checks.
async fn stop_anchor_run(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    publisher: &mut Child,
    output_ids: &[String],
    resume: &mut MixedResume,
) -> Result<(), String> {
    stop_child(publisher).await;
    stop_mixed_outputs(api, pipeline_id, output_ids).await;
    let lifecycle_started = Instant::now();
    let lifecycle_result =
        wait_for_outputs_stopped(api, pipeline_id, output_ids, Duration::from_secs(60)).await;
    if let Err(error) = &lifecycle_result {
        if env.check_selected("lifecycle")
            && resume.allows(&mixed_scenario_check_id(cfg, "clean_shutdown"))
        {
            emit_mixed_result(
                env,
                cfg,
                &mixed_scenario_check_id(cfg, "clean_shutdown"),
                "fail",
                lifecycle_started.elapsed(),
                Some(json!({
                    "message": error,
                    "stopped": false,
                    "requested": output_ids.len(),
                })),
            )?;
        }
        return Err(error.clone());
    }
    let delete_summary = delete_and_verify_mixed_outputs(
        env,
        api,
        cfg,
        pipeline_id,
        output_ids,
        Duration::from_secs(30),
    )
    .await?;
    if env.check_selected("lifecycle")
        && resume.allows(&mixed_scenario_check_id(cfg, "clean_shutdown"))
    {
        emit_mixed_result(
            env,
            cfg,
            &mixed_scenario_check_id(cfg, "clean_shutdown"),
            "pass",
            lifecycle_started.elapsed(),
            Some(json!({
                "stopped": output_ids.len(),
                "deleted": delete_summary["deleted"],
            })),
        )?;
    }
    log_mixed_ok(env, "lifecycle: all outputs stopped and deleted")
}

pub(in super::super) async fn run_mixed_live_config(
    env: &MixedEnv,
    api: &RampApi,
    restream_pid: u32,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<Value, String> {
    let cfg = case.scenario_id();
    let n = env.n_per_group;
    let output_cases = selected_mixed_output_cases(
        mixed_output_cases_for_input(case),
        env.output_groups.as_deref(),
    )?;
    preflight_mixed_rtmps_capabilities(api, &output_cases).await?;
    let total = n * output_cases.len();
    let (pipeline_id, stream_key) = create_mixed_pipeline(api, cfg).await?;

    let mut publisher = spawn_mixed_live_input(env, case, &stream_key).await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(45)).await?;
    let mut standby_publisher = spawn_mixed_standby_input(env, api, case, &pipeline_id).await?;
    verify_optional_mixed_hls_preview(env, api, cfg, &pipeline_id, case, resume).await?;
    let recording = verify_mixed_recording(env, api, cfg, &pipeline_id, case, resume).await?;
    if case.is_multi_track() {
        verify_optional_mixed_adaptive_ring(env, api, cfg, &pipeline_id, resume).await?;
    }

    let rss_baseline = process_rss_kb(restream_pid).await.unwrap_or(0);
    if !env.skip_load {
        snapshot_mixed(env, restream_pid, cfg, "baseline (input live, 0 outputs)").await?;
    }

    let mut output_ids = Vec::with_capacity(total);
    let mut ffmpeg_srt_sinks = Vec::new();
    let mut ffmpeg_signal_sinks = Vec::new();
    add_mixed_outputs_and_gate(
        env,
        api,
        &pipeline_id,
        restream_pid,
        case,
        &output_cases,
        resume,
        &mut output_ids,
        &mut ffmpeg_srt_sinks,
        &mut ffmpeg_signal_sinks,
    )
    .await?;

    let rss_min_audio_tracks = case.is_multi_track().then_some(2);
    let rss = record_mixed_rss_delta(
        env,
        cfg,
        restream_pid,
        rss_baseline,
        total,
        rss_min_audio_tracks,
    )
    .await?;

    if !ffmpeg_srt_sinks.is_empty() {
        finish_ffmpeg_srt_sinks(&mut ffmpeg_srt_sinks).await?;
    }

    verify_mixed_output_cases_inner(
        env,
        api,
        cfg,
        &output_cases,
        resume,
        case.is_multi_track(),
        case.is_multi_track(),
    )
    .await?;

    let (sink_probe_result, sink_probe_failure) =
        run_optional_mixed_sink_probe(env, api, &pipeline_id, cfg, &mut output_ids, resume).await?;

    let mut teardown = MixedLiveTeardown {
        env,
        api,
        cfg,
        pipeline_id: &pipeline_id,
        resume,
    };
    stop_mixed_live_run(
        &mut teardown,
        &mut publisher,
        standby_publisher.as_mut().map(|(_, publisher)| publisher),
        &output_ids,
    )
    .await?;

    if let Some(error) = sink_probe_failure {
        return Err(error);
    }

    write_mixed_artifact_index(env)?;
    let mut result = json!({
        "scenario": cfg,
        "pipelineId": pipeline_id,
        "nPerGroup": n,
        "totalOutputs": total,
        "rssDeltaKb": rss.delta_kb,
        "perOutputKb": rss.per_output_kb,
        "extFfmpegCount": rss.ffmpeg.count,
        "extFfmpegRssKb": rss.ffmpeg.rss_kb,
        "audioTracks": 2,
        "bufferedStandby": standby_publisher.as_ref().map(|(input, _)| json!({
            "inputId": input.id,
            "connected": true,
            "forwardingState": "standby",
        })),
        "recording": recording,
        "outputMatrix": mixed_output_matrix_json(&output_cases),
        "artifacts": {
            "outputsJson": env.outputs_json_path(),
            "artifactIndexJson": env.artifact_index_path(),
        },
        "outputs": env.output_registry_json(),
    });
    if let Some(probe) = sink_probe_result {
        result["sinkProbe"] = probe.summary;
        result["sinkProbePassed"] = json!(probe.passed);
    }
    Ok(result)
}

/// Spawns the case publisher (multi-track SRT or plain live).
async fn spawn_mixed_live_input(
    env: &MixedEnv,
    case: MixedInputCase,
    stream_key: &str,
) -> Result<Child, String> {
    if case.is_multi_track() {
        spawn_mixed_srt_multi_publisher(env, case, stream_key).await
    } else {
        spawn_mixed_live_publisher(env, case, stream_key).await
    }
}

/// Creates and connects the buffered standby input when the case uses one.
async fn spawn_mixed_standby_input(
    env: &MixedEnv,
    api: &RampApi,
    case: MixedInputCase,
    pipeline_id: &str,
) -> Result<Option<(HarnessInput, Child)>, String> {
    if !case.has_buffered_standby() {
        return Ok(None);
    }
    let standby = create_backup_input(api, pipeline_id).await?;
    let publisher = spawn_mixed_standby_publisher(env, case, &standby.stream_key).await?;
    wait_for_input_state(
        api,
        pipeline_id,
        &standby.id,
        "standby",
        Duration::from_secs(30),
    )
    .await?;
    Ok(Some((standby, publisher)))
}

/// Handles one live scenario's stop and delete lifecycle checks.
struct MixedLiveTeardown<'a> {
    env: &'a MixedEnv,
    api: &'a RampApi,
    cfg: &'a str,
    pipeline_id: &'a str,
    resume: &'a mut MixedResume,
}

/// Stops one live scenario's publisher and outputs, then verifies deletion.
async fn stop_mixed_live_run(
    teardown: &mut MixedLiveTeardown<'_>,
    publisher: &mut Child,
    standby: Option<&mut Child>,
    output_ids: &[String],
) -> Result<(), String> {
    let env = teardown.env;
    let api = teardown.api;
    let cfg = teardown.cfg;
    let pipeline_id = teardown.pipeline_id;
    let resume = &mut *teardown.resume;
    stop_child(publisher).await;
    if let Some(standby) = standby {
        stop_child(standby).await;
    }
    stop_mixed_outputs(api, pipeline_id, output_ids).await;
    wait_for_outputs_stopped(api, pipeline_id, output_ids, Duration::from_secs(60)).await?;
    let delete_summary = delete_and_verify_mixed_outputs(
        env,
        api,
        cfg,
        pipeline_id,
        output_ids,
        Duration::from_secs(30),
    )
    .await?;
    if env.check_selected("lifecycle")
        && resume.allows(&mixed_scenario_check_id(cfg, "clean_shutdown"))
    {
        emit_mixed_result(
            env,
            cfg,
            &mixed_scenario_check_id(cfg, "clean_shutdown"),
            "pass",
            Duration::ZERO,
            Some(json!({
                "stopped": output_ids.len(),
                "deleted": delete_summary["deleted"],
            })),
        )?;
    }
    Ok(())
}

pub(in super::super) fn mixed_input_fixture(case: MixedInputCase) -> Result<PathBuf, String> {
    let codec = match case.codec() {
        MixedVideoCodec::H264 => "h264",
        MixedVideoCodec::H265 => "h265",
    };
    restream_engine::test_fixtures::av_marker_transport_fixture_for_bframes(
        codec,
        case.is_multi_track(),
        case.fixture_bframe_mode(),
    )
}
