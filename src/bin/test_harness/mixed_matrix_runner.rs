//! Mixed matrix, fast-breadth, and signal shared-batch orchestration.

use super::*;

pub(crate) fn mixed_matrix_fail_fast() -> bool {
    std::env::var("MIXED_MATRIX_FAIL_FAST")
        .ok()
        .is_some_and(|value| value == "1" || value.eq_ignore_ascii_case("true"))
}

pub(crate) fn mixed_matrix_default_check_names() -> Vec<String> {
    mixed_default_checks()
        .iter()
        .map(|check| check.as_str().to_string())
        .collect()
}

pub(crate) fn mixed_matrix_cases_can_share_wave(
    first: MixedInputCase,
    second: MixedInputCase,
) -> bool {
    !matches!(first.codec(), MixedVideoCodec::H265)
        && !matches!(second.codec(), MixedVideoCodec::H265)
}

const MIXED_RUNTIME_LOG_NOISE_PATTERNS: [&str; 5] = [
    "PPS id out of range",
    "Could not find ref with POC",
    "Error constructing the frame RPS.",
    "Skipping invalid undecodable NALU",
    "Error parsing NAL",
];

pub(crate) fn mixed_runtime_log_noise_matches(line: &str) -> bool {
    MIXED_RUNTIME_LOG_NOISE_PATTERNS
        .iter()
        .any(|pattern| line.contains(pattern))
}

pub(crate) fn mixed_runtime_log_noise_lines(path: &Path, pipeline_id: &str) -> Vec<String> {
    effective_log_paths(path)
        .into_iter()
        .filter_map(|candidate| std::fs::read_to_string(candidate).ok())
        .flat_map(|content| {
            content
                .lines()
                .filter(|line| line.contains(pipeline_id) && mixed_runtime_log_noise_matches(line))
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .collect()
}

pub(crate) fn verify_mixed_runtime_log_hygiene(
    env: &MixedEnv,
    cfg: &str,
    pipeline_id: &str,
    elapsed: Duration,
) -> Result<(), String> {
    let matches = mixed_runtime_log_noise_lines(&env.restream_log, pipeline_id);
    let id = mixed_scenario_check_id(cfg, "runtime_log");
    if matches.is_empty() {
        emit_mixed_result(
            env,
            cfg,
            &id,
            "pass",
            elapsed,
            Some(json!({
                "pipelineId": pipeline_id,
                "matchedLines": 0,
            })),
        )?;
        log_mixed_ok(env, "runtime-log: clean")?;
        return Ok(());
    }

    emit_mixed_result(
        env,
        cfg,
        &id,
        "fail",
        elapsed,
        Some(json!({
            "pipelineId": pipeline_id,
            "matchedLines": matches.len(),
            "patterns": MIXED_RUNTIME_LOG_NOISE_PATTERNS,
            "sample": matches.iter().take(5).cloned().collect::<Vec<_>>(),
        })),
    )?;
    Err(format!(
        "runtime-log: decoder noise detected for {pipeline_id}: {}",
        matches
            .iter()
            .take(3)
            .cloned()
            .collect::<Vec<_>>()
            .join(" | ")
    ))
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
pub(crate) struct MatrixCaseProgress {
    case: MixedInputCase,
    batch_group: MixedSharedBatchGroup,
    output_cells: usize,
    state: MatrixCaseState,
    wave: Option<usize>,
    error: Option<String>,
}

pub(crate) fn matrix_case_progress_rows() -> Vec<MatrixCaseProgress> {
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
                "hlsPreviewTiming": HlsPreviewTiming::for_input(row.case).as_str(),
                "supportedHlsPreviewTimings": HlsPreviewTiming::supported_names(),
                "probeSampling": {
                    "policy": ProbeSamplingPolicy::for_input(row.case).as_str(),
                },
                "supportedProbeSamplingPolicies": ProbeSamplingPolicy::supported_names(),
                "wave": row.wave,
                "outputCells": row.output_cells,
                "error": row.error,
            })
        })
        .collect()
}

