//! Mixed-matrix runtime, sinks, probes, and assertion helpers.

use super::*;

#[path = "mixed_checks.rs"]
mod mixed_checks;
#[path = "mixed_control.rs"]
mod mixed_control;
#[path = "mixed_lifecycle.rs"]
mod mixed_lifecycle;
#[path = "mixed_outputs.rs"]
mod mixed_outputs;
#[path = "mixed_playback.rs"]
mod mixed_playback;
#[path = "mixed_probes.rs"]
mod mixed_probes;
#[path = "mixed_reporting.rs"]
mod mixed_reporting;
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

pub(super) use mixed_checks::{verify_mixed_output_cases_inner, verify_mixed_output_dimensions};
pub(super) use mixed_control::{
    MixedResume, mixed_output_checks_need_live_progress_gate, mixed_output_progress_timeout,
    mixed_progress_output_ids,
};
pub(super) use mixed_lifecycle::{stop_mixed_outputs, wait_for_outputs_stopped};
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
    count_log_matches, emit_mixed_result, emit_mixed_timing, emit_mixed_timing_window,
    file_tail_lines, log_mixed_ok, safe_artifact_stem,
};
pub(super) use mixed_runtime::{
    spawn_mixed_live_publisher, spawn_mixed_srt_multi_publisher, start_mixed_mediamtx,
    start_mixed_restream,
};
#[cfg(test)]
pub(super) use mixed_signal::{
    PcmQualityReport, analyze_pcm_s16le, marker_gaps_from_intervals, max_audio_pts_gap_ms,
    nearest_marker_offsets_ms, parse_blackdetect_intervals, parse_silencedetect_intervals,
    validate_signal_quality,
};
pub(super) use mixed_signal::{
    SignalTolerances, decode_pcm_quality, run_ffmpeg_filter_log, signal_report_json,
    validate_signal_quality_with_tolerances, verify_mixed_signal_quality,
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
pub(super) use output_helpers::{create_output, start_output};

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
            assertion_log: std::env::var_os("ASSERTION_LOG")
                .filter(|value| !value.is_empty())
                .map(PathBuf::from),
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
        return mixed_input_matrix_correctness_serial().await;
    }

    mixed_input_matrix_correctness_shared().await
}

fn mixed_matrix_fail_fast() -> bool {
    std::env::var("MIXED_MATRIX_FAIL_FAST")
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

fn mixed_matrix_default_check_names() -> Vec<String> {
    mixed_default_checks()
        .iter()
        .map(|check| check.as_str().to_string())
        .collect()
}

fn apply_mixed_matrix_defaults(
    env: &mut MixedEnv,
    default_checks: Option<&[String]>,
    default_assertion_log: Option<&Path>,
    explicit_collect_failures: bool,
) {
    if let Some(default_checks) = default_checks {
        env.only_checks = Some(default_checks.to_vec());
    }
    if !explicit_collect_failures {
        // Full matrix should continue collecting evidence across scenarios.
        env.collect_failures = true;
    }
    if let Some(assertion_log) = default_assertion_log {
        env.assertion_log = Some(assertion_log.to_path_buf());
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum MatrixCaseState {
    Pending,
    InProgress,
    Passed,
    Failed,
}

impl MatrixCaseState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::InProgress => "in-progress",
            Self::Passed => "passed",
            Self::Failed => "failed",
        }
    }

    fn is_completed(self) -> bool {
        matches!(self, Self::Passed | Self::Failed)
    }
}

#[derive(Clone)]
struct MatrixCaseProgress {
    case: MixedInputCase,
    batch_group: MixedSharedBatchGroup,
    output_cells: usize,
    state: MatrixCaseState,
    wave: Option<usize>,
    error: Option<String>,
}

fn matrix_case_progress_rows() -> Vec<MatrixCaseProgress> {
    mixed_input_cases()
        .iter()
        .copied()
        .map(|case| MatrixCaseProgress {
            case,
            batch_group: case.shared_batch_group(),
            output_cells: mixed_output_cases_for_input(case).len(),
            state: MatrixCaseState::Pending,
            wave: None,
            error: None,
        })
        .collect()
}

fn matrix_mark_case_state(
    rows: &mut [MatrixCaseProgress],
    case: MixedInputCase,
    state: MatrixCaseState,
    wave: Option<usize>,
    error: Option<String>,
) {
    if let Some(row) = rows.iter_mut().find(|row| row.case == case) {
        row.state = state;
        row.wave = wave;
        row.error = error;
    }
}

fn matrix_progress_totals(rows: &[MatrixCaseProgress]) -> Value {
    let total_cases = rows.len();
    let completed_cases = rows.iter().filter(|row| row.state.is_completed()).count();
    let in_progress_cases = rows
        .iter()
        .filter(|row| row.state == MatrixCaseState::InProgress)
        .count();
    let failed_cases = rows
        .iter()
        .filter(|row| row.state == MatrixCaseState::Failed)
        .count();
    let pending_cases = total_cases.saturating_sub(completed_cases + in_progress_cases);

    let total_cells = rows.iter().map(|row| row.output_cells).sum::<usize>();
    let completed_cells = rows
        .iter()
        .filter(|row| row.state.is_completed())
        .map(|row| row.output_cells)
        .sum::<usize>();
    let in_progress_cells = rows
        .iter()
        .filter(|row| row.state == MatrixCaseState::InProgress)
        .map(|row| row.output_cells)
        .sum::<usize>();
    let pending_cells = total_cells.saturating_sub(completed_cells + in_progress_cells);

    json!({
        "totalCases": total_cases,
        "completedCases": completed_cases,
        "inProgressCases": in_progress_cases,
        "failedCases": failed_cases,
        "pendingCases": pending_cases,
        "totalCells": total_cells,
        "completedCells": completed_cells,
        "inProgressCells": in_progress_cells,
        "pendingCells": pending_cells,
    })
}

