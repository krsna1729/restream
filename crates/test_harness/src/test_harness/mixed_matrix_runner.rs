//! Fast-breadth and signal shared-batch orchestration.

use super::*;
use crate::mixed_runner::mixed_stack::MixedHarnessStack;

#[path = "mixed_matrix_execution.rs"]
mod mixed_matrix_execution;
#[path = "mixed_matrix_progress.rs"]
mod mixed_matrix_progress;

pub(super) use mixed_matrix_execution::{
    mixed_input_matrix_correctness_serial, mixed_input_matrix_correctness_shared,
};
use mixed_matrix_progress::{
    MatrixCaseProgress, MatrixCaseState, apply_mixed_matrix_defaults, matrix_case_progress_json,
    matrix_mark_case_state, matrix_progress_totals, write_matrix_scenario_progress_for_mode,
};
pub(crate) use mixed_matrix_progress::{
    matrix_case_progress_rows, mixed_matrix_default_check_names, mixed_matrix_fail_fast,
    plan_shared_waves, verify_mixed_runtime_log_hygiene, write_matrix_scenario_progress,
};
#[cfg(test)]
pub(crate) use mixed_matrix_progress::{
    mixed_matrix_cases_can_share_wave, mixed_runtime_log_noise_lines,
    mixed_runtime_log_noise_matches,
};

pub(crate) async fn mixed_fast_breadth_correctness() -> Result<Value, String> {
    let setup = ListMatrixSetup::from_batches(
        selected_mixed_fast_breadth_batches()?,
        mixed_fast_breadth_default_work_dir,
    );
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut covered_output_cells = 0usize;
    let total_output_cells = setup.total_output_cells();
    let scenario_path = setup.root.join("scenario.json");
    let mut case_progress = matrix_case_progress_rows()
        .into_iter()
        .filter(|row| setup.selected_cases.contains(&row.case))
        .collect::<Vec<_>>();

    write_fast_breadth_progress(&scenario_path, &case_progress, &failures)?;

    {
        let mut run = FastBreadthRun {
            setup: &setup,
            scenario_path: &scenario_path,
            case_progress: &mut case_progress,
            failures: &mut failures,
            results: &mut results,
            covered_output_cells: &mut covered_output_cells,
        };
        for batch in &setup.selected_batches {
            run_one_fast_breadth_batch(&mut run, batch).await?;
        }
    }

    if !failures.is_empty() {
        write_fast_breadth_progress(&scenario_path, &case_progress, &failures)?;
        return Err(format!(
            "mixed fast breadth failed {} selected case(s): {}",
            failures.len(),
            failures.join(" | ")
        ));
    }

    Ok(fast_breadth_report(
        &setup,
        covered_output_cells,
        total_output_cells,
        results,
    ))
}

/// Runs one fast-breadth batch on its own shared stack and emits its timing.
async fn run_one_fast_breadth_batch(
    run: &mut FastBreadthRun<'_>,
    batch: &MixedFastBreadthBatch,
) -> Result<(), String> {
    let stack_mode = format!("{MIXED_FAST_BREADTH_MODE}.{}", batch.group.as_str());
    let stack_env = MixedEnv::from_env_with_default_work_dir(
        &stack_mode,
        run.setup.root.join("_shared").join(batch.group.as_str()),
    );
    let mut stack = start_mixed_harness_stack(stack_env).await?;

    let wave_started = Instant::now();
    let mut wave_index = 0usize;
    for (case_a, case_b) in plan_shared_waves(&batch.cases) {
        wave_index += 1;
        run.run_wave(batch, &mut stack, wave_index, case_a, case_b)
            .await?;
    }
    emit_mixed_timing(
        &stack.env,
        MIXED_FAST_BREADTH_MODE,
        &format!("batch.{}", batch.group.as_str()),
        if run.failures.is_empty() {
            "pass"
        } else {
            "fail"
        },
        wave_started.elapsed(),
        Some(json!({
            "group": batch.group.as_str(),
            "cases": batch.cases.iter().map(|case| case.scenario_id()).collect::<Vec<_>>(),
        })),
    )?;
    stop_mixed_harness_stack(&mut stack).await;
    Ok(())
}