fn matrix_case_artifact_index_json(root: &Path, rows: &[MatrixCaseProgress]) -> Vec<Value> {
    rows.iter()
        .map(|row| {
            let work_dir = root.join(row.case.artifact_rel_dir());
            json!({
                "id": row.case.scenario_id(),
                "status": row.state.as_str(),
                "batchGroup": row.batch_group.as_str(),
                "workDir": work_dir,
                "scenarioJson": work_dir.join("scenario.json"),
                "artifactIndexJson": work_dir.join("artifact-index.json"),
                "outputsJson": work_dir.join("outputs.json"),
                "sqliteSnapshotDir": work_dir.join("sqlite-snapshot"),
                "logs": [
                    work_dir.join(format!("{}-restream.log", row.case.scenario_id())),
                    work_dir.join(format!("{}-mediamtx.log", row.case.scenario_id())),
                ],
                "media": work_dir.join("media"),
            })
        })
        .collect()
}

pub(crate) fn write_json_pretty_atomic(path: &Path, value: &Value) -> Result<(), String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    let tmp_path = path.with_extension("json.tmp");
    let payload = serde_json::to_vec_pretty(value).map_err(|error| error.to_string())?;
    std::fs::write(&tmp_path, payload).map_err(|error| error.to_string())?;
    std::fs::rename(tmp_path, path).map_err(|error| error.to_string())
}

pub(crate) fn write_matrix_scenario_progress(
    path: &Path,
    execution: &str,
    fail_fast: bool,
    rows: &[MatrixCaseProgress],
    failures: &[String],
) -> Result<(), String> {
    write_matrix_scenario_progress_for_mode(
        path,
        MIXED_MATRIX_MODE,
        execution,
        fail_fast,
        rows,
        failures,
    )
}

fn write_matrix_scenario_progress_for_mode(
    path: &Path,
    mode: &str,
    execution: &str,
    fail_fast: bool,
    rows: &[MatrixCaseProgress],
    failures: &[String],
) -> Result<(), String> {
    let root_cause_summary_path = write_mixed_root_cause_summary(path, failures)?;
    let root = path
        .parent()
        .ok_or_else(|| format!("scenario path has no parent: {}", path.display()))?;
    let assertion_log_path = root.join("assertions.jsonl");
    let artifact_index_path = write_mixed_root_artifact_index(
        root,
        mode,
        path,
        &root_cause_summary_path,
        Some(&assertion_log_path),
        matrix_case_artifact_index_json(root, rows),
    )?;
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
            "mode": mode,
            "execution": execution,
            "executionState": execution_state,
            "passed": passed,
            "progress": progress,
            "failures": failures,
            "rootCauseSummary": mixed_root_cause_summary_json(failures),
            "continueOnScenarioFailure": !fail_fast,
            "failFastOptOutEnv": "MIXED_MATRIX_FAIL_FAST",
            "artifacts": {
                "rootCauseSummaryJson": root_cause_summary_path,
                "artifactIndexJson": artifact_index_path,
            },
            "caseProgress": matrix_case_progress_json(rows),
            "updatedAt": Utc::now().to_rfc3339(),
        }),
    )
}