fn matrix_case_progress_json(rows: &[MatrixCaseProgress]) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            json!({
                "id": row.case.scenario_id(),
                "status": row.state.as_str(),
                "batchGroup": row.batch_group.as_str(),
                "wave": row.wave,
                "outputCells": row.output_cells,
                "error": row.error,
            })
        })
        .collect()
}

fn write_json_pretty_atomic(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let tmp_path = path.with_extension("json.tmp");
    let payload = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    std::fs::write(&tmp_path, payload).map_err(|error| error.to_string())?;
    std::fs::rename(tmp_path, path).map_err(|error| error.to_string())
}

fn write_matrix_scenario_progress(
    path: &Path,
    execution: &str,
    fail_fast: bool,
    rows: &[MatrixCaseProgress],
    failures: &[String],
) -> Result<(), String> {
    let progress = matrix_progress_totals(rows);
    let total_cases = progress["totalCases"].as_u64().unwrap_or(0);
    let completed_cases = progress["completedCases"].as_u64().unwrap_or(0);
    let execution_state = if completed_cases < total_cases {
        "running"
    } else {
        "completed"
    };
    let passed = completed_cases == total_cases && failures.is_empty();

    write_json_pretty_atomic(
        path,
        &json!({
            "mode": MIXED_MATRIX_MODE,
            "execution": execution,
            "executionState": execution_state,
            "passed": passed,
            "progress": progress,
            "failures": failures,
            "continueOnScenarioFailure": !fail_fast,
            "failFastOptOutEnv": "MIXED_MATRIX_FAIL_FAST",
            "caseProgress": matrix_case_progress_json(rows),
            "updatedAt": Utc::now().to_rfc3339(),
        }),
    )
}

async fn mixed_input_matrix_correctness_serial() -> Result<Value, String> {
    let root = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(mixed_matrix_default_work_dir);
    let scenario_path = root.join("scenario.json");
    let explicit_only_checks = std::env::var_os("ONLY_CHECKS").is_some();
    let explicit_collect_failures = std::env::var_os("COLLECT_FAILURES").is_some();
    let explicit_assertion_log = std::env::var_os("ASSERTION_LOG").is_some();
    let fail_fast = mixed_matrix_fail_fast();
    let default_checks = (!explicit_only_checks).then(mixed_matrix_default_check_names);
    let default_assertion_log = (!explicit_assertion_log).then(|| root.join("assertions.jsonl"));
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut case_progress = matrix_case_progress_rows();
    let mut covered_output_cells = 0usize;
    let total_output_cells: usize = mixed_input_cases()
        .iter()
        .map(|case| mixed_output_cases_for_input(*case).len())
        .sum();
    write_matrix_scenario_progress(
        &scenario_path,
        "serial",
        fail_fast,
        &case_progress,
        &failures,
    )?;
    for (wave_index, case) in mixed_input_cases().iter().copied().enumerate() {
        let mode = mixed_input_mode_name(case);
        let mut env =
            MixedEnv::from_env_with_default_work_dir(&mode, root.join(case.artifact_rel_dir()));
        apply_mixed_matrix_defaults(
            &mut env,
            default_checks.as_deref(),
            default_assertion_log.as_deref(),
            explicit_collect_failures,
        );
        covered_output_cells += mixed_output_cases_for_input(case).len();
        matrix_mark_case_state(
            &mut case_progress,
            case,
            MatrixCaseState::InProgress,
            Some(wave_index + 1),
            None,
        );
        write_matrix_scenario_progress(
            &scenario_path,
            "serial",
            fail_fast,
            &case_progress,
            &failures,
        )?;
        match run_mixed_input_case_with_env(case, env).await {
            Ok(result) => results.push(result),
            Err(error) => {
                let failure = format!("mixed input case {} failed: {error}", case.scenario_id());
                matrix_mark_case_state(
                    &mut case_progress,
                    case,
                    MatrixCaseState::Failed,
                    Some(wave_index + 1),
                    Some(error.clone()),
                );
                if fail_fast {
                    failures.push(failure.clone());
                    write_matrix_scenario_progress(
                        &scenario_path,
                        "serial",
                        fail_fast,
                        &case_progress,
                        &failures,
                    )?;
                    return Err(failure);
                }
                failures.push(failure);
                write_matrix_scenario_progress(
                    &scenario_path,
                    "serial",
                    fail_fast,
                    &case_progress,
                    &failures,
                )?;
                continue;
            }
        }
        matrix_mark_case_state(
            &mut case_progress,
            case,
            MatrixCaseState::Passed,
            Some(wave_index + 1),
            None,
        );
        write_matrix_scenario_progress(
            &scenario_path,
            "serial",
            fail_fast,
            &case_progress,
            &failures,
        )?;
    }
    let progress = matrix_progress_totals(&case_progress);

    Ok(json!({
        "passed": failures.is_empty(),
        "mode": MIXED_MATRIX_MODE,
        "progress": progress,
        "caseProgress": matrix_case_progress_json(&case_progress),
        "coverage": {
            "selectedInputCases": mixed_input_cases().len(),
            "totalInputCases": mixed_input_cases().len(),
            "selectedOutputCells": covered_output_cells,
            "totalOutputCells": total_output_cells,
            "execution": "serial",
            "defaultExecution": "shared-batch",
            "forcedSerial": true,
            "serialOptOutEnv": "MIXED_MATRIX_SERIAL",
            "continueOnScenarioFailure": !fail_fast,
            "failFastOptOutEnv": "MIXED_MATRIX_FAIL_FAST",
            "defaultCollectFailures": !explicit_collect_failures,
            "defaultAssertionLog": if explicit_assertion_log {
                Value::Null
            } else {
                root.join("assertions.jsonl").to_string_lossy().into_owned().into()
            },
            "defaultChecks": if explicit_only_checks {
                Value::Null
            } else {
                mixed_default_checks()
                    .iter()
                    .map(|check| check.as_str())
                    .collect::<Vec<_>>()
                    .into()
            },
            "sharedBatchGroups": ["live-rtmp", "live-srt", "file-ingest"],
        },
        "inputCases": mixed_input_cases().iter().map(|case| {
            json!({
                "id": case.scenario_id(),
                "source": case.source_name(),
                "ingest": case.ingest_name(),
                "video": case.codec_name(),
                "audio": case.audio_layout_name(),
                "reorder": case.reorder_name(),
                "sourceHasBframes": case.source_has_b_frames(),
            })
        }).collect::<Vec<_>>(),
        "failures": failures,
        "results": results,
    }))
}

