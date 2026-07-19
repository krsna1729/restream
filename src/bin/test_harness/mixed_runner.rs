//! Mixed-matrix runtime, sinks, probes, and assertion helpers.

use super::*;

#[path = "mixed_artifact_index.rs"]
mod mixed_artifact_index;
#[path = "mixed_artifacts.rs"]
mod mixed_artifacts;
#[path = "mixed_checks.rs"]
mod mixed_checks;
#[path = "mixed_control.rs"]
mod mixed_control;
#[path = "mixed_executor.rs"]
mod mixed_executor;
#[path = "mixed_lifecycle.rs"]
mod mixed_lifecycle;
#[path = "mixed_matrix_runner.rs"]
mod mixed_matrix_runner;
#[path = "mixed_outputs.rs"]
mod mixed_outputs;
#[path = "mixed_playback.rs"]
mod mixed_playback;
#[path = "mixed_probes.rs"]
mod mixed_probes;
#[path = "mixed_reporting.rs"]
mod mixed_reporting;
#[path = "mixed_root_cause.rs"]
mod mixed_root_cause;
#[path = "mixed_runtime.rs"]
mod mixed_runtime;
#[path = "mixed_signal.rs"]
mod mixed_signal;
#[path = "mixed_sinks.rs"]
mod mixed_sinks;
#[path = "mixed_stack.rs"]
mod mixed_stack;
#[path = "mixed_telemetry.rs"]
mod mixed_telemetry;
#[path = "output_helpers.rs"]
mod output_helpers;

pub(super) use mixed_artifact_index::{
    mixed_artifact_index_path, mixed_root_artifact_index_path, write_mixed_artifact_index,
    write_mixed_root_artifact_index,
};
pub(super) use mixed_artifacts::{HarnessOutputCell, HarnessOutputRegistry, infer_output_protocol};
pub(super) use mixed_checks::{verify_mixed_output_cases_inner, verify_mixed_output_dimensions};
pub(super) use mixed_control::{
    MixedResume, mixed_output_checks_need_live_progress_gate,
    mixed_output_progress_timeout_for_case, mixed_progress_output_ids,
};
pub(super) use mixed_executor::{ScenarioExecutionContext, scenario_executor_for_plan};
pub(super) use mixed_lifecycle::{
    delete_and_verify_mixed_outputs, stop_mixed_outputs, wait_for_outputs_stopped,
};
#[cfg(test)]
pub(super) use mixed_matrix_runner::{
    matrix_case_progress_rows, mixed_matrix_cases_can_share_wave, mixed_runtime_log_noise_lines,
    mixed_runtime_log_noise_matches, write_matrix_scenario_progress,
};
pub(super) use mixed_matrix_runner::{
    mixed_fast_breadth_correctness, mixed_signal_correctness, verify_mixed_runtime_log_hygiene,
    write_json_pretty_atomic,
};
pub(super) use mixed_outputs::{
    MixedGroupSpec, add_mixed_group, add_mixed_output_matrix_rows, mixed_output_matrix_json,
    mixed_output_publish_url, mixed_output_read_url,
};
pub(super) use mixed_playback::{verify_mixed_recording, verify_optional_mixed_hls_preview};
#[cfg(test)]
pub(super) use mixed_probes::decode_scan_needs_video_dts_fallback;
pub(super) use mixed_probes::{
    MixedProbeSpec, ffprobe_compact_audio_track_count, ffprobe_compact_validate_dts,
    ffprobe_compact_video_dimensions, verify_mixed_audio_route, verify_mixed_decode_scan,
    verify_mixed_stream, warm_mixed_stream,
};
pub(super) use mixed_reporting::{
    count_log_matches, effective_log_paths, emit_mixed_result, emit_mixed_timing,
    emit_mixed_timing_window, file_tail_lines, log_mixed_ok, safe_artifact_stem,
};
pub(super) use mixed_root_cause::{
    mixed_root_cause_summary_json, mixed_root_cause_summary_path, write_mixed_root_cause_summary,
};
pub(super) use mixed_runtime::{
    spawn_mixed_live_publisher, spawn_mixed_srt_multi_publisher, spawn_mixed_standby_publisher,
    start_mixed_mediamtx, start_mixed_restream,
};
#[cfg(test)]
pub(super) use mixed_signal::{
    PcmQualityReport, analyze_pcm_s16le, marker_gaps_from_intervals, max_audio_pts_gap_ms,
    nearest_marker_offsets_ms, parse_blackdetect_intervals, parse_silencedetect_intervals,
    validate_signal_quality,
};
pub(super) use mixed_signal::{
    SignalTolerances, capture_signal_sample, decode_pcm_quality, run_ffmpeg_filter_log,
    signal_report_json, validate_signal_quality_with_tolerances, verify_mixed_signal_quality,
};
pub(super) use mixed_sinks::{
    add_mixed_multi_output_cases, add_mixed_output_cases, finish_ffmpeg_signal_sinks,
    finish_ffmpeg_srt_sinks, run_optional_mixed_sink_probe, validate_signal_capture_artifact,
};
pub(super) use mixed_stack::{
    bind_mixed_env_to_shared_stack, start_mixed_harness_stack, stop_mixed_harness_stack,
};
pub(super) use mixed_telemetry::{
    record_mixed_rss_delta, snapshot_mixed, verify_mixed_graph_stage_sharing,
    verify_optional_mixed_adaptive_ring,
};
pub(super) use output_helpers::{create_output, create_output_with_rtmp_mode, start_output};

