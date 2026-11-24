//! Serial and shared-stack full mixed-matrix execution.

use super::*;
use crate::mixed_runner::mixed_stack::MixedHarnessStack;

pub(in super::super) async fn mixed_input_matrix_correctness_serial() -> Result<Value, String> {
    let setup = MatrixSetup::from_env();
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut case_progress = matrix_case_progress_rows();
    let mut covered_output_cells = 0usize;
    let total_output_cells = setup.total_output_cells();
    write_matrix_scenario_progress(
        &setup.scenario_path,
        "serial",
        setup.fail_fast,
        &case_progress,
        &failures,
    )?;
    for (index, case) in mixed_input_cases().iter().copied().enumerate() {
        let wave_index = index + 1;
        let result = run_serial_matrix_case(
            &setup,
            case,
            wave_index,
            &mut covered_output_cells,
            &mut case_progress,
            &mut failures,
        )
        .await?;
        if let Some(result) = result {
            results.push(result);
        }
    }

    Ok(MatrixReport {
        execution: "serial",
        forced_serial: true,
        covered_output_cells,
        total_output_cells,
        case_progress: &case_progress,
        failures,
        results,
        shared_batches: None,
    }
    .into_json(&setup))
}

/// Runs one serial input case and records its progress; `Err` aborts under fail-fast.
async fn run_serial_matrix_case(
    setup: &MatrixSetup,
    case: MixedInputCase,
    wave_index: usize,
    covered_output_cells: &mut usize,
    case_progress: &mut [MatrixCaseProgress],
    failures: &mut Vec<String>,
) -> Result<Option<Value>, String> {
    let mode = mixed_input_mode_name(case);
    let mut env =
        MixedEnv::from_env_with_default_work_dir(&mode, setup.root.join(case.artifact_rel_dir()));
    apply_mixed_matrix_defaults(
        &mut env,
        setup.default_checks.as_deref(),
        setup.default_assertion_log.as_deref(),
        setup.explicit_collect_failures,
    );
    *covered_output_cells += mixed_output_cases_for_input(case).len();
    matrix_mark_case_state(
        case_progress,
        case,
        MatrixCaseState::InProgress,
        Some(wave_index),
        None,
    );
    write_matrix_scenario_progress(
        &setup.scenario_path,
        "serial",
        setup.fail_fast,
        case_progress,
        failures.as_slice(),
    )?;
    match run_mixed_input_case_with_env(case, env).await {
        Ok(result) => {
            matrix_mark_case_state(
                case_progress,
                case,
                MatrixCaseState::Passed,
                Some(wave_index),
                None,
            );
            write_matrix_scenario_progress(
                &setup.scenario_path,
                "serial",
                setup.fail_fast,
                case_progress,
                failures.as_slice(),
            )?;
            Ok(Some(result))
        }
        Err(error) => {
            let failure = format!("mixed input case {} failed: {error}", case.scenario_id());
            matrix_mark_case_state(
                case_progress,
                case,
                MatrixCaseState::Failed,
                Some(wave_index),
                Some(error),
            );
            failures.push(failure.clone());
            write_matrix_scenario_progress(
                &setup.scenario_path,
                "serial",
                setup.fail_fast,
                case_progress,
                failures.as_slice(),
            )?;
            if setup.fail_fast {
                Err(failure)
            } else {
                Ok(None)
            }
        }
    }
}

pub(in super::super) async fn mixed_input_matrix_correctness_shared() -> Result<Value, String> {
    let setup = MatrixSetup::from_env();
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut case_progress = matrix_case_progress_rows();
    let mut covered_output_cells = 0usize;
    let total_output_cells = setup.total_output_cells();
    let batch_groups = [
        MixedSharedBatchGroup::LiveRtmp,
        MixedSharedBatchGroup::LiveSrt,
        MixedSharedBatchGroup::FileIngest,
    ];
    write_matrix_scenario_progress(
        &setup.scenario_path,
        "shared-batch",
        setup.fail_fast,
        &case_progress,
        &failures,
    )?;

    {
        let mut runner = SharedMatrixRunner {
            setup: &setup,
            case_progress: &mut case_progress,
            failures: &mut failures,
            results: &mut results,
            covered_output_cells: &mut covered_output_cells,
        };
        for group in batch_groups {
            run_shared_batch_group(&mut runner, group).await?;
        }
    }

    Ok(MatrixReport {
        execution: "shared-batch",
        forced_serial: false,
        covered_output_cells,
        total_output_cells,
        case_progress: &case_progress,
        failures,
        results,
        shared_batches: Some(shared_batches_json()),
    }
    .into_json(&setup))
}