async fn mixed_input_matrix_correctness_shared() -> Result<Value, String> {
    let root = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(mixed_matrix_default_work_dir);
    let scenario_path = root.join("scenario.json");
    let explicit_only_checks = std::env::var_os("ONLY_CHECKS").is_some();
    let explicit_collect_failures = std::env::var_os("COLLECT_FAILURES").is_some();
    let explicit_assertion_log = std::env::var_os("ASSERTION_LOG").is_some();
    let fail_fast = mixed_matrix_fail_fast();
    let default_checks = (!explicit_only_checks).then(mixed_matrix_default_check_names);
    let default_assertion_log = (!explicit_assertion_log).then(|| root.join("assertions.jsonl"));
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut case_progress = matrix_case_progress_rows();
    let mut covered_output_cells = 0usize;
    let total_output_cells: usize = mixed_input_cases()
        .iter()
        .map(|case| mixed_output_cases_for_input(*case).len())
        .sum();
    let batch_groups = [
        MixedSharedBatchGroup::LiveRtmp,
        MixedSharedBatchGroup::LiveSrt,
        MixedSharedBatchGroup::FileIngest,
    ];
    write_matrix_scenario_progress(
        &scenario_path,
        "shared-batch",
        fail_fast,
        &case_progress,
        &failures,
    )?;

    for group in batch_groups {
        let failures_before_group = failures.len();
        let cases: Vec<_> = mixed_input_cases()
            .iter()
            .copied()
            .filter(|case| case.shared_batch_group() == group)
            .collect();
        if cases.is_empty() {
            continue;
        }

        let stack_mode = format!("{MIXED_MATRIX_MODE}.{}", group.as_str());
        let mut stack_env = MixedEnv::from_env_with_default_work_dir(
            &stack_mode,
            root.join("_shared").join(group.as_str()),
        );
        apply_mixed_matrix_defaults(
            &mut stack_env,
            default_checks.as_deref(),
            default_assertion_log.as_deref(),
            explicit_collect_failures,
        );
        let mut stack = start_mixed_harness_stack(stack_env).await?;
        let mut stack_stopped = false;
        let wave_started = Instant::now();

        let mut cases_queue: VecDeque<MixedInputCase> = cases.iter().copied().collect();
        let mut wave_index = 0usize;
        while let Some(case_a) = cases_queue.pop_front() {
            wave_index += 1;
            let case_b = cases_queue.pop_front();

            matrix_mark_case_state(
                &mut case_progress,
                case_a,
                MatrixCaseState::InProgress,
                Some(wave_index),
                None,
            );
            if let Some(case_b) = case_b {
                matrix_mark_case_state(
                    &mut case_progress,
                    case_b,
                    MatrixCaseState::InProgress,
                    Some(wave_index),
                    None,
                );
            }
            write_matrix_scenario_progress(
                &scenario_path,
                "shared-batch",
                fail_fast,
                &case_progress,
                &failures,
            )?;

            let mut env_a = MixedEnv::from_env_with_default_work_dir(
                case_a.scenario_id(),
                root.join(case_a.artifact_rel_dir()),
            );
            bind_mixed_env_to_shared_stack(&mut env_a, &stack.env);
            env_a.sink_port_offset = 0;
            apply_mixed_matrix_defaults(
                &mut env_a,
                default_checks.as_deref(),
                default_assertion_log.as_deref(),
                explicit_collect_failures,
            );
            covered_output_cells += mixed_output_cases_for_input(case_a).len();
            let mut wave_failed = false;

            if let Some(case_b) = case_b {
                let mut env_b = MixedEnv::from_env_with_default_work_dir(
                    case_b.scenario_id(),
                    root.join(case_b.artifact_rel_dir()),
                );
                bind_mixed_env_to_shared_stack(&mut env_b, &stack.env);
                env_b.sink_port_offset = 1;
                env_b.ffmpeg_signal_sink_base = stack
                    .env
                    .ffmpeg_signal_sink_base
                    .checked_add(128)
                    .ok_or("mixed ffmpeg signal sink base overflowed")?;
                env_b.ffmpeg_srt_sink_base = stack
                    .env
                    .ffmpeg_srt_sink_base
                    .checked_add(128)
                    .ok_or("mixed ffmpeg srt sink base overflowed")?;
                apply_mixed_matrix_defaults(
                    &mut env_b,
                    default_checks.as_deref(),
                    default_assertion_log.as_deref(),
                    explicit_collect_failures,
                );
                covered_output_cells += mixed_output_cases_for_input(case_b).len();

                let (result_a, result_b) = tokio::join!(
                    run_mixed_input_case_on_active_stack(
                        case_a,
                        env_a,
                        &stack.api,
                        stack.restream_pid,
                    ),
                    run_mixed_input_case_on_active_stack(
                        case_b,
                        env_b,
                        &stack.api,
                        stack.restream_pid,
                    ),
                );
                let mut fail_fast_error = None;
                for (case, result) in [(case_a, result_a), (case_b, result_b)] {
                    match result {
                        Ok(mut value) => {
                            value["batchGroup"] = json!(group.as_str());
                            value["wave"] = json!(wave_index);
                            results.push(value);
                            matrix_mark_case_state(
                                &mut case_progress,
                                case,
                                MatrixCaseState::Passed,
                                Some(wave_index),
                                None,
                            );
                        }
                        Err(error) => {
                            let failure =
                                format!("mixed input case {} failed: {error}", case.scenario_id());
                            wave_failed = true;
                            failures.push(failure.clone());
                            matrix_mark_case_state(
                                &mut case_progress,
                                case,
                                MatrixCaseState::Failed,
                                Some(wave_index),
                                Some(error),
                            );
                            if fail_fast && fail_fast_error.is_none() {
                                fail_fast_error = Some(failure);
                            }
                        }
                    }
                }
                write_matrix_scenario_progress(
                    &scenario_path,
                    "shared-batch",
                    fail_fast,
                    &case_progress,
                    &failures,
                )?;
                if let Some(failure) = fail_fast_error {
                    stop_mixed_harness_stack(&mut stack).await;
                    return Err(failure);
                }
            } else {
                let mut fail_fast_error = None;
                match run_mixed_input_case_on_active_stack(
                    case_a,
                    env_a,
                    &stack.api,
                    stack.restream_pid,
                )
                .await
                {
                    Ok(mut value) => {
                        value["batchGroup"] = json!(group.as_str());
                        value["wave"] = json!(wave_index);
                        results.push(value);
                        matrix_mark_case_state(
                            &mut case_progress,
                            case_a,
                            MatrixCaseState::Passed,
                            Some(wave_index),
                            None,
                        );
                    }
                    Err(error) => {
                        let failure =
                            format!("mixed input case {} failed: {error}", case_a.scenario_id());
                        wave_failed = true;
                        failures.push(failure.clone());
                        matrix_mark_case_state(
                            &mut case_progress,
                            case_a,
                            MatrixCaseState::Failed,
                            Some(wave_index),
                            Some(error),
                        );
                        if fail_fast {
                            fail_fast_error = Some(failure);
                        }
                    }
                }
                write_matrix_scenario_progress(
                    &scenario_path,
                    "shared-batch",
                    fail_fast,
                    &case_progress,
                    &failures,
                )?;
                if let Some(failure) = fail_fast_error {
                    stop_mixed_harness_stack(&mut stack).await;
                    return Err(failure);
                }
            }

            if wave_failed {
                stop_mixed_harness_stack(&mut stack).await;
                stack_stopped = true;
                if !cases_queue.is_empty() {
                    let mut restarted_stack_env = MixedEnv::from_env_with_default_work_dir(
                        &stack_mode,
                        root.join("_shared").join(group.as_str()),
                    );
                    apply_mixed_matrix_defaults(
                        &mut restarted_stack_env,
                        default_checks.as_deref(),
                        default_assertion_log.as_deref(),
                        explicit_collect_failures,
                    );
                    stack = start_mixed_harness_stack(restarted_stack_env).await?;
                    stack_stopped = false;
                }
            }
        }

        emit_mixed_timing(
            &stack.env,
            MIXED_MATRIX_MODE,
            &format!("batch.{}", group.as_str()),
            if failures.len() == failures_before_group {
                "pass"
            } else {
                "fail"
            },
            wave_started.elapsed(),
            Some(json!({
                "group": group.as_str(),
                "cases": cases.iter().map(|case| case.scenario_id()).collect::<Vec<_>>(),
            })),
        )?;
        if !stack_stopped {
            stop_mixed_harness_stack(&mut stack).await;
        }
    }
    let progress = matrix_progress_totals(&case_progress);

    Ok(json!({
        "passed": failures.is_empty(),
        "mode": MIXED_MATRIX_MODE,
        "progress": progress,
        "caseProgress": matrix_case_progress_json(&case_progress),
        "coverage": {
            "selectedInputCases": mixed_input_cases().len(),
            "totalInputCases": mixed_input_cases().len(),
            "selectedOutputCells": covered_output_cells,
            "totalOutputCells": total_output_cells,
            "execution": "shared-batch",
            "defaultExecution": "shared-batch",
            "forcedSerial": false,
            "serialOptOutEnv": "MIXED_MATRIX_SERIAL",
            "continueOnScenarioFailure": !fail_fast,
            "failFastOptOutEnv": "MIXED_MATRIX_FAIL_FAST",
            "defaultCollectFailures": !explicit_collect_failures,
            "defaultAssertionLog": if explicit_assertion_log {
                Value::Null
            } else {
                root.join("assertions.jsonl").to_string_lossy().into_owned().into()
            },
            "defaultChecks": if explicit_only_checks {
                Value::Null
            } else {
                mixed_default_checks()
                    .iter()
                    .map(|check| check.as_str())
                    .collect::<Vec<_>>()
                    .into()
            },
            "sharedBatchGroups": ["live-rtmp", "live-srt", "file-ingest"],
            "sharedBatches": [
                {
                    "group": "live-rtmp",
                    "maxConcurrentPipelines": 2,
                    "cases": mixed_input_cases()
                        .iter()
                        .filter(|case| case.shared_batch_group() == MixedSharedBatchGroup::LiveRtmp)
                        .map(|case| case.scenario_id())
                        .collect::<Vec<_>>(),
                },
                {
                    "group": "live-srt",
                    "maxConcurrentPipelines": 2,
                    "cases": mixed_input_cases()
                        .iter()
                        .filter(|case| case.shared_batch_group() == MixedSharedBatchGroup::LiveSrt)
                        .map(|case| case.scenario_id())
                        .collect::<Vec<_>>(),
                },
                {
                    "group": "file-ingest",
                    "maxConcurrentPipelines": 2,
                    "cases": mixed_input_cases()
                        .iter()
                        .filter(|case| case.shared_batch_group() == MixedSharedBatchGroup::FileIngest)
                        .map(|case| case.scenario_id())
                        .collect::<Vec<_>>(),
                }
            ],
        },
        "inputCases": mixed_input_cases().iter().map(|case| {
            json!({
                "id": case.scenario_id(),
                "source": case.source_name(),
                "ingest": case.ingest_name(),
                "video": case.codec_name(),
                "audio": case.audio_layout_name(),
                "reorder": case.reorder_name(),
                "sourceHasBframes": case.source_has_b_frames(),
            })
        }).collect::<Vec<_>>(),
        "failures": failures,
        "results": results,
    }))
}