/// Runtime configuration and artifact paths for one mixed-matrix scenario.
#[derive(Clone)]
pub(super) struct MixedEnv {
    pub(super) work_dir: PathBuf,
    pub(super) media_dir: PathBuf,
    pub(super) scale_log: PathBuf,
    pub(super) timing_log: PathBuf,
    pub(super) rss_summary: PathBuf,
    pub(super) summary_log: PathBuf,
    pub(super) restream_log: PathBuf,
    pub(super) mediamtx_log: PathBuf,
    pub(super) mediamtx_config: PathBuf,
    pub(super) restream_bin: PathBuf,
    pub(super) restream_db_path: PathBuf,
    pub(super) assertion_log: Option<PathBuf>,
    pub(super) only_checks: Option<Vec<String>>,
    pub(super) resume_from: Option<String>,
    pub(super) skip_load: bool,
    pub(super) restream_http: u16,
    pub(super) restream_rtmp: u16,
    pub(super) restream_srt: u16,
    pub(super) mtx_rtmp: u16,
    pub(super) mtx_srt: u16,
    pub(super) mtx_hls: u16,
    pub(super) mtx_api: u16,
    pub(super) ffmpeg_srt_sink: bool,
    pub(super) ffmpeg_srt_sink_base: u16,
    pub(super) ffmpeg_srt_sink_seconds: u64,
    pub(super) ffmpeg_signal_sink_base: u16,
    pub(super) sink_port_offset: usize,
    pub(super) av_signal_seconds: u64,
    pub(super) av_soak_seconds: u64,
    pub(super) n_per_group: usize,
    pub(super) snapshot_sleep: Duration,
    pub(super) collect_failures: bool,
    pub(super) probe_sampling_policy: ProbeSamplingPolicy,
    pub(super) restream_env_overrides: Vec<(&'static str, String)>,
    pub(super) output_registry: Arc<Mutex<HarnessOutputRegistry>>,
}
impl MixedEnv {
    pub(super) fn from_env_with_default_work_dir(
        log_stem: &str,
        default_work_dir: PathBuf,
    ) -> Self {
        let work_dir = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or(default_work_dir);
        let ports = harness_port_defaults();
        Self {
            media_dir: std::env::var_os("RESTREAM_MEDIA_DIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("media")),
            scale_log: std::env::var_os("SCALE_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("scale.csv")),
            timing_log: std::env::var_os("TIMING_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("timing.jsonl")),
            rss_summary: std::env::var_os("RSS_SUMMARY")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("rss-summary.csv")),
            summary_log: std::env::var_os("SUMMARY_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join("summary.txt")),
            restream_log: std::env::var_os("MIXED_RESTREAM_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join(format!("{log_stem}-restream.log"))),
            mediamtx_log: std::env::var_os("MIXED_MEDIAMTX_LOG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join(format!("{log_stem}-mediamtx.log"))),
            mediamtx_config: std::env::var_os("MIXED_MEDIAMTX_CONFIG")
                .map(PathBuf::from)
                .unwrap_or_else(|| work_dir.join(format!("{log_stem}-mediamtx.yml"))),
            restream_bin: default_restream_bin(),
            restream_db_path: std::env::var_os("RESTREAM_DB_PATH")
                .map(PathBuf::from)
                .unwrap_or_else(|| default_work_db_path(&work_dir, &format!("{log_stem}.db"))),
            assertion_log: Some(
                std::env::var_os("ASSERTION_LOG")
                    .filter(|value| !value.is_empty())
                    .map(PathBuf::from)
                    .unwrap_or_else(|| work_dir.join("assertions.jsonl")),
            ),
            only_checks: std::env::var("ONLY_CHECKS")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .map(|value| {
                    value
                        .split(',')
                        .map(|item| item.trim().replace('_', "-"))
                        .filter(|item| !item.is_empty())
                        .collect()
                }),
            resume_from: std::env::var("RESUME_FROM")
                .ok()
                .filter(|value| !value.trim().is_empty()),
            skip_load: std::env::var("SKIP_LOAD")
                .ok()
                .is_some_and(|value| value == "1"),
            restream_http: ports.restream_http,
            restream_rtmp: ports.restream_rtmp,
            restream_srt: ports.restream_srt,
            mtx_rtmp: ports.mtx_rtmp,
            mtx_srt: ports.mtx_srt,
            mtx_hls: ports.mtx_hls,
            mtx_api: ports.mtx_api,
            ffmpeg_srt_sink: std::env::var("SRT_SINK")
                .or_else(|_| std::env::var("MIXED_SRT_SINK"))
                .ok()
                .is_some_and(|value| value.eq_ignore_ascii_case("ffmpeg")),
            ffmpeg_srt_sink_base: ports.ffmpeg_srt_sink_base,
            ffmpeg_srt_sink_seconds: env_secs("FFMPEG_SRT_SINK_SECONDS", 8),
            ffmpeg_signal_sink_base: ports.ffmpeg_signal_sink_base,
            sink_port_offset: 0,
            av_signal_seconds: env_secs("AV_SIGNAL_SECONDS", 20),
            av_soak_seconds: env_secs("AV_SOAK_SECONDS", 120),
            n_per_group: env_usize("N_PER_GROUP", 2),
            snapshot_sleep: Duration::from_secs(env_secs("SNAPSHOT_SLEEP_SECS", 3)),
            collect_failures: std::env::var("COLLECT_FAILURES")
                .ok()
                .is_some_and(|value| value == "1"),
            probe_sampling_policy: ProbeSamplingPolicy::LastDuplicate,
            restream_env_overrides: Vec::new(),
            output_registry: Arc::new(Mutex::new(HarnessOutputRegistry::new())),
            work_dir,
        }
    }

    pub(super) fn check_selected(&self, check: &str) -> bool {
        self.only_checks
            .as_ref()
            .is_none_or(|items| items.iter().any(|item| item == check))
    }

    pub(super) fn explicit_check_selected(&self, check: &str) -> bool {
        self.only_checks
            .as_ref()
            .is_some_and(|items| items.iter().any(|item| item == check))
    }

    pub(super) fn use_direct_signal_sinks(&self) -> bool {
        self.only_checks.as_ref().is_some_and(|items| {
            !items.is_empty()
                && items
                    .iter()
                    .all(|item| item == "signal" || item == "soak-drift")
        })
    }

    pub(super) fn needs_live_output_progress_gate(&self) -> bool {
        mixed_output_checks_need_live_progress_gate(self.only_checks.as_deref())
    }

    pub(super) fn probe_duplicate_index(&self) -> usize {
        let max_duplicate = self.n_per_group.max(1);
        self.probe_sampling_policy
            .duplicate_index(max_duplicate)
            .clamp(1, max_duplicate)
    }

    pub(super) fn outputs_json_path(&self) -> PathBuf {
        self.work_dir.join("outputs.json")
    }

    pub(super) fn artifact_index_path(&self) -> PathBuf {
        mixed_artifact_index_path(self)
    }

    pub(super) fn register_output_cell(&self, cell: HarnessOutputCell) -> Result<(), String> {
        let mut registry = self
            .output_registry
            .lock()
            .map_err(|_| "mixed output registry lock poisoned".to_string())?;
        registry.insert(cell);
        registry.write_outputs_json(&self.outputs_json_path())
    }

    pub(super) fn output_cell_label(&self, output_id: &str) -> Option<String> {
        self.output_registry
            .lock()
            .ok()
            .and_then(|registry| registry.get(output_id).map(HarnessOutputCell::label))
    }

    pub(super) fn output_cell(
        &self,
        cell_id: &str,
        duplicate_index: usize,
    ) -> Option<HarnessOutputCell> {
        self.output_registry
            .lock()
            .ok()
            .and_then(|registry| registry.find_cell(cell_id, duplicate_index).cloned())
    }

    pub(super) fn output_registry_json(&self) -> Value {
        self.output_registry
            .lock()
            .map(|registry| registry.to_json())
            .unwrap_or_else(|_| {
                json!({
                    "schemaVersion": mixed_artifacts::MIXED_OUTPUTS_SCHEMA_VERSION,
                    "outputs": [],
                    "error": "mixed output registry lock poisoned",
                })
            })
    }
}

pub(super) async fn mixed_input_case_correctness(case: MixedInputCase) -> Result<Value, String> {
    let mode = mixed_input_mode_name(case);
    let env = MixedEnv::from_env_with_default_work_dir(&mode, mixed_input_default_work_dir(case));
    run_mixed_input_case_with_env(case, env).await
}

pub(super) async fn mixed_input_matrix_correctness() -> Result<Value, String> {
    let force_serial = std::env::var("MIXED_MATRIX_SERIAL")
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"));
    if force_serial {
        return mixed_matrix_runner::mixed_input_matrix_correctness_serial().await;
    }

    mixed_matrix_runner::mixed_input_matrix_correctness_shared().await
}

pub(super) async fn run_mixed_input_case_with_env(
    case: MixedInputCase,
    env: MixedEnv,
) -> Result<Value, String> {
    let stack_started = Instant::now();
    let mut stack = start_mixed_harness_stack(env).await?;
    emit_mixed_timing(
        &stack.env,
        case.scenario_id(),
        "stack.start",
        "pass",
        stack_started.elapsed(),
        None,
    )?;
    let config = run_mixed_input_case_on_active_stack(
        case,
        stack.env.clone(),
        &stack.api,
        stack.restream_pid,
    )
    .await;

    let cleanup_started = Instant::now();
    stop_mixed_harness_stack(&mut stack).await;
    emit_mixed_timing(
        &stack.env,
        case.scenario_id(),
        "stack.cleanup",
        "pass",
        cleanup_started.elapsed(),
        None,
    )?;
    config
}

pub(super) async fn run_mixed_input_case_on_active_stack(
    case: MixedInputCase,
    mut env: MixedEnv,
    api: &RampApi,
    restream_pid: u32,
) -> Result<Value, String> {
    let cfg = case.scenario_id();
    let plan = MixedScenarioPlan::for_input(case);
    env.probe_sampling_policy = plan.probe_sampling_policy;
    let scenario_started = Instant::now();
    if env.n_per_group == 0 {
        return Err("N_PER_GROUP must be greater than zero".to_string());
    }
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    ensure_mixed_artifacts(&env)?;
    let mut resume = MixedResume::new(env.resume_from.clone());

    let config_started = Instant::now();
    let mut executor = scenario_executor_for_plan(plan)?;
    let executor_name = executor.name();
    let executor_steps = executor.step_names();
    let config = executor
        .execute(ScenarioExecutionContext {
            env: &env,
            api,
            restream_pid,
            case,
            resume: &mut resume,
        })
        .await;
    emit_mixed_timing(
        &env,
        cfg,
        "scenario.execute",
        if config.is_ok() { "pass" } else { "fail" },
        config_started.elapsed(),
        None,
    )?;
    emit_mixed_timing(
        &env,
        cfg,
        "scenario.total",
        if config.is_ok() { "pass" } else { "fail" },
        scenario_started.elapsed(),
        None,
    )?;

    let mut config = config?;
    if let Some(pipeline_id) = config["pipelineId"].as_str() {
        let cleanup_started = Instant::now();
        delete_pipeline_v1(api, pipeline_id).await?;
        emit_mixed_timing(
            &env,
            cfg,
            "scenario.pipeline_cleanup",
            "pass",
            cleanup_started.elapsed(),
            Some(json!({
                "pipelineId": pipeline_id,
            })),
        )?;
        config["pipelineDeleted"] = json!(true);
    }

    write_mixed_artifact_index(&env)?;
    Ok(json!({
        "passed": true,
        "mode": mixed_input_mode_name(case),
        "scenario": case.scenario_id(),
        "inputCase": {
            "id": case.scenario_id(),
            "source": case.source_name(),
            "ingest": case.ingest_name(),
            "video": case.codec_name(),
            "audio": case.audio_layout_name(),
            "reorder": case.reorder_name(),
            "sourceHasBframes": case.source_has_b_frames(),
        },
        "sharedStackGroup": case.shared_batch_group().as_str(),
        "sinkPortOffset": env.sink_port_offset,
        "plan": {
            "sourceAdapter": plan.source.adapter.as_str(),
            "executor": executor_name,
            "executorSteps": executor_steps,
            "outputCells": plan.output_cells(),
            "checks": plan.check_names(),
            "hlsPreviewTiming": plan.hls_preview_timing.as_str(),
            "supportedHlsPreviewTimings": HlsPreviewTiming::supported_names(),
            "probeSampling": {
                "policy": plan.probe_sampling_policy.as_str(),
                "duplicateIndex": env.probe_duplicate_index(),
                "nPerGroup": env.n_per_group,
            },
            "supportedProbeSamplingPolicies": ProbeSamplingPolicy::supported_names(),
            "expectedStages": {
                "video": plan.expected_stages.video,
                "audio": plan.expected_stages.audio,
                "codecEdge": plan.expected_stages.codec_edge,
            },
        },
        "configs": [config],
        "artifacts": {
            "workDir": env.work_dir,
            "scaleCsv": env.scale_log,
            "timingJsonl": env.timing_log,
            "rssSummary": env.rss_summary,
            "outputsJson": env.outputs_json_path(),
            "artifactIndexJson": env.artifact_index_path(),
            "summary": env.summary_log,
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
            "mediaDir": env.media_dir,
        },
        "outputs": env.output_registry_json(),
    }))
}

pub(super) fn ensure_mixed_artifacts(env: &MixedEnv) -> Result<(), String> {
    if !env.scale_log.exists() {
        std::fs::write(
            &env.scale_log,
            "config,label,cpu_pct,rss_kb,ext_ffmpeg_n,ext_ffmpeg_rss_kb\n",
        )
        .map_err(|e| e.to_string())?;
    }
    if !env.timing_log.exists() {
        std::fs::write(&env.timing_log, "").map_err(|e| e.to_string())?;
    }
    if !env.rss_summary.exists() {
        std::fs::write(&env.rss_summary, "").map_err(|e| e.to_string())?;
    }
    if !env.summary_log.exists() {
        std::fs::write(&env.summary_log, "").map_err(|e| e.to_string())?;
    }
    if !env.outputs_json_path().exists() {
        env.output_registry
            .lock()
            .map_err(|_| "mixed output registry lock poisoned".to_string())?
            .write_outputs_json(&env.outputs_json_path())?;
    }
    write_mixed_artifact_index(env)?;
    Ok(())
}

pub(super) async fn create_mixed_pipeline(
    api: &RampApi,
    cfg: &str,
) -> Result<(String, String), String> {
    let stream_key = format!("sk-{cfg}");
    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": cfg, "streamKey": stream_key}),
        )
        .await?;
    let pipeline_id = pipeline["pipeline"]["id"]
        .as_str()
        .ok_or("pipeline create response missing pipeline.id")?
        .to_string();
    Ok((pipeline_id, stream_key))
}

pub(super) async fn run_mixed_anchor_config(
    env: &MixedEnv,
    api: &RampApi,
    restream_pid: u32,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<Value, String> {
    let cfg = case.scenario_id();
    let n = env.n_per_group;
    let output_cases = single_track_mixed_output_cases();
    let total = n * output_cases.len();
    let (source_output_case, scaled_output_cases) = output_cases
        .split_first()
        .ok_or("mixed anchor output matrix must contain a source row")?;
    let (pipeline_id, stream_key) = create_mixed_pipeline(api, cfg).await?;

    let mut publisher = spawn_mixed_live_publisher(env, case, &stream_key).await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(45)).await?;
    let hls_preview =
        verify_optional_mixed_hls_preview(env, api, cfg, &pipeline_id, case, resume).await?;
    let recording = verify_mixed_recording(env, api, cfg, &pipeline_id, case, resume).await?;
    let rss_baseline = process_rss_kb(restream_pid).await.unwrap_or(0);
    if !env.skip_load {
        snapshot_mixed(env, restream_pid, cfg, "baseline (input live, 0 outputs)").await?;
    }

    let mut output_ids = Vec::with_capacity(total + 1);
    let hls_output = create_output(
        api,
        &pipeline_id,
        "hls-preview",
        &format!("hls://{cfg}-preview"),
        "source",
    )
    .await?;
    start_output(api, &pipeline_id, &hls_output).await?;
    output_ids.push(hls_output.clone());
    env.register_output_cell(HarnessOutputCell {
        scenario_id: cfg.to_string(),
        batch_group: "hls-preview".to_string(),
        wave: 0,
        pipeline_id: pipeline_id.clone(),
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

    if env.check_selected("smoke")
        && resume.allows(&mixed_scenario_check_id(
            cfg,
            "no_early_external_transcoder",
        ))
    {
        let started = Instant::now();
        let launches =
            count_log_matches(&env.restream_log, "[external-transcoder] Launching ffmpeg");
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
        log_mixed_ok(env, "smoke: no external transcoder for source outputs")?;
    }

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
    verify_mixed_graph_stage_sharing(env, api, cfg, &pipeline_id, case, resume).await?;
    if env.needs_live_output_progress_gate() {
        // The live-anchor matrix adds an HLS helper output ahead of the probe
        // rows. Gate the external reads on the actual RTMP/SRT egress rows so
        // the first ffprobe does not race outputs that are still starting up.
        let progress_output_ids = mixed_progress_output_ids(&output_ids, &hls_output);
        wait_for_outputs_progress_with_env(
            api,
            &pipeline_id,
            &progress_output_ids,
            mixed_output_progress_timeout_for_case(case, progress_output_ids.len()),
            Some(env),
        )
        .await?;
    }

    let rss = record_mixed_rss_delta(env, cfg, restream_pid, rss_baseline, total, None).await?;

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

    if env.check_selected("hls") {
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
    }

    // Phase 4: harness sink probe — assert DTS monotonicity, video+audio
    // presence, and keyframe cadence on the live egress.
    let sink_port = harness_port_defaults()
        .sink
        .checked_add(env.sink_port_offset as u16)
        .ok_or("mixed sink probe port overflowed")?;
    let (sink_probe_result, sink_probe_failure) = run_optional_mixed_sink_probe(
        env,
        api,
        &pipeline_id,
        cfg,
        sink_port,
        &mut output_ids,
        resume,
    )
    .await?;

    let mut hls_put_probe_result = None;
    if env.check_selected("hls-put-probe")
        && resume.allows(&mixed_scenario_check_id(cfg, "hls_put"))
    {
        let started = Instant::now();
        let put_port = harness_port_defaults()
            .hls_put
            .checked_add(env.sink_port_offset as u16)
            .ok_or("mixed hls-put probe port overflowed")?;
        match run_hls_put_probe(api, &pipeline_id, cfg, put_port).await {
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
                    pipeline_id: pipeline_id.clone(),
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
                hls_put_probe_result = Some(probe);
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
            }
        }
    }

    let mut burst_graph_result = None;
    if env.check_selected("burst-graph")
        && resume.allows(&mixed_scenario_check_id(cfg, "burst_graph"))
    {
        let started = Instant::now();
        match run_burst_graph_check(api, &pipeline_id).await {
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
                burst_graph_result = Some((passed, summary));
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
            }
        }
    }

    stop_child(&mut publisher).await;
    stop_mixed_outputs(api, &pipeline_id, &output_ids).await;
    let lifecycle_started = Instant::now();
    let lifecycle_result =
        wait_for_outputs_stopped(api, &pipeline_id, &output_ids, Duration::from_secs(60)).await;
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
        &pipeline_id,
        &output_ids,
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
        log_mixed_ok(env, "lifecycle: all outputs stopped and deleted")?;
    } else {
        log_mixed_ok(env, "lifecycle: all outputs stopped and deleted")?;
    }

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
    if let Some(summary) = hls_preview {
        result["hlsPreview"] = summary;
    }
    if let Some(probe) = sink_probe_result {
        result["sinkProbe"] = probe.summary;
        result["sinkProbePassed"] = json!(probe.passed);
    }
    if let Some(probe) = hls_put_probe_result {
        result["hlsPutProbe"] = probe.summary;
        result["hlsPutProbePassed"] = json!(probe.passed);
    }
    if let Some((passed, summary)) = burst_graph_result {
        result["burstGraph"] = summary;
        result["burstGraphPassed"] = json!(passed);
    }
    Ok(result)
}

pub(super) async fn run_mixed_live_config(
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
    let (pipeline_id, stream_key) = create_mixed_pipeline(api, cfg).await?;

    let mut publisher = if case.is_multi_track() {
        spawn_mixed_srt_multi_publisher(env, case, &stream_key).await?
    } else {
        spawn_mixed_live_publisher(env, case, &stream_key).await?
    };
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(45)).await?;
    let mut standby_publisher = if case.has_buffered_standby() {
        let standby = create_backup_input(api, &pipeline_id).await?;
        let publisher = spawn_mixed_standby_publisher(env, case, &standby.stream_key).await?;
        wait_for_input_state(
            api,
            &pipeline_id,
            &standby.id,
            "standby",
            Duration::from_secs(30),
        )
        .await?;
        Some((standby, publisher))
    } else {
        None
    };
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
    let mut next_ffmpeg_srt_sink = 0usize;
    let mut ffmpeg_signal_sinks = Vec::new();
    let mut next_ffmpeg_signal_sink = 0usize;
    if case.is_multi_track() {
        add_mixed_multi_output_cases(
            env,
            api,
            &pipeline_id,
            restream_pid,
            cfg,
            output_cases,
            &mut ffmpeg_srt_sinks,
            &mut next_ffmpeg_srt_sink,
            &mut ffmpeg_signal_sinks,
            &mut next_ffmpeg_signal_sink,
            &mut output_ids,
        )
        .await?;
    } else {
        add_mixed_output_cases(
            env,
            api,
            &pipeline_id,
            restream_pid,
            cfg,
            output_cases,
            &mut ffmpeg_signal_sinks,
            &mut next_ffmpeg_signal_sink,
            &mut output_ids,
        )
        .await?;
    }
    verify_mixed_graph_stage_sharing(env, api, cfg, &pipeline_id, case, resume).await?;
    if !ffmpeg_signal_sinks.is_empty() {
        finish_ffmpeg_signal_sinks(env, &mut ffmpeg_signal_sinks, resume).await?;
    }
    if env.needs_live_output_progress_gate() {
        // Mirror the file-ingest gate: under shared HEVC mixed fanout the last
        // duplicated readers can still be wiring up while the first ffprobe or
        // signal capture starts. Waiting for bytes-out keeps the live matrix
        // from turning a startup lag into a false codec/output failure.
        wait_for_outputs_progress_with_env(
            api,
            &pipeline_id,
            &output_ids,
            mixed_output_progress_timeout_for_case(case, output_ids.len()),
            Some(env),
        )
        .await?;
    }

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
        output_cases,
        resume,
        case.is_multi_track(),
        case.is_multi_track(),
    )
    .await?;

    let sink_port = harness_port_defaults()
        .sink
        .checked_add(env.sink_port_offset as u16)
        .ok_or("mixed sink probe port overflowed")?;
    let (sink_probe_result, sink_probe_failure) = run_optional_mixed_sink_probe(
        env,
        api,
        &pipeline_id,
        cfg,
        sink_port,
        &mut output_ids,
        resume,
    )
    .await?;

    stop_child(&mut publisher).await;
    if let Some((_, standby)) = standby_publisher.as_mut() {
        stop_child(standby).await;
    }
    stop_mixed_outputs(api, &pipeline_id, &output_ids).await;
    wait_for_outputs_stopped(api, &pipeline_id, &output_ids, Duration::from_secs(60)).await?;
    let delete_summary = delete_and_verify_mixed_outputs(
        env,
        api,
        cfg,
        &pipeline_id,
        &output_ids,
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
        "outputMatrix": mixed_output_matrix_json(output_cases),
        "artifacts": {
            "outputsJson": env.outputs_json_path(),
            "artifactIndexJson": env.artifact_index_path(),
        },
        "outputs": env.output_registry_json(),
    });
    if case.is_multi_track() {
        result["audioTracks"] = json!(2);
    }
    if let Some(probe) = sink_probe_result {
        result["sinkProbe"] = probe.summary;
        result["sinkProbePassed"] = json!(probe.passed);
    }
    Ok(result)
}

pub(super) fn mixed_input_fixture(case: MixedInputCase) -> Result<PathBuf, String> {
    let codec = match case.codec() {
        MixedVideoCodec::H264 => "h264",
        MixedVideoCodec::H265 => "h265",
    };
    restream::test_fixtures::av_marker_transport_fixture_for_bframes(
        codec,
        case.is_multi_track(),
        case.fixture_bframe_mode(),
    )
}

pub(super) async fn run_mixed_file_config(
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

    let fixture = mixed_input_fixture(case)?;

    let fixture_name = fixture.file_name().unwrap().to_string_lossy().to_string();
    let media_dest = env.media_dir.join(&fixture_name);
    if !media_dest.exists() {
        std::fs::copy(&fixture, &media_dest).map_err(|e| e.to_string())?;
    }

    let (pipeline_id, stream_key) = create_mixed_pipeline(api, cfg).await?;

    api.put_json(
        &format!("/api/v1/pipelines/{pipeline_id}/file-ingest"),
        json!({"filename": fixture_name, "loop": true}),
    )
    .await?;

    let ingest_list = api.get_json("/api/v1/ingests").await?;
    let ingest_id = ingest_list
        .as_array()
        .and_then(|arr| {
            arr.iter()
                .find(|i| i["streamKey"].as_str() == Some(&stream_key))
        })
        .and_then(|i| i["id"].as_str())
        .ok_or("file ingest not found in list")?
        .to_string();

    api.post_empty(&format!("/api/v1/ingests/{ingest_id}/start"))
        .await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(45)).await?;
    let hls_preview =
        verify_optional_mixed_hls_preview(env, api, cfg, &pipeline_id, case, resume).await?;
    let recording = verify_mixed_recording(env, api, cfg, &pipeline_id, case, resume).await?;
    let rss_baseline = process_rss_kb(restream_pid).await.unwrap_or(0);
    if !env.skip_load {
        snapshot_mixed(
            env,
            restream_pid,
            cfg,
            "baseline (file ingest live, 0 outputs)",
        )
        .await?;
    }