/// Mutable matrix bookkeeping plus the setup shared across one batch execution.
struct SharedMatrixRunner<'a> {
    setup: &'a MatrixSetup,
    case_progress: &'a mut Vec<MatrixCaseProgress>,
    failures: &'a mut Vec<String>,
    results: &'a mut Vec<Value>,
    covered_output_cells: &'a mut usize,
}

/// Whether a wave failed and, under fail-fast, the first failure to abort with.
#[derive(Default)]
struct WaveOutcome {
    failed: bool,
    fail_fast: Option<String>,
}

impl SharedMatrixRunner<'_> {
    fn failure_count(&self) -> usize {
        self.failures.len()
    }

    fn write_progress(&self, execution: &str) -> Result<(), String> {
        write_matrix_scenario_progress(
            &self.setup.scenario_path,
            execution,
            self.setup.fail_fast,
            &self.case_progress[..],
            &self.failures[..],
        )
    }

    fn mark_in_progress(
        &mut self,
        cases: &[MixedInputCase],
        wave_index: usize,
    ) -> Result<(), String> {
        for case in cases {
            matrix_mark_case_state(
                &mut self.case_progress[..],
                *case,
                MatrixCaseState::InProgress,
                Some(wave_index),
                None,
            );
        }
        self.write_progress("shared-batch")
    }

    async fn run_wave(
        &mut self,
        group: MixedSharedBatchGroup,
        stack: &mut MixedHarnessStack,
        wave_index: usize,
        case_a: MixedInputCase,
        case_b: Option<MixedInputCase>,
    ) -> Result<WaveOutcome, String> {
        let mut outcome = WaveOutcome::default();
        if let Some(case_b) = case_b {
            self.mark_in_progress(&[case_a, case_b], wave_index)?;
            let env_a = shared_case_env(self.setup, case_a, &stack.env, false)?;
            *self.covered_output_cells += mixed_output_cases_for_input(case_a).len();
            let env_b = shared_case_env(self.setup, case_b, &stack.env, true)?;
            *self.covered_output_cells += mixed_output_cases_for_input(case_b).len();

            let (result_a, result_b) = tokio::join!(
                run_mixed_input_case_on_active_stack(case_a, env_a, &stack.api, stack.restream_pid),
                run_mixed_input_case_on_active_stack(case_b, env_b, &stack.api, stack.restream_pid),
            );
            self.record_case(group, wave_index, case_a, result_a, &mut outcome);
            self.record_case(group, wave_index, case_b, result_b, &mut outcome);
        } else {
            let env_a = shared_case_env(self.setup, case_a, &stack.env, false)?;
            *self.covered_output_cells += mixed_output_cases_for_input(case_a).len();
            self.mark_in_progress(&[case_a], wave_index)?;
            let result_a =
                run_mixed_input_case_on_active_stack(case_a, env_a, &stack.api, stack.restream_pid)
                    .await;
            self.record_case(group, wave_index, case_a, result_a, &mut outcome);
        }
        self.write_progress("shared-batch")?;
        Ok(outcome)
    }

    fn record_case(
        &mut self,
        group: MixedSharedBatchGroup,
        wave_index: usize,
        case: MixedInputCase,
        result: Result<Value, String>,
        outcome: &mut WaveOutcome,
    ) {
        match result {
            Ok(mut value) => {
                value["batchGroup"] = json!(group.as_str());
                value["wave"] = json!(wave_index);
                self.results.push(value);
                matrix_mark_case_state(
                    &mut self.case_progress[..],
                    case,
                    MatrixCaseState::Passed,
                    Some(wave_index),
                    None,
                );
            }
            Err(error) => {
                let failure = format!("mixed input case {} failed: {error}", case.scenario_id());
                outcome.failed = true;
                self.failures.push(failure.clone());
                matrix_mark_case_state(
                    &mut self.case_progress[..],
                    case,
                    MatrixCaseState::Failed,
                    Some(wave_index),
                    Some(error),
                );
                if self.setup.fail_fast && outcome.fail_fast.is_none() {
                    outcome.fail_fast = Some(failure);
                }
            }
        }
    }
}

