//! Serial and shared-stack full mixed-matrix execution.

use super::*;

pub(in super::super) async fn mixed_input_matrix_correctness_serial() -> Result<Value, String> {
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

pub(in super::super) async fn mixed_input_matrix_correctness_shared() -> Result<Value, String> {
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