    let mut output_ids = Vec::with_capacity(total);
    let mut ffmpeg_srt_sinks = Vec::new();
    let mut next_ffmpeg_srt_sink = 0usize;
    let mut ffmpeg_signal_sinks = Vec::new();
    let mut next_ffmpeg_signal_sink = 0usize;
    if case.is_multi_track() {
        add_mixed_multi_output_cases(
            env,
            api,
            &pipeline_id,
            restream_pid,
            cfg,
            output_cases,
            &mut ffmpeg_srt_sinks,
            &mut next_ffmpeg_srt_sink,
            &mut ffmpeg_signal_sinks,
            &mut next_ffmpeg_signal_sink,
            &mut output_ids,
        )
        .await?;
    } else {
        add_mixed_output_cases(
            env,
            api,
            &pipeline_id,
            restream_pid,
            cfg,
            output_cases,
            &mut ffmpeg_signal_sinks,
            &mut next_ffmpeg_signal_sink,
            &mut output_ids,
        )
        .await?;
    }
    verify_mixed_graph_stage_sharing(env, api, cfg, &pipeline_id, case, resume).await?;
    if !ffmpeg_signal_sinks.is_empty() {
        finish_ffmpeg_signal_sinks(env, &mut ffmpeg_signal_sinks, resume).await?;
    }
    if env.needs_live_output_progress_gate() {
        // The first resumed external read can arrive while SRT egresses are
        // still in "connecting/stalled" even though they do become healthy a
        // few seconds later. Wait for real bytes-out once per scenario so the
        // first probed cell does not lose signal to a startup race.
        wait_for_outputs_progress_with_env(
            api,
            &pipeline_id,
            &output_ids,
            mixed_output_progress_timeout_for_case(case, output_ids.len()),
            Some(env),
        )
        .await?;
    }