/// Runs one shared batch group: its waves, restart-on-failure, and timing emission.
async fn run_shared_batch_group(
    runner: &mut SharedMatrixRunner<'_>,
    group: MixedSharedBatchGroup,
) -> Result<(), String> {
    let failures_before_group = runner.failure_count();
    let cases: Vec<_> = mixed_input_cases()
        .iter()
        .copied()
        .filter(|case| case.shared_batch_group() == group)
        .collect();
    if cases.is_empty() {
        return Ok(());
    }

    let mut stack = start_mixed_harness_stack(shared_stack_env(runner.setup, group)).await?;
    let mut stack_stopped = false;
    let wave_started = Instant::now();

    let waves = plan_shared_waves(&cases);
    let wave_count = waves.len();
    for (index, (case_a, case_b)) in waves.into_iter().enumerate() {
        let wave_index = index + 1;
        let outcome = runner
            .run_wave(group, &mut stack, wave_index, case_a, case_b)
            .await?;
        if let Some(failure) = outcome.fail_fast {
            stop_mixed_harness_stack(&mut stack).await;
            return Err(failure);
        }
        if outcome.failed {
            stop_mixed_harness_stack(&mut stack).await;
            stack_stopped = true;
            if wave_index < wave_count {
                stack = start_mixed_harness_stack(shared_stack_env(runner.setup, group)).await?;
                stack_stopped = false;
            }
        }
    }

    emit_mixed_timing(
        &stack.env,
        MIXED_MATRIX_MODE,
        &format!("batch.{}", group.as_str()),
        if runner.failure_count() == failures_before_group {
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
    Ok(())
}

/// Builds a case env bound to the shared stack, mirroring the matrix defaults.
fn shared_case_env(
    setup: &MatrixSetup,
    case: MixedInputCase,
    stack_env: &MixedEnv,
    second_slot: bool,
) -> Result<MixedEnv, String> {
    let mut env = MixedEnv::from_env_with_default_work_dir(
        case.scenario_id(),
        setup.root.join(case.artifact_rel_dir()),
    );
    bind_mixed_env_to_shared_stack(&mut env, stack_env);
    env.sink_port_offset = usize::from(second_slot);
    if second_slot {
        env.ffmpeg_signal_sink_base = stack_env
            .ffmpeg_signal_sink_base
            .checked_add(128)
            .ok_or("mixed ffmpeg signal sink base overflowed")?;
        env.ffmpeg_srt_sink_base = stack_env
            .ffmpeg_srt_sink_base
            .checked_add(128)
            .ok_or("mixed ffmpeg srt sink base overflowed")?;
    }
    apply_mixed_matrix_defaults(
        &mut env,
        setup.default_checks.as_deref(),
        setup.default_assertion_log.as_deref(),
        setup.explicit_collect_failures,
    );
    Ok(env)
}

/// Builds the group stack env with matrix defaults applied.
fn shared_stack_env(setup: &MatrixSetup, group: MixedSharedBatchGroup) -> MixedEnv {
    let stack_mode = format!("{MIXED_MATRIX_MODE}.{}", group.as_str());
    let mut env = MixedEnv::from_env_with_default_work_dir(
        &stack_mode,
        setup.root.join("_shared").join(group.as_str()),
    );
    apply_mixed_matrix_defaults(
        &mut env,
        setup.default_checks.as_deref(),
        setup.default_assertion_log.as_deref(),
        setup.explicit_collect_failures,
    );
    env
}

/// Per-group shared-batch case lists reported in the coverage block.
fn shared_batches_json() -> Value {
    json!([
        {
            "group": "live-rtmp",
            "maxConcurrentPipelines": 2,
            "cases": shared_group_case_ids(MixedSharedBatchGroup::LiveRtmp),
        },
        {
            "group": "live-srt",
            "maxConcurrentPipelines": 2,
            "cases": shared_group_case_ids(MixedSharedBatchGroup::LiveSrt),
        },
        {
            "group": "file-ingest",
            "maxConcurrentPipelines": 2,
            "cases": shared_group_case_ids(MixedSharedBatchGroup::FileIngest),
        },
    ])
}

fn shared_group_case_ids(group: MixedSharedBatchGroup) -> Vec<&'static str> {
    mixed_input_cases()
        .iter()
        .filter(|case| case.shared_batch_group() == group)
        .map(|case| case.scenario_id())
        .collect()
}

/// Environment-derived flags and paths shared by serial and shared-batch modes.
struct MatrixSetup {
    root: PathBuf,
    scenario_path: PathBuf,
    explicit_only_checks: bool,
    explicit_collect_failures: bool,
    explicit_assertion_log: bool,
    fail_fast: bool,
    default_checks: Option<Vec<String>>,
    default_assertion_log: Option<PathBuf>,
}

impl MatrixSetup {
    fn from_env() -> Self {
        let root = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(mixed_matrix_default_work_dir);
        let scenario_path = root.join("scenario.json");
        let explicit_only_checks = std::env::var_os("ONLY_CHECKS").is_some();
        let explicit_collect_failures = std::env::var_os("COLLECT_FAILURES").is_some();
        let explicit_assertion_log = std::env::var_os("ASSERTION_LOG").is_some();
        let fail_fast = mixed_matrix_fail_fast();
        let default_checks = (!explicit_only_checks).then(mixed_matrix_default_check_names);
        let default_assertion_log =
            (!explicit_assertion_log).then(|| root.join("assertions.jsonl"));
        Self {
            root,
            scenario_path,
            explicit_only_checks,
            explicit_collect_failures,
            explicit_assertion_log,
            fail_fast,
            default_checks,
            default_assertion_log,
        }
    }

    fn total_output_cells(&self) -> usize {
        mixed_input_cases()
            .iter()
            .map(|case| mixed_output_cases_for_input(*case).len())
            .sum()
    }
}

/// Final matrix result JSON shared by serial and shared-batch execution.
struct MatrixReport<'a> {
    execution: &'a str,
    forced_serial: bool,
    covered_output_cells: usize,
    total_output_cells: usize,
    case_progress: &'a [MatrixCaseProgress],
    failures: Vec<String>,
    results: Vec<Value>,
    shared_batches: Option<Value>,
}