pub(super) async fn mixed_fast_breadth_correctness() -> Result<Value, String> {
    let root = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(mixed_fast_breadth_default_work_dir);
    let selected_batches = selected_mixed_fast_breadth_batches()?;
    let explicit_n_per_group = std::env::var_os("N_PER_GROUP").is_some();
    let explicit_only_checks = std::env::var_os("ONLY_CHECKS").is_some();
    let explicit_skip_load = std::env::var_os("SKIP_LOAD").is_some();
    let explicit_collect_failures = std::env::var_os("COLLECT_FAILURES").is_some();
    let explicit_assertion_log = std::env::var_os("ASSERTION_LOG").is_some();
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut covered_output_cells = 0usize;
    let mut total_output_cells = 0usize;

    let selected_cases: Vec<MixedInputCase> = selected_batches
        .iter()
        .flat_map(|batch| batch.cases.iter().copied())
        .collect();

    for case in &selected_cases {
        total_output_cells += mixed_output_cases_for_input(*case).len();
    }

    for batch in &selected_batches {
        let stack_mode = format!("{MIXED_FAST_BREADTH_MODE}.{}", batch.group.as_str());
        let stack_env = MixedEnv::from_env_with_default_work_dir(
            &stack_mode,
            root.join("_shared").join(batch.group.as_str()),
        );
        let mut stack = start_mixed_harness_stack(stack_env).await?;

        let wave_started = Instant::now();
        let mut batch_cases = batch.cases.iter().copied();
        let mut wave_index = 0usize;
        while let Some(case_a) = batch_cases.next() {
            wave_index += 1;
            let case_b = batch_cases.next();

            let selected_a = mixed_fast_breadth_selected(case_a);
            let mut env_a = MixedEnv::from_env_with_default_work_dir(
                case_a.scenario_id(),
                root.join(case_a.artifact_rel_dir()),
            );
            bind_mixed_env_to_shared_stack(&mut env_a, &stack.env);
            env_a.sink_port_offset = 0;
            if !explicit_n_per_group {
                env_a.n_per_group = 1;
            }
            if !explicit_only_checks {
                env_a.only_checks = Some(
                    selected_a
                        .check_names()
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                );
            }
            if !explicit_skip_load {
                env_a.skip_load = true;
            }
            if !explicit_collect_failures {
                env_a.collect_failures = true;
            }
            if !explicit_assertion_log {
                env_a.assertion_log = Some(root.join("assertions.jsonl"));
            }
            covered_output_cells += mixed_output_cases_for_input(case_a).len();

            if let Some(case_b) = case_b {
                let selected_b = mixed_fast_breadth_selected(case_b);
                let mut env_b = MixedEnv::from_env_with_default_work_dir(
                    case_b.scenario_id(),
                    root.join(case_b.artifact_rel_dir()),
                );
                bind_mixed_env_to_shared_stack(&mut env_b, &stack.env);
                env_b.sink_port_offset = 1;
                env_b.ffmpeg_signal_sink_base = stack
                    .env
                    .ffmpeg_signal_sink_base
                    .checked_add(128)
                    .ok_or("mixed ffmpeg signal sink base overflowed")?;
                env_b.ffmpeg_srt_sink_base = stack
                    .env
                    .ffmpeg_srt_sink_base
                    .checked_add(128)
                    .ok_or("mixed ffmpeg srt sink base overflowed")?;
                if !explicit_n_per_group {
                    env_b.n_per_group = 1;
                }
                if !explicit_only_checks {
                    env_b.only_checks = Some(
                        selected_b
                            .check_names()
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                    );
                }
                if !explicit_skip_load {
                    env_b.skip_load = true;
                }
                if !explicit_collect_failures {
                    env_b.collect_failures = true;
                }
                if !explicit_assertion_log {
                    env_b.assertion_log = Some(root.join("assertions.jsonl"));
                }
                covered_output_cells += mixed_output_cases_for_input(case_b).len();

                let (result_a, result_b) = tokio::join!(
                    run_mixed_input_case_on_active_stack(
                        case_a,
                        env_a,
                        &stack.api,
                        stack.restream_pid,
                    ),
                    run_mixed_input_case_on_active_stack(
                        case_b,
                        env_b,
                        &stack.api,
                        stack.restream_pid,
                    ),
                );
                for (case, selected, result) in [
                    (case_a, selected_a, result_a),
                    (case_b, selected_b, result_b),
                ] {
                    match result {
                        Ok(mut value) => {
                            value["fastBreadthRationale"] = json!(selected.rationale);
                            value["batchGroup"] = json!(batch.group.as_str());
                            value["wave"] = json!(wave_index);
                            results.push(value);
                        }
                        Err(error) => failures.push(format!("{}: {error}", case.scenario_id())),
                    }
                }
            } else {
                match run_mixed_input_case_on_active_stack(
                    case_a,
                    env_a,
                    &stack.api,
                    stack.restream_pid,
                )
                .await
                {
                    Ok(mut value) => {
                        value["fastBreadthRationale"] = json!(selected_a.rationale);
                        value["batchGroup"] = json!(batch.group.as_str());
                        value["wave"] = json!(wave_index);
                        results.push(value);
                    }
                    Err(error) => failures.push(format!("{}: {error}", case_a.scenario_id())),
                }
            }
        }
        emit_mixed_timing(
            &stack.env,
            MIXED_FAST_BREADTH_MODE,
            &format!("batch.{}", batch.group.as_str()),
            if failures.is_empty() { "pass" } else { "fail" },
            wave_started.elapsed(),
            Some(json!({
                "group": batch.group.as_str(),
                "cases": batch.cases.iter().map(|case| case.scenario_id()).collect::<Vec<_>>(),
            })),
        )?;
        stop_mixed_harness_stack(&mut stack).await;
    }

    if !failures.is_empty() {
        return Err(format!(
            "mixed fast breadth failed {} selected case(s): {}",
            failures.len(),
            failures.join(" | ")
        ));
    }

    Ok(json!({
        "passed": true,
        "mode": MIXED_FAST_BREADTH_MODE,
        "coverage": {
            "selectedInputCases": selected_cases.len(),
            "totalInputCases": mixed_input_cases().len(),
            "selectedOutputCells": covered_output_cells,
            "totalOutputCells": total_output_cells,
            "nPerGroup": std::env::var("N_PER_GROUP").ok().unwrap_or_else(|| "1".to_string()),
            "selectedBatchGroups": selected_batches
                .iter()
                .map(|batch| batch.group.as_str())
                .collect::<Vec<_>>(),
            "defaultChecks": if explicit_only_checks {
                Value::Null
            } else {
                selected_cases.iter().map(|case| {
                    let selected = mixed_fast_breadth_selected(*case);
                    json!({
                        "id": case.scenario_id(),
                        "checks": selected.check_names(),
                    })
                }).collect::<Vec<_>>().into()
            },
            "defaultSkipLoad": !explicit_skip_load,
            "defaultCollectFailures": !explicit_collect_failures,
            "defaultAssertionLog": if explicit_assertion_log {
                Value::Null
            } else {
                root.join("assertions.jsonl").to_string_lossy().into_owned().into()
            },
            "sharedBatches": selected_batches.iter().map(|batch| {
                json!({
                    "group": batch.group.as_str(),
                    "cases": batch.cases.iter().map(|case| case.scenario_id()).collect::<Vec<_>>(),
                    "maxConcurrentPipelines": batch.cases.len().min(2),
                })
            }).collect::<Vec<_>>(),
        },
        "inputCases": selected_cases.iter().map(|case| {
            let selected = mixed_fast_breadth_selected(*case);
            json!({
                "id": case.scenario_id(),
                "source": case.source_name(),
                "ingest": case.ingest_name(),
                "video": case.codec_name(),
                "audio": case.audio_layout_name(),
                "reorder": case.reorder_name(),
                "sourceHasBframes": case.source_has_b_frames(),
                "outputCells": mixed_output_cases_for_input(*case).len(),
                "checks": selected.check_names(),
                "rationale": selected.rationale,
            })
        }).collect::<Vec<_>>(),
        "results": results,
    }))
}