    let duration_secs: u64 = 10;
    if !ffmpeg_srt_sinks.is_empty() {
        finish_ffmpeg_srt_sinks(&mut ffmpeg_srt_sinks).await?;
    }
    verify_mixed_output_cases_inner(
        env,
        api,
        cfg,
        output_cases,
        resume,
        case.is_multi_track(),
        true,
    )
    .await?;

    println!("[{cfg}] sustaining {total} outputs for {duration_secs}s");
    tokio::time::sleep(Duration::from_secs(duration_secs)).await;
    if !env.skip_load {
        snapshot_mixed(
            env,
            restream_pid,
            cfg,
            &format!("after {duration_secs}s sustained"),
        )
        .await?;
    }

    let rss_peak = process_rss_kb(restream_pid).await.unwrap_or(0);
    let growth_kb = rss_peak.saturating_sub(rss_baseline);

    stop_mixed_outputs(api, &pipeline_id, &output_ids).await;
    wait_for_outputs_stopped(api, &pipeline_id, &output_ids, Duration::from_secs(60)).await?;
    delete_and_verify_mixed_outputs(
        env,
        api,
        cfg,
        &pipeline_id,
        &output_ids,
        Duration::from_secs(30),
    )
    .await?;

    api.post_empty(&format!("/api/v1/ingests/{ingest_id}/stop"))
        .await?;