impl MatrixReport<'_> {
    fn into_json(self, setup: &MatrixSetup) -> Value {
        let mut coverage = json!({
            "selectedInputCases": mixed_input_cases().len(),
            "totalInputCases": mixed_input_cases().len(),
            "selectedOutputCells": self.covered_output_cells,
            "totalOutputCells": self.total_output_cells,
            "execution": self.execution,
            "defaultExecution": "shared-batch",
            "forcedSerial": self.forced_serial,
            "serialOptOutEnv": "MIXED_MATRIX_SERIAL",
            "continueOnScenarioFailure": !setup.fail_fast,
            "failFastOptOutEnv": "MIXED_MATRIX_FAIL_FAST",
            "defaultCollectFailures": !setup.explicit_collect_failures,
            "defaultAssertionLog": if setup.explicit_assertion_log {
                Value::Null
            } else {
                setup.root.join("assertions.jsonl").to_string_lossy().into_owned().into()
            },
            "defaultChecks": if setup.explicit_only_checks {
                Value::Null
            } else {
                mixed_default_checks()
                    .iter()
                    .map(|check| check.as_str())
                    .collect::<Vec<_>>()
                    .into()
            },
            "sharedBatchGroups": ["live-rtmp", "live-srt", "file-ingest"],
        });
        if let Some(shared_batches) = self.shared_batches {
            coverage["sharedBatches"] = shared_batches;
        }

        json!({
            "passed": self.failures.is_empty(),
            "mode": MIXED_MATRIX_MODE,
            "progress": matrix_progress_totals(self.case_progress),
            "caseProgress": matrix_case_progress_json(self.case_progress),
            "rootCauseSummary": mixed_root_cause_summary_json(&self.failures),
            "artifacts": {
                "rootCauseSummaryJson": mixed_root_cause_summary_path(&setup.scenario_path),
                "artifactIndexJson": mixed_root_artifact_index_path(&setup.root),
            },
            "coverage": coverage,
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
            "failures": self.failures,
            "results": self.results,
        })
    }
}