pub(super) async fn mixed_input_matrix_correctness_serial() -> Result<Value, String> {
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
    let root_cause_summary_path = mixed_root_cause_summary_path(&scenario_path);

    Ok(json!({
        "passed": failures.is_empty(),
        "mode": MIXED_MATRIX_MODE,
        "progress": progress,
        "caseProgress": matrix_case_progress_json(&case_progress),
        "rootCauseSummary": mixed_root_cause_summary_json(&failures),
        "artifacts": {
            "rootCauseSummaryJson": root_cause_summary_path,
            "artifactIndexJson": mixed_root_artifact_index_path(&root),
        },
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

pub(super) async fn mixed_input_matrix_correctness_shared() -> Result<Value, String> {
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
            let case_b = cases_queue
                .front()
                .copied()
                .filter(|case_b| mixed_matrix_cases_can_share_wave(case_a, *case_b))
                .and_then(|_| cases_queue.pop_front());

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
    let root_cause_summary_path = mixed_root_cause_summary_path(&scenario_path);

    Ok(json!({
        "passed": failures.is_empty(),
        "mode": MIXED_MATRIX_MODE,
        "progress": progress,
        "caseProgress": matrix_case_progress_json(&case_progress),
        "rootCauseSummary": mixed_root_cause_summary_json(&failures),
        "artifacts": {
            "rootCauseSummaryJson": root_cause_summary_path,
            "artifactIndexJson": mixed_root_artifact_index_path(&root),
        },
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

pub(crate) async fn mixed_fast_breadth_correctness() -> Result<Value, String> {
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
    let scenario_path = root.join("scenario.json");
    let mut case_progress = matrix_case_progress_rows()
        .into_iter()
        .filter(|row| selected_cases.contains(&row.case))
        .collect::<Vec<_>>();

    for case in &selected_cases {
        total_output_cells += mixed_output_cases_for_input(*case).len();
    }
    write_matrix_scenario_progress_for_mode(
        &scenario_path,
        MIXED_FAST_BREADTH_MODE,
        "fast-breadth",
        false,
        &case_progress,
        &failures,
    )?;

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

                matrix_mark_case_state(
                    &mut case_progress,
                    case_a,
                    MatrixCaseState::InProgress,
                    Some(wave_index),
                    None,
                );
                matrix_mark_case_state(
                    &mut case_progress,
                    case_b,
                    MatrixCaseState::InProgress,
                    Some(wave_index),
                    None,
                );
                write_matrix_scenario_progress_for_mode(
                    &scenario_path,
                    MIXED_FAST_BREADTH_MODE,
                    "fast-breadth",
                    false,
                    &case_progress,
                    &failures,
                )?;

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
                            matrix_mark_case_state(
                                &mut case_progress,
                                case,
                                MatrixCaseState::Passed,
                                Some(wave_index),
                                None,
                            );
                            results.push(value);
                        }
                        Err(error) => {
                            let failure = format!("{}: {error}", case.scenario_id());
                            matrix_mark_case_state(
                                &mut case_progress,
                                case,
                                MatrixCaseState::Failed,
                                Some(wave_index),
                                Some(failure.clone()),
                            );
                            failures.push(failure);
                        }
                    }
                }
                write_matrix_scenario_progress_for_mode(
                    &scenario_path,
                    MIXED_FAST_BREADTH_MODE,
                    "fast-breadth",
                    false,
                    &case_progress,
                    &failures,
                )?;
            } else {
                matrix_mark_case_state(
                    &mut case_progress,
                    case_a,
                    MatrixCaseState::InProgress,
                    Some(wave_index),
                    None,
                );
                write_matrix_scenario_progress_for_mode(
                    &scenario_path,
                    MIXED_FAST_BREADTH_MODE,
                    "fast-breadth",
                    false,
                    &case_progress,
                    &failures,
                )?;
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
                        matrix_mark_case_state(
                            &mut case_progress,
                            case_a,
                            MatrixCaseState::Passed,
                            Some(wave_index),
                            None,
                        );
                        results.push(value);
                    }
                    Err(error) => {
                        let failure = format!("{}: {error}", case_a.scenario_id());
                        matrix_mark_case_state(
                            &mut case_progress,
                            case_a,
                            MatrixCaseState::Failed,
                            Some(wave_index),
                            Some(failure.clone()),
                        );
                        failures.push(failure);
                    }
                }
                write_matrix_scenario_progress_for_mode(
                    &scenario_path,
                    MIXED_FAST_BREADTH_MODE,
                    "fast-breadth",
                    false,
                    &case_progress,
                    &failures,
                )?;
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
        write_matrix_scenario_progress_for_mode(
            &scenario_path,
            MIXED_FAST_BREADTH_MODE,
            "fast-breadth",
            false,
            &case_progress,
            &failures,
        )?;
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

pub(crate) async fn mixed_signal_correctness() -> Result<Value, String> {
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
