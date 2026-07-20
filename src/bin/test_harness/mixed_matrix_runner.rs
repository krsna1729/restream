//! Fast-breadth and signal shared-batch orchestration.

use super::*;

#[path = "mixed_matrix_execution.rs"]
mod mixed_matrix_execution;
#[path = "mixed_matrix_progress.rs"]
mod mixed_matrix_progress;

pub(super) use mixed_matrix_execution::{
    mixed_input_matrix_correctness_serial, mixed_input_matrix_correctness_shared,
};
use mixed_matrix_progress::{
    MatrixCaseState, apply_mixed_matrix_defaults, matrix_case_progress_json,
    matrix_mark_case_state, matrix_progress_totals, write_matrix_scenario_progress_for_mode,
};
pub(crate) use mixed_matrix_progress::{
    matrix_case_progress_rows, mixed_matrix_cases_can_share_wave, mixed_matrix_default_check_names,
    mixed_matrix_fail_fast, verify_mixed_runtime_log_hygiene, write_json_pretty_atomic,
    write_matrix_scenario_progress,
};
#[cfg(test)]
pub(crate) use mixed_matrix_progress::{
    mixed_runtime_log_noise_lines, mixed_runtime_log_noise_matches,
};

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