    if env.check_selected("runtime-log") {
        let runtime_log_started = Instant::now();
        verify_mixed_runtime_log_hygiene(env, cfg, &pipeline_id, runtime_log_started.elapsed())?;
    }

    println!(
        "[{cfg}] done: {total} outputs, baseline={rss_baseline}kB peak={rss_peak}kB growth={growth_kb}kB"
    );

    write_mixed_artifact_index(env)?;
    let mut result = json!({
        "scenario": cfg,
        "inputCase": case.scenario_id(),
        "codec": case.codec_name(),
        "trackLayout": case.track_layout_name(),
        "outputCount": total,
        "outputMatrix": mixed_output_matrix_json(output_cases),
        "recording": recording,
        "artifacts": {
            "outputsJson": env.outputs_json_path(),
            "artifactIndexJson": env.artifact_index_path(),
        },
        "outputs": env.output_registry_json(),
        "rssBaselineKb": rss_baseline,
        "rssPeakKb": rss_peak,
        "rssGrowthKb": growth_kb,
    });
    if let Some(summary) = hls_preview {
        result["hlsPreview"] = summary;
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runtime_log_noise_matcher_only_flags_decoder_noise_patterns() {
        assert!(mixed_runtime_log_noise_matches(
            "[hevc @ 0x1] PPS id out of range: 0"
        ));
        assert!(mixed_runtime_log_noise_matches(
            "[hevc @ 0x1] Could not find ref with POC 0"
        ));
        assert!(mixed_runtime_log_noise_matches(
            "[hevc @ 0x1] Error constructing the frame RPS."
        ));
        assert!(!mixed_runtime_log_noise_matches(
            "stage exit pipeline=pipe encoding=720p"
        ));
    }

    #[test]
    fn mixed_env_register_output_cell_writes_outputs_json() {
        let temp = std::env::temp_dir().join(format!(
            "restream-mixed-output-registry-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).expect("temp dir");
        let env = MixedEnv::from_env_with_default_work_dir("mixed.registry", temp.clone());

        env.register_output_cell(HarnessOutputCell {
            scenario_id: "mixed.asset.file.h264.a1.bf0".to_string(),
            batch_group: "rtmp.source".to_string(),
            wave: 0,
            pipeline_id: "pipe".to_string(),
            output_id: "output-1".to_string(),
            output_name: "rtmp.source-1".to_string(),
            cell_id: "rtmp.source".to_string(),
            duplicate_index: 1,
            protocol: "rtmp".to_string(),
            encoding: "source".to_string(),
            rtmp_mode: Some(RtmpOutputMode::Legacy.as_str().to_string()),
            selected_audio_track: None,
            publish_url: "rtmp://127.0.0.1:1935/live/out".to_string(),
            read_url: None,
            expected_dimensions: Some("1920x1080".to_string()),
            expected_audio_tracks: Some(1),
            terminal_stage: None,
        })
        .expect("cell registered");

        let body = std::fs::read_to_string(env.outputs_json_path()).expect("outputs.json");
        let json: Value = serde_json::from_str(&body).expect("valid output registry json");
        assert_eq!(
            json["schemaVersion"],
            mixed_artifacts::MIXED_OUTPUTS_SCHEMA_VERSION
        );
        assert_eq!(json["outputs"][0]["outputId"], "output-1");
        assert_eq!(
            env.output_cell_label("output-1").expect("cell label"),
            "mixed.asset.file.h264.a1.bf0 / rtmp.source / out1"
        );

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn matrix_progress_writes_root_cause_summary_artifact() {
        let temp = std::env::temp_dir().join(format!(
            "restream-mixed-root-cause-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).expect("temp dir");
        let scenario_path = temp.join("scenario.json");
        let rows = matrix_case_progress_rows();
        let failures = vec![
            "mixed input case mixed.live.rtmp.h265.a2.bf2 failed: stream 0 has DTS gap 0.900000s"
                .to_string(),
        ];

        write_matrix_scenario_progress(&scenario_path, "shared-batch", false, &rows, &failures)
            .expect("progress json");

        let scenario_body = std::fs::read_to_string(&scenario_path).expect("scenario.json");
        let scenario: Value = serde_json::from_str(&scenario_body).expect("valid scenario json");
        assert_eq!(
            scenario["rootCauseSummary"]["causes"][0]["cause"],
            "timestamp_discontinuity"
        );
        assert_eq!(
            scenario["caseProgress"][0]["hlsPreviewTiming"],
            "before-fanout"
        );
        assert_eq!(
            scenario["caseProgress"][0]["probeSampling"]["policy"],
            "last-duplicate"
        );
        assert_eq!(
            scenario["caseProgress"][0]["supportedHlsPreviewTimings"],
            json!(["before-fanout", "after-progress", "disabled"])
        );
        assert_eq!(
            scenario["caseProgress"][0]["supportedProbeSamplingPolicies"],
            json!([
                "all-duplicates",
                "first-duplicate",
                "last-duplicate",
                "representative"
            ])
        );
        assert_eq!(
            scenario["artifacts"]["rootCauseSummaryJson"],
            temp.join("root-cause-summary.json")
                .to_string_lossy()
                .as_ref()
        );
        assert_eq!(
            scenario["artifacts"]["artifactIndexJson"],
            temp.join("artifact-index.json").to_string_lossy().as_ref()
        );

        let summary_body = std::fs::read_to_string(temp.join("root-cause-summary.json"))
            .expect("root cause summary");
        let summary: Value = serde_json::from_str(&summary_body).expect("valid summary json");
        assert_eq!(summary["totalFailures"], 1);
        assert_eq!(
            summary["causes"][0]["scenarios"][0],
            "mixed.live.rtmp.h265.a2.bf2"
        );
        let index_body =
            std::fs::read_to_string(temp.join("artifact-index.json")).expect("artifact index");
        let index: Value = serde_json::from_str(&index_body).expect("valid artifact index");
        assert_eq!(index["mode"], MIXED_MATRIX_MODE);
        assert_eq!(index["scenarioJson"], json!(scenario_path));
        assert_eq!(
            index["rootCauseSummaryJson"],
            json!(temp.join("root-cause-summary.json"))
        );
        assert_eq!(
            index["cases"][0]["artifactIndexJson"],
            json!(temp.join("asset/file/h264/a1/bf0/artifact-index.json"))
        );
        assert_eq!(
            index["cases"][0]["outputsJson"],
            json!(temp.join("asset/file/h264/a1/bf0/outputs.json"))
        );
        assert_eq!(
            index["cases"][0]["sqliteSnapshotDir"],
            json!(temp.join("asset/file/h264/a1/bf0/sqlite-snapshot"))
        );
        assert_eq!(
            index["cases"][0]["media"],
            json!(temp.join("asset/file/h264/a1/bf0/media"))
        );

        std::fs::remove_dir_all(temp).ok();
    }

    #[test]
    fn runtime_log_noise_scan_scopes_to_pipeline_id() {
        let temp = std::env::temp_dir().join(format!(
            "restream-mixed-runtime-log-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .expect("unix epoch")
                .as_nanos()
        ));
        std::fs::create_dir_all(&temp).expect("temp dir");
        let log_path = temp.join("restream.log");
        std::fs::write(
            &log_path,
            concat!(
                "INFO pipeline_id=pipe-ok normal line\n",
                "ERROR pipeline_id=pipe-bad [hevc @ 0x1] PPS id out of range: 0\n",
                "ERROR pipeline_id=pipe-other [hevc @ 0x1] PPS id out of range: 0\n"
            ),
        )
        .expect("log write");

        let matches = mixed_runtime_log_noise_lines(&log_path, "pipe-bad");
        assert_eq!(matches.len(), 1);
        assert!(matches[0].contains("pipe-bad"));

        std::fs::remove_file(&log_path).ok();
        std::fs::remove_dir_all(&temp).ok();
    }
}