/// Matrix bookkeeping plus selection flags for one fast-breadth execution.
struct FastBreadthRun<'a> {
    setup: &'a ListMatrixSetup,
    scenario_path: &'a Path,
    case_progress: &'a mut Vec<MatrixCaseProgress>,
    failures: &'a mut Vec<String>,
    results: &'a mut Vec<Value>,
    covered_output_cells: &'a mut usize,
}

impl FastBreadthRun<'_> {
    fn write_progress(&self) -> Result<(), String> {
        write_fast_breadth_progress(
            self.scenario_path,
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
        self.write_progress()
    }

    fn record_result(
        &mut self,
        batch: &MixedFastBreadthBatch,
        wave_index: usize,
        case: MixedInputCase,
        rationale: &str,
        result: Result<Value, String>,
    ) {
        match result {
            Ok(mut value) => {
                value["fastBreadthRationale"] = json!(rationale);
                value["batchGroup"] = json!(batch.group.as_str());
                value["wave"] = json!(wave_index);
                matrix_mark_case_state(
                    &mut self.case_progress[..],
                    case,
                    MatrixCaseState::Passed,
                    Some(wave_index),
                    None,
                );
                self.results.push(value);
            }
            Err(error) => {
                let failure = format!("{}: {error}", case.scenario_id());
                matrix_mark_case_state(
                    &mut self.case_progress[..],
                    case,
                    MatrixCaseState::Failed,
                    Some(wave_index),
                    Some(failure.clone()),
                );
                self.failures.push(failure);
            }
        }
    }

    fn env_for(
        &self,
        case: MixedInputCase,
        stack_env: &MixedEnv,
        second_slot: bool,
        checks: &[&'static str],
    ) -> Result<MixedEnv, String> {
        let setup = self.setup;
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
        apply_fast_breadth_defaults(&mut env, setup, checks);
        Ok(env)
    }

    async fn run_wave(
        &mut self,
        batch: &MixedFastBreadthBatch,
        stack: &mut MixedHarnessStack,
        wave_index: usize,
        case_a: MixedInputCase,
        case_b: Option<MixedInputCase>,
    ) -> Result<(), String> {
        let selected_a = mixed_fast_breadth_selected(case_a);
        let checks_a = selected_a.check_names();
        let env_a = self.env_for(case_a, &stack.env, false, &checks_a)?;
        *self.covered_output_cells += mixed_output_cases_for_input(case_a).len();
        if let Some(case_b) = case_b {
            let selected_b = mixed_fast_breadth_selected(case_b);
            let checks_b = selected_b.check_names();
            let env_b = self.env_for(case_b, &stack.env, true, &checks_b)?;
            *self.covered_output_cells += mixed_output_cases_for_input(case_b).len();
            self.mark_in_progress(&[case_a, case_b], wave_index)?;
            let (result_a, result_b) = tokio::join!(
                run_mixed_input_case_on_active_stack(case_a, env_a, &stack.api, stack.restream_pid),
                run_mixed_input_case_on_active_stack(case_b, env_b, &stack.api, stack.restream_pid),
            );
            self.record_result(
                batch,
                wave_index,
                case_a,
                selected_a.rationale.as_str(),
                result_a,
            );
            self.record_result(
                batch,
                wave_index,
                case_b,
                selected_b.rationale.as_str(),
                result_b,
            );
        } else {
            self.mark_in_progress(&[case_a], wave_index)?;
            let result_a =
                run_mixed_input_case_on_active_stack(case_a, env_a, &stack.api, stack.restream_pid)
                    .await;
            self.record_result(
                batch,
                wave_index,
                case_a,
                selected_a.rationale.as_str(),
                result_a,
            );
        }
        self.write_progress()
    }
}

fn apply_fast_breadth_defaults(
    env: &mut MixedEnv,
    setup: &ListMatrixSetup,
    checks: &[&'static str],
) {
    if !setup.explicit_n_per_group {
        env.n_per_group = 1;
    }
    if !setup.explicit_only_checks {
        env.only_checks = Some(checks.iter().map(|check| check.to_string()).collect());
    }
    if !setup.explicit_skip_load {
        env.skip_load = true;
    }
    if !setup.explicit_collect_failures {
        env.collect_failures = true;
    }
    if !setup.explicit_assertion_log {
        env.assertion_log = Some(setup.root.join("assertions.jsonl"));
    }
}

fn write_fast_breadth_progress(
    scenario_path: &Path,
    case_progress: &[MatrixCaseProgress],
    failures: &[String],
) -> Result<(), String> {
    write_matrix_scenario_progress_for_mode(
        scenario_path,
        MIXED_FAST_BREADTH_MODE,
        "fast-breadth",
        false,
        case_progress,
        failures,
    )
}

/// Final fast-breadth coverage/result JSON.
fn fast_breadth_report(
    setup: &ListMatrixSetup,
    covered_output_cells: usize,
    total_output_cells: usize,
    results: Vec<Value>,
) -> Value {
    json!({
        "passed": true,
        "mode": MIXED_FAST_BREADTH_MODE,
        "coverage": {
            "selectedInputCases": setup.selected_cases.len(),
            "totalInputCases": mixed_input_cases().len(),
            "selectedOutputCells": covered_output_cells,
            "totalOutputCells": total_output_cells,
            "nPerGroup": std::env::var("N_PER_GROUP").ok().unwrap_or_else(|| "1".to_string()),
            "selectedBatchGroups": setup.selected_batches
                .iter()
                .map(|batch| batch.group.as_str())
                .collect::<Vec<_>>(),
            "defaultChecks": if setup.explicit_only_checks {
                Value::Null
            } else {
                setup.selected_cases.iter().map(|case| {
                    let selected = mixed_fast_breadth_selected(*case);
                    json!({
                        "id": case.scenario_id(),
                        "checks": selected.check_names(),
                    })
                }).collect::<Vec<_>>().into()
            },
            "defaultSkipLoad": !setup.explicit_skip_load,
            "defaultCollectFailures": !setup.explicit_collect_failures,
            "defaultAssertionLog": if setup.explicit_assertion_log {
                Value::Null
            } else {
                setup.root.join("assertions.jsonl").to_string_lossy().into_owned().into()
            },
            "sharedBatches": setup.selected_batches.iter().map(|batch| {
                json!({
                    "group": batch.group.as_str(),
                    "cases": batch.cases.iter().map(|case| case.scenario_id()).collect::<Vec<_>>(),
                    "maxConcurrentPipelines": batch.cases.len().min(2),
                })
            }).collect::<Vec<_>>(),
        },
        "inputCases": setup.selected_cases.iter().map(|case| {
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
    })
}

/// Environment-derived flags, paths, and selected batches for list modes.
struct ListMatrixSetup {
    root: PathBuf,
    selected_batches: Vec<&'static MixedFastBreadthBatch>,
    selected_cases: Vec<MixedInputCase>,
    explicit_n_per_group: bool,
    explicit_only_checks: bool,
    explicit_skip_load: bool,
    explicit_collect_failures: bool,
    explicit_assertion_log: bool,
}

impl ListMatrixSetup {
    fn from_batches(
        selected_batches: Vec<&'static MixedFastBreadthBatch>,
        default_work_dir: fn() -> PathBuf,
    ) -> Self {
        let root = std::env::var_os("WORK_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(default_work_dir);
        let explicit_n_per_group = std::env::var_os("N_PER_GROUP").is_some();
        let explicit_only_checks = std::env::var_os("ONLY_CHECKS").is_some();
        let explicit_skip_load = std::env::var_os("SKIP_LOAD").is_some();
        let explicit_collect_failures = std::env::var_os("COLLECT_FAILURES").is_some();
        let explicit_assertion_log = std::env::var_os("ASSERTION_LOG").is_some();
        let selected_cases: Vec<MixedInputCase> = selected_batches
            .iter()
            .flat_map(|batch| batch.cases.iter().copied())
            .collect();
        Self {
            root,
            selected_batches,
            selected_cases,
            explicit_n_per_group,
            explicit_only_checks,
            explicit_skip_load,
            explicit_collect_failures,
            explicit_assertion_log,
        }
    }

    fn total_output_cells(&self) -> usize {
        self.selected_cases
            .iter()
            .map(|case| mixed_output_cases_for_input(*case).len())
            .sum()
    }
}

pub(crate) async fn mixed_signal_correctness() -> Result<Value, String> {
    let setup = ListMatrixSetup::from_batches(
        selected_mixed_signal_batches()?,
        mixed_signal_default_work_dir,
    );
    let mut results = Vec::new();
    let mut failures = Vec::new();
    let mut covered_output_cells = 0usize;
    let total_output_cells = setup.total_output_cells();

    {
        let mut run = SignalRun {
            setup: &setup,
            failures: &mut failures,
            results: &mut results,
            covered_output_cells: &mut covered_output_cells,
        };
        for batch in &setup.selected_batches {
            run_one_signal_batch(&mut run, batch).await?;
        }
    }

    if !failures.is_empty() {
        return Err(format!(
            "mixed signal failed {} selected case(s): {}",
            failures.len(),
            failures.join(" | ")
        ));
    }

    Ok(mixed_signal_report(
        &setup,
        covered_output_cells,
        total_output_cells,
        results,
    ))
}

/// Runs one signal-sentinel batch on its own shared stack and emits its timing.
async fn run_one_signal_batch(
    run: &mut SignalRun<'_>,
    batch: &MixedFastBreadthBatch,
) -> Result<(), String> {
    let stack_mode = format!("{MIXED_SIGNAL_MODE}.{}", batch.group.as_str());
    let stack_env = MixedEnv::from_env_with_default_work_dir(
        &stack_mode,
        run.setup.root.join("_shared").join(batch.group.as_str()),
    );
    let mut stack = start_mixed_harness_stack(stack_env).await?;

    let wave_started = Instant::now();
    let mut wave_index = 0usize;
    for (case_a, case_b) in plan_shared_waves(&batch.cases) {
        wave_index += 1;
        run.run_wave(batch, &mut stack, wave_index, case_a, case_b)
            .await?;
    }
    emit_mixed_timing(
        &stack.env,
        MIXED_SIGNAL_MODE,
        &format!("batch.{}", batch.group.as_str()),
        if run.failures.is_empty() {
            "pass"
        } else {
            "fail"
        },
        wave_started.elapsed(),
        Some(json!({
            "group": batch.group.as_str(),
            "cases": batch.cases.iter().map(|case| case.scenario_id()).collect::<Vec<_>>(),
        })),
    )?;
    stop_mixed_harness_stack(&mut stack).await;
    Ok(())
}

/// Matrix bookkeeping plus selection flags for one signal-sentinel execution.
struct SignalRun<'a> {
    setup: &'a ListMatrixSetup,
    failures: &'a mut Vec<String>,
    results: &'a mut Vec<Value>,
    covered_output_cells: &'a mut usize,
}

impl SignalRun<'_> {
    fn record_result(
        &mut self,
        batch: &MixedFastBreadthBatch,
        wave_index: usize,
        case: MixedInputCase,
        rationale: &str,
        result: Result<Value, String>,
    ) {
        match result {
            Ok(mut value) => {
                value["signalRationale"] = json!(rationale);
                value["batchGroup"] = json!(batch.group.as_str());
                value["wave"] = json!(wave_index);
                self.results.push(value);
            }
            Err(error) => self
                .failures
                .push(format!("{}: {error}", case.scenario_id())),
        }
    }

    fn env_for(
        &self,
        case: MixedInputCase,
        stack_env: &MixedEnv,
        second_slot: bool,
        checks: &[&'static str],
    ) -> Result<MixedEnv, String> {
        let setup = self.setup;
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
        apply_signal_defaults(&mut env, setup, checks);
        Ok(env)
    }

    async fn run_wave(
        &mut self,
        batch: &MixedFastBreadthBatch,
        stack: &mut MixedHarnessStack,
        wave_index: usize,
        case_a: MixedInputCase,
        case_b: Option<MixedInputCase>,
    ) -> Result<(), String> {
        let selected_a = mixed_signal_selected(case_a);
        let checks_a = selected_a.check_names();
        let env_a = self.env_for(case_a, &stack.env, false, &checks_a)?;
        *self.covered_output_cells += mixed_output_cases_for_input(case_a).len();
        if let Some(case_b) = case_b {
            let selected_b = mixed_signal_selected(case_b);
            let checks_b = selected_b.check_names();
            let env_b = self.env_for(case_b, &stack.env, true, &checks_b)?;
            *self.covered_output_cells += mixed_output_cases_for_input(case_b).len();
            let (result_a, result_b) = tokio::join!(
                run_mixed_input_case_on_active_stack(case_a, env_a, &stack.api, stack.restream_pid),
                run_mixed_input_case_on_active_stack(case_b, env_b, &stack.api, stack.restream_pid),
            );
            self.record_result(
                batch,
                wave_index,
                case_a,
                selected_a.rationale.as_str(),
                result_a,
            );
            self.record_result(
                batch,
                wave_index,
                case_b,
                selected_b.rationale.as_str(),
                result_b,
            );
        } else {
            let result_a =
                run_mixed_input_case_on_active_stack(case_a, env_a, &stack.api, stack.restream_pid)
                    .await;
            self.record_result(
                batch,
                wave_index,
                case_a,
                selected_a.rationale.as_str(),
                result_a,
            );
        }
        Ok(())
    }
}

fn apply_signal_defaults(env: &mut MixedEnv, setup: &ListMatrixSetup, checks: &[&'static str]) {
    if !setup.explicit_n_per_group {
        env.n_per_group = 1;
    }
    if !setup.explicit_only_checks {
        env.only_checks = Some(checks.iter().map(|check| check.to_string()).collect());
    }
    if !setup.explicit_skip_load {
        env.skip_load = true;
    }
    if !setup.explicit_collect_failures {
        env.collect_failures = true;
    }
    if !setup.explicit_assertion_log {
        env.assertion_log = Some(setup.root.join("assertions.jsonl"));
    }
}

/// Final signal-sentinel coverage/result JSON.
fn mixed_signal_report(
    setup: &ListMatrixSetup,
    covered_output_cells: usize,
    total_output_cells: usize,
    results: Vec<Value>,
) -> Value {
    json!({
        "passed": true,
        "mode": MIXED_SIGNAL_MODE,
        "coverage": {
            "selectedInputCases": setup.selected_cases.len(),
            "totalInputCases": mixed_input_cases().len(),
            "selectedOutputCells": covered_output_cells,
            "totalOutputCells": total_output_cells,
            "nPerGroup": std::env::var("N_PER_GROUP").ok().unwrap_or_else(|| "1".to_string()),
            "selectedBatchGroups": setup.selected_batches
                .iter()
                .map(|batch| batch.group.as_str())
                .collect::<Vec<_>>(),
            "defaultChecks": if setup.explicit_only_checks {
                Value::Null
            } else {
                setup.selected_cases.iter().map(|case| {
                    let selected = mixed_signal_selected(*case);
                    json!({
                        "id": case.scenario_id(),
                        "checks": selected.check_names(),
                    })
                }).collect::<Vec<_>>().into()
            },
            "defaultSkipLoad": !setup.explicit_skip_load,
            "defaultCollectFailures": !setup.explicit_collect_failures,
            "defaultAssertionLog": if setup.explicit_assertion_log {
                Value::Null
            } else {
                setup.root.join("assertions.jsonl").to_string_lossy().into_owned().into()
            },
            "sharedBatches": setup.selected_batches.iter().map(|batch| {
                json!({
                    "group": batch.group.as_str(),
                    "cases": batch.cases.iter().map(|case| case.scenario_id()).collect::<Vec<_>>(),
                    "maxConcurrentPipelines": batch.cases.len().min(2),
                })
            }).collect::<Vec<_>>(),
        },
        "inputCases": setup.selected_cases.iter().map(|case| {
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
    })
}