pub(super) async fn mixed_signal_correctness() -> Result<Value, String> {
    let root = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(mixed_signal_default_work_dir);
    let selected_batches = selected_mixed_signal_batches()?;
    let explicit_n_per_group = std::env::var_os("N_PER_GROUP").is_some();
    let explicit_only_checks = std::env::var_os("ONLY_CHECKS").is_some();
    let explicit_skip_load = std::env::var_os("SKIP_LOAD").is_some();
    let explicit_collect_failures = std::env::var_os("COLLECT_FAILURES").is_some();
    let explicit_assertion_log = std::env::var_os("ASSERTION_LOG").is_some();
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut covered_output_cells = 0usize;
    let mut total_output_cells = 0usize;

    let selected_cases: Vec<MixedInputCase> = selected_batches
        .iter()
        .flat_map(|batch| batch.cases.iter().copied())
        .collect();

    for case in &selected_cases {
        total_output_cells += mixed_output_cases_for_input(*case).len();
    }

    for batch in &selected_batches {
        let stack_mode = format!("{MIXED_SIGNAL_MODE}.{}", batch.group.as_str());
        let stack_env = MixedEnv::from_env_with_default_work_dir(
            &stack_mode,
            root.join("_shared").join(batch.group.as_str()),
        );
        let mut stack = start_mixed_harness_stack(stack_env).await?;

        let wave_started = Instant::now();
        let mut batch_cases = batch.cases.iter().copied();
        let mut wave_index = 0usize;
        while let Some(case_a) = batch_cases.next() {
            wave_index += 1;
            let case_b = batch_cases.next();

            let selected_a = mixed_signal_selected(case_a);
            let mut env_a = MixedEnv::from_env_with_default_work_dir(
                case_a.scenario_id(),
                root.join(case_a.artifact_rel_dir()),
            );
            bind_mixed_env_to_shared_stack(&mut env_a, &stack.env);
            env_a.sink_port_offset = 0;
            if !explicit_n_per_group {
                env_a.n_per_group = 1;
            }
            if !explicit_only_checks {
                env_a.only_checks = Some(
                    selected_a
                        .check_names()
                        .into_iter()
                        .map(str::to_string)
                        .collect(),
                );
            }
            if !explicit_skip_load {
                env_a.skip_load = true;
            }
            if !explicit_collect_failures {
                env_a.collect_failures = true;
            }
            if !explicit_assertion_log {
                env_a.assertion_log = Some(root.join("assertions.jsonl"));
            }
            covered_output_cells += mixed_output_cases_for_input(case_a).len();

            if let Some(case_b) = case_b {
                let selected_b = mixed_signal_selected(case_b);
                let mut env_b = MixedEnv::from_env_with_default_work_dir(
                    case_b.scenario_id(),
                    root.join(case_b.artifact_rel_dir()),
                );
                bind_mixed_env_to_shared_stack(&mut env_b, &stack.env);
                env_b.sink_port_offset = 1;
                env_b.ffmpeg_signal_sink_base = stack
                    .env
                    .ffmpeg_signal_sink_base
                    .checked_add(128)
                    .ok_or("mixed ffmpeg signal sink base overflowed")?;
                env_b.ffmpeg_srt_sink_base = stack
                    .env
                    .ffmpeg_srt_sink_base
                    .checked_add(128)
                    .ok_or("mixed ffmpeg srt sink base overflowed")?;
                if !explicit_n_per_group {
                    env_b.n_per_group = 1;
                }
                if !explicit_only_checks {
                    env_b.only_checks = Some(
                        selected_b
                            .check_names()
                            .into_iter()
                            .map(str::to_string)
                            .collect(),
                    );
                }
                if !explicit_skip_load {
                    env_b.skip_load = true;
                }
                if !explicit_collect_failures {
                    env_b.collect_failures = true;
                }
                if !explicit_assertion_log {
                    env_b.assertion_log = Some(root.join("assertions.jsonl"));
                }
                covered_output_cells += mixed_output_cases_for_input(case_b).len();

                let (result_a, result_b) = tokio::join!(
                    run_mixed_input_case_on_active_stack(
                        case_a,
                        env_a,
                        &stack.api,
                        stack.restream_pid,
                    ),
                    run_mixed_input_case_on_active_stack(
                        case_b,
                        env_b,
                        &stack.api,
                        stack.restream_pid,
                    ),
                );
                for (case, selected, result) in [
                    (case_a, selected_a, result_a),
                    (case_b, selected_b, result_b),
                ] {
                    match result {
                        Ok(mut value) => {
                            value["signalRationale"] = json!(selected.rationale);
                            value["batchGroup"] = json!(batch.group.as_str());
                            value["wave"] = json!(wave_index);
                            results.push(value);
                        }
                        Err(error) => failures.push(format!("{}: {error}", case.scenario_id())),
                    }
                }
            } else {
                match run_mixed_input_case_on_active_stack(
                    case_a,
                    env_a,
                    &stack.api,
                    stack.restream_pid,
                )
                .await
                {
                    Ok(mut value) => {
                        value["signalRationale"] = json!(selected_a.rationale);
                        value["batchGroup"] = json!(batch.group.as_str());
                        value["wave"] = json!(wave_index);
                        results.push(value);
                    }
                    Err(error) => failures.push(format!("{}: {error}", case_a.scenario_id())),
                }
            }
        }
        emit_mixed_timing(
            &stack.env,
            MIXED_SIGNAL_MODE,
            &format!("batch.{}", batch.group.as_str()),
            if failures.is_empty() { "pass" } else { "fail" },
            wave_started.elapsed(),
            Some(json!({
                "group": batch.group.as_str(),
                "cases": batch.cases.iter().map(|case| case.scenario_id()).collect::<Vec<_>>(),
            })),
        )?;
        stop_mixed_harness_stack(&mut stack).await;
    }

    if !failures.is_empty() {
        return Err(format!(
            "mixed signal failed {} selected case(s): {}",
            failures.len(),
            failures.join(" | ")
        ));
    }

    Ok(json!({
        "passed": true,
        "mode": MIXED_SIGNAL_MODE,
        "coverage": {
            "selectedInputCases": selected_cases.len(),
            "totalInputCases": mixed_input_cases().len(),
            "selectedOutputCells": covered_output_cells,
            "totalOutputCells": total_output_cells,
            "nPerGroup": std::env::var("N_PER_GROUP").ok().unwrap_or_else(|| "1".to_string()),
            "selectedBatchGroups": selected_batches
                .iter()
                .map(|batch| batch.group.as_str())
                .collect::<Vec<_>>(),
            "defaultChecks": if explicit_only_checks {
                Value::Null
            } else {
                selected_cases.iter().map(|case| {
                    let selected = mixed_signal_selected(*case);
                    json!({
                        "id": case.scenario_id(),
                        "checks": selected.check_names(),
                    })
                }).collect::<Vec<_>>().into()
            },
            "defaultSkipLoad": !explicit_skip_load,
            "defaultCollectFailures": !explicit_collect_failures,
            "defaultAssertionLog": if explicit_assertion_log {
                Value::Null
            } else {
                root.join("assertions.jsonl").to_string_lossy().into_owned().into()
            },
            "sharedBatches": selected_batches.iter().map(|batch| {
                json!({
                    "group": batch.group.as_str(),
                    "cases": batch.cases.iter().map(|case| case.scenario_id()).collect::<Vec<_>>(),
                    "maxConcurrentPipelines": batch.cases.len().min(2),
                })
            }).collect::<Vec<_>>(),
        },
        "inputCases": selected_cases.iter().map(|case| {
            let selected = mixed_signal_selected(*case);
            json!({
                "id": case.scenario_id(),
                "source": case.source_name(),
                "ingest": case.ingest_name(),
                "video": case.codec_name(),
                "audio": case.audio_layout_name(),
                "reorder": case.reorder_name(),
                "sourceHasBframes": case.source_has_b_frames(),
                "outputCells": mixed_output_cases_for_input(*case).len(),
                "checks": selected.check_names(),
                "rationale": selected.rationale,
            })
        }).collect::<Vec<_>>(),
        "results": results,
    }))
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
    env: MixedEnv,
    api: &RampApi,
    restream_pid: u32,
) -> Result<Value, String> {
    let cfg = case.scenario_id();
    let plan = MixedScenarioPlan::for_input(case);
    let scenario_started = Instant::now();
    if env.n_per_group == 0 {
        return Err("N_PER_GROUP must be greater than zero".to_string());
    }
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    ensure_mixed_artifacts(&env)?;
    let mut resume = MixedResume::new(env.resume_from.clone());

    let config_started = Instant::now();
    let config = match (plan.source.adapter, case.codec(), case.is_multi_track()) {
        (MixedSourceAdapter::FileIngest, _, _) => {
            run_mixed_file_config(&env, api, restream_pid, case, &mut resume).await
        }
        (MixedSourceAdapter::SrtPublisher, MixedVideoCodec::H264, false) => {
            run_mixed_anchor_config(&env, api, restream_pid, case, &mut resume).await
        }
        (MixedSourceAdapter::SrtPublisher, MixedVideoCodec::H265, false) => {
            run_mixed_live_config(&env, api, restream_pid, case, &mut resume).await
        }
        (MixedSourceAdapter::RtmpPublisher, MixedVideoCodec::H264, false) => {
            run_mixed_live_config(&env, api, restream_pid, case, &mut resume).await
        }
        (MixedSourceAdapter::SrtPublisher, MixedVideoCodec::H264, true) => {
            run_mixed_live_config(&env, api, restream_pid, case, &mut resume).await
        }
        (MixedSourceAdapter::SrtPublisher, MixedVideoCodec::H265, true) => {
            run_mixed_live_config(&env, api, restream_pid, case, &mut resume).await
        }
        _ => Err(format!(
            "unsupported mixed input case {}",
            case.scenario_id()
        )),
    };
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
            "outputCells": plan.output_cells(),
            "checks": plan.check_names(),
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
            "summary": env.summary_log,
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
            "mediaDir": env.media_dir,
        }
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
        wait_for_outputs_progress(
            api,
            &pipeline_id,
            &progress_output_ids,
            mixed_output_progress_timeout(progress_output_ids.len()),
        )
        .await?;
    }

    let rss = record_mixed_rss_delta(env, cfg, restream_pid, rss_baseline, total, None).await?;

    if env.check_selected("ffprobe") {
        verify_mixed_output_dimensions(env, cfg, output_cases, resume).await?;
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
            MixedProbeSpec {
                cfg,
                id: mixed_scenario_check_id(cfg, "hls_transport_mtx"),
                label: "HLS/mtx",
                url: &format!(
                    "http://127.0.0.1:{}/live/{cfg}-rtmp.src.a0-{n}/index.m3u8",
                    env.mtx_hls
                ),
                expected: "1920x1080",
                cookie: None,
            },
            resume,
        )
        .await?;
        verify_mixed_stream(
            env,
            MixedProbeSpec {
                cfg,
                id: mixed_scenario_check_id(cfg, "hls_transport_restream"),
                label: "HLS/restream",
                url: &format!(
                    "http://127.0.0.1:{}/hls/{pipeline_id}/index.m3u8",
                    env.restream_http
                ),
                expected: "1920x1080",
                cookie: api.cookie.as_deref(),
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
    if env.check_selected("lifecycle")
        && resume.allows(&mixed_scenario_check_id(cfg, "clean_shutdown"))
    {
        if let Err(error) = lifecycle_result {
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
            return Err("lifecycle: outputs did not all stop within 60 s".to_string());
        }
        emit_mixed_result(
            env,
            cfg,
            &mixed_scenario_check_id(cfg, "clean_shutdown"),
            "pass",
            lifecycle_started.elapsed(),
            Some(json!({
                "stopped": output_ids.len(),
            })),
        )?;
        log_mixed_ok(env, "lifecycle: all outputs stopped")?;
    } else if lifecycle_result.is_err() {
        tokio::time::sleep(Duration::from_secs(3)).await;
    } else {
        log_mixed_ok(env, "lifecycle: all outputs stopped")?;
    }

    if let Some(error) = sink_probe_failure {
        return Err(error);
    }

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
        wait_for_outputs_progress(
            api,
            &pipeline_id,
            &output_ids,
            mixed_output_progress_timeout(output_ids.len()),
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
    stop_mixed_outputs(api, &pipeline_id, &output_ids).await;
    tokio::time::sleep(Duration::from_secs(8)).await;

    if let Some(error) = sink_probe_failure {
        return Err(error);
    }

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
        "recording": recording,
        "outputMatrix": mixed_output_matrix_json(output_cases),
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
        wait_for_outputs_progress(
            api,
            &pipeline_id,
            &output_ids,
            mixed_output_progress_timeout(output_ids.len()),
        )
        .await?;
    }

    let duration_secs: u64 = 10;
    verify_optional_mixed_hls_preview(env, api, cfg, &pipeline_id, case, resume).await?;
    if !ffmpeg_srt_sinks.is_empty() {
        finish_ffmpeg_srt_sinks(&mut ffmpeg_srt_sinks).await?;
    }
    verify_mixed_output_cases_inner(env, cfg, output_cases, resume, case.is_multi_track(), true)
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

    for (i, output_id) in output_ids.iter().enumerate() {
        api.post_empty(&format!(
            "/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"
        ))
        .await?;
        if i % 4 == 3 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    api.post_empty(&format!("/api/v1/ingests/{ingest_id}/stop"))
        .await?;

    println!(
        "[{cfg}] done: {total} outputs, baseline={rss_baseline}kB peak={rss_peak}kB growth={growth_kb}kB"
    );

    Ok(json!({
        "scenario": cfg,
        "inputCase": case.scenario_id(),
        "codec": case.codec_name(),
        "trackLayout": case.track_layout_name(),
        "outputCount": total,
        "outputMatrix": mixed_output_matrix_json(output_cases),
        "recording": recording,
        "rssBaselineKb": rss_baseline,
        "rssPeakKb": rss_peak,
        "rssGrowthKb": growth_kb,
    }))
}
