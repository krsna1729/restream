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

pub(super) fn apply_mixed_matrix_defaults(
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
pub(super) enum MatrixCaseState {
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
    pub(super) case: MixedInputCase,
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

pub(super) fn matrix_mark_case_state(
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

pub(super) fn matrix_progress_totals(rows: &[MatrixCaseProgress]) -> Value {
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

pub(super) fn matrix_case_progress_json(rows: &[MatrixCaseProgress]) -> Vec<Value> {
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

pub(super) fn write_matrix_scenario_progress_for_mode(
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
