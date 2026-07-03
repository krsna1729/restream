//! Mixed-matrix runtime, sinks, probes, and assertion helpers.

use super::*;

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
            av_signal_seconds: env_secs("AV_SIGNAL_SECONDS", 20),
            av_soak_seconds: env_secs("AV_SOAK_SECONDS", 120),
            n_per_group: env_usize("N_PER_GROUP", 25),
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

pub(super) fn mixed_output_checks_need_live_progress_gate(only_checks: Option<&[String]>) -> bool {
    let check_selected =
        |check: &str| only_checks.is_none_or(|items| items.iter().any(|item| item == check));
    let direct_signal_sinks = only_checks.is_some_and(|items| {
        !items.is_empty()
            && items
                .iter()
                .all(|item| item == "signal" || item == "soak-drift")
    });
    check_selected("ffprobe") || (check_selected("signal") && !direct_signal_sinks)
}

pub(super) fn mixed_progress_output_ids(
    output_ids: &[String],
    non_progress_output_id: &str,
) -> Vec<String> {
    output_ids
        .iter()
        .filter(|output_id| output_id.as_str() != non_progress_output_id)
        .cloned()
        .collect()
}

/// Resume gate that skips mixed checks until a requested assertion id is reached.
pub(super) struct MixedResume {
    pub(super) target: Option<String>,
    pub(super) active: bool,
}

impl MixedResume {
    pub(super) fn new(target: Option<String>) -> Self {
        Self {
            active: target.is_none(),
            target,
        }
    }

    pub(super) fn allows(&mut self, id: &str) -> bool {
        if self.active {
            return true;
        }
        if self.target.as_deref() == Some(id) {
            self.active = true;
            return true;
        }
        false
    }
}

/// Shared live stack for mixed harness waves.
pub(super) struct MixedHarnessStack {
    pub(super) env: MixedEnv,
    pub(super) mediamtx: Child,
    pub(super) restream: Child,
    pub(super) api: RampApi,
    pub(super) restream_pid: u32,
}

pub(super) async fn start_mixed_harness_stack(env: MixedEnv) -> Result<MixedHarnessStack, String> {
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(&env.media_dir).map_err(|e| e.to_string())?;
    let mediamtx = start_mixed_mediamtx(&env).await?;
    let restream = start_mixed_restream(&env).await?;
    let restream_pid = restream.id().ok_or("restream pid missing")?;
    let mut api = RampApi::new(env.restream_http);
    api.login().await?;
    Ok(MixedHarnessStack {
        env,
        mediamtx,
        restream,
        api,
        restream_pid,
    })
}

pub(super) async fn stop_mixed_harness_stack(stack: &mut MixedHarnessStack) {
    stop_child(&mut stack.restream).await;
    stop_child(&mut stack.mediamtx).await;
}

pub(super) fn bind_mixed_env_to_shared_stack(env: &mut MixedEnv, stack_env: &MixedEnv) {
    env.restream_http = stack_env.restream_http;
    env.restream_rtmp = stack_env.restream_rtmp;
    env.restream_srt = stack_env.restream_srt;
    env.mtx_rtmp = stack_env.mtx_rtmp;
    env.mtx_srt = stack_env.mtx_srt;
    env.mtx_hls = stack_env.mtx_hls;
    env.mtx_api = stack_env.mtx_api;
    env.media_dir = stack_env.media_dir.clone();
    env.restream_log = stack_env.restream_log.clone();
    env.mediamtx_log = stack_env.mediamtx_log.clone();
    env.mediamtx_config = stack_env.mediamtx_config.clone();
    env.restream_db_path = stack_env.restream_db_path.clone();
}

pub(super) async fn mixed_input_case_correctness(case: MixedInputCase) -> Result<Value, String> {
    let mode = mixed_input_mode_name(case);
    let env = MixedEnv::from_env_with_default_work_dir(&mode, mixed_input_default_work_dir(case));
    run_mixed_input_case_with_env(case, env).await
}

pub(super) async fn mixed_input_matrix_correctness() -> Result<Value, String> {
    let root = std::env::var_os("WORK_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(mixed_matrix_default_work_dir);
    let mut results = Vec::new();
    for case in mixed_input_cases() {
        let mode = mixed_input_mode_name(*case);
        let env =
            MixedEnv::from_env_with_default_work_dir(&mode, root.join(case.artifact_rel_dir()));
        match run_mixed_input_case_with_env(*case, env).await {
            Ok(result) => results.push(result),
            Err(error) => {
                return Err(format!(
                    "mixed input case {} failed: {error}",
                    case.scenario_id()
                ));
            }
        }
    }
    Ok(json!({
        "passed": true,
        "mode": MIXED_MATRIX_MODE,
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
            run_mixed_single_live_config(&env, api, restream_pid, case, &mut resume).await
        }
        (MixedSourceAdapter::RtmpPublisher, MixedVideoCodec::H264, false) => {
            run_mixed_single_live_config(&env, api, restream_pid, case, &mut resume).await
        }
        (MixedSourceAdapter::SrtPublisher, MixedVideoCodec::H264, true) => {
            run_mixed_srt_multi_config(&env, api, restream_pid, case, &mut resume).await
        }
        (MixedSourceAdapter::SrtPublisher, MixedVideoCodec::H265, true) => {
            run_mixed_srt_multi_config(&env, api, restream_pid, case, &mut resume).await
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

    config.map(|config| {
        json!({
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
        })
    })
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

pub(super) async fn start_mixed_restream(env: &MixedEnv) -> Result<Child, String> {
    std::fs::create_dir_all(&env.media_dir).map_err(|e| e.to_string())?;
    start_restream_child_in_media_dir(
        &env.restream_bin,
        &TestPorts {
            http: env.restream_http,
            rtmp: env.restream_rtmp,
            srt: env.restream_srt,
        },
        &env.restream_db_path,
        &env.restream_log,
        &env.media_dir,
    )
    .await
}

pub(super) async fn start_mixed_mediamtx(env: &MixedEnv) -> Result<Child, String> {
    std::fs::write(
        &env.mediamtx_config,
        format!(
            "logLevel: warn\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: yes\nhlsAddress: :{}\nhlsPartDuration: 200ms\nhlsSegmentDuration: 2s\nwebrtc: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            env.mtx_rtmp, env.mtx_srt, env.mtx_hls, env.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let log = std::fs::File::create(&env.mediamtx_log).map_err(|e| e.to_string())?;
    let stderr_log = log.try_clone().map_err(|e| e.to_string())?;
    let mut child = Command::new("mediamtx")
        .arg(&env.mediamtx_config)
        .env_remove("MTX_RTMP")
        .env_remove("MTX_SRT")
        .env_remove("MTX_HLS")
        .env_remove("MTX_API")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", env.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut child).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }
    Ok(child)
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
    let hls_output = create_mixed_output(
        api,
        &pipeline_id,
        "hls-preview",
        &format!("hls://{cfg}-preview"),
        "source",
    )
    .await?;
    start_mixed_output(api, &pipeline_id, &hls_output).await?;
    output_ids.push(hls_output.clone());

    add_mixed_group(
        env,
        api,
        &pipeline_id,
        MixedGroupSpec {
            cfg,
            group: "rtmp.src.a0",
            count: n,
            encoding: "source",
        },
        |index| {
            format!(
                "rtmp://127.0.0.1:{}/live/{cfg}-rtmp.src.a0-{index}",
                env.mtx_rtmp
            )
        },
        &mut output_ids,
    )
    .await?;
    if !env.skip_load {
        snapshot_mixed(env, restream_pid, cfg, &format!("after {n} RTMP source")).await?;
    }

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

    add_mixed_group(
        env,
        api,
        &pipeline_id,
        MixedGroupSpec {
            cfg,
            group: "rtmp.720p.a0",
            count: n,
            encoding: "720p",
        },
        |index| {
            format!(
                "rtmp://127.0.0.1:{}/live/{cfg}-rtmp.720p.a0-{index}",
                env.mtx_rtmp
            )
        },
        &mut output_ids,
    )
    .await?;
    if !env.skip_load {
        snapshot_mixed(env, restream_pid, cfg, &format!("after {n} RTMP 720p")).await?;
    }

    add_mixed_group(
        env,
        api,
        &pipeline_id,
        MixedGroupSpec {
            cfg,
            group: "rtmp.1080p.a0",
            count: n,
            encoding: "1080p",
        },
        |index| {
            format!(
                "rtmp://127.0.0.1:{}/live/{cfg}-rtmp.1080p.a0-{index}",
                env.mtx_rtmp
            )
        },
        &mut output_ids,
    )
    .await?;
    if !env.skip_load {
        snapshot_mixed(env, restream_pid, cfg, &format!("after {n} RTMP 1080p")).await?;
    }

    add_mixed_group(
        env,
        api,
        &pipeline_id,
        MixedGroupSpec {
            cfg,
            group: "srt.src.a0",
            count: n,
            encoding: "source",
        },
        |index| {
            format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{cfg}-srt.src.a0-{index}",
                env.mtx_srt
            )
        },
        &mut output_ids,
    )
    .await?;
    if !env.skip_load {
        snapshot_mixed(env, restream_pid, cfg, &format!("after {n} SRT source")).await?;
    }

    add_mixed_group(
        env,
        api,
        &pipeline_id,
        MixedGroupSpec {
            cfg,
            group: "srt.720p.a0",
            count: n,
            encoding: "720p",
        },
        |index| {
            format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{cfg}-srt.720p.a0-{index}",
                env.mtx_srt
            )
        },
        &mut output_ids,
    )
    .await?;
    if !env.skip_load {
        snapshot_mixed(env, restream_pid, cfg, &format!("after {n} SRT 720p")).await?;
    }

    add_mixed_group(
        env,
        api,
        &pipeline_id,
        MixedGroupSpec {
            cfg,
            group: "srt.1080p.a0",
            count: n,
            encoding: "1080p",
        },
        |index| {
            format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{cfg}-srt.1080p.a0-{index}",
                env.mtx_srt
            )
        },
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
            Duration::from_secs(60),
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
    let (sink_probe_result, sink_probe_failure) =
        run_optional_mixed_sink_probe(env, api, &pipeline_id, cfg, &mut output_ids, resume).await?;

    let mut hls_put_probe_result = None;
    if env.check_selected("hls-put-probe")
        && resume.allows(&mixed_scenario_check_id(cfg, "hls_put"))
    {
        let started = Instant::now();
        let put_port = harness_port_defaults().hls_put;
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

pub(super) async fn run_mixed_single_live_config(
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

    let mut publisher = spawn_mixed_live_publisher(env, case, &stream_key).await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(45)).await?;
    let recording = verify_mixed_recording(env, api, cfg, &pipeline_id, case, resume).await?;
    verify_optional_mixed_hls_preview(env, api, cfg, &pipeline_id, case, resume).await?;
    let rss_baseline = process_rss_kb(restream_pid).await.unwrap_or(0);
    if !env.skip_load {
        snapshot_mixed(env, restream_pid, cfg, "baseline (input live, 0 outputs)").await?;
    }

    let mut output_ids = Vec::with_capacity(total);
    let mut ffmpeg_signal_sinks = Vec::new();
    let mut next_ffmpeg_signal_sink = 0usize;
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
    verify_mixed_graph_stage_sharing(env, api, cfg, &pipeline_id, case, resume).await?;
    if !ffmpeg_signal_sinks.is_empty() {
        finish_ffmpeg_signal_sinks(env, &mut ffmpeg_signal_sinks, resume).await?;
    }

    let rss = record_mixed_rss_delta(env, cfg, restream_pid, rss_baseline, total, None).await?;

    verify_mixed_output_cases(env, cfg, output_cases, resume).await?;

    let (sink_probe_result, sink_probe_failure) =
        run_optional_mixed_sink_probe(env, api, &pipeline_id, cfg, &mut output_ids, resume).await?;

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
        "recording": recording,
        "outputMatrix": mixed_output_matrix_json(output_cases),
    });
    if let Some(probe) = sink_probe_result {
        result["sinkProbe"] = probe.summary;
        result["sinkProbePassed"] = json!(probe.passed);
    }
    Ok(result)
}

pub(super) async fn run_mixed_srt_multi_config(
    env: &MixedEnv,
    api: &RampApi,
    restream_pid: u32,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<Value, String> {
    let n = env.n_per_group;
    let cfg = case.scenario_id();
    let output_cases = mixed_output_cases_for_input(case);
    let total = n * output_cases.len();
    let (pipeline_id, stream_key) = create_mixed_pipeline(api, cfg).await?;

    let mut publisher = spawn_mixed_srt_multi_publisher(env, case, &stream_key).await?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(45)).await?;

    verify_optional_mixed_hls_preview(env, api, cfg, &pipeline_id, case, resume).await?;
    let recording = verify_mixed_recording(env, api, cfg, &pipeline_id, case, resume).await?;

    // Verify adaptive ring sizing: 2-audio-track SRT stream → 100+ pkt/s →
    // ring must have grown beyond the 1024-slot default and hold ≥ 5 s of depth.
    let ring_check_id = mixed_scenario_check_id(cfg, "adaptive_source_ring");
    if env.check_selected("ffprobe") || resume.allows(&ring_check_id) {
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
                                    "adaptive-ring {cfg}: cap={} depth={:.1}s \
                             overflows={}{}",
                                    snapshot.capacity,
                                    snapshot.depth_secs,
                                    snapshot.overflows,
                                    if snapshot.resized { " [resized]" } else { "" }
                                ),
                            )?;
                            break;
                        } else {
                            return Err(format!(
                                "adaptive ring check failed for {cfg}: cap={} depth={:.1}s overflows={}",
                                snapshot.capacity, snapshot.depth_secs, snapshot.overflows
                            ));
                        }
                    }
                }
                Err(error) => {
                    last_error = Some(error);
                }
            }
            if Instant::now() >= deadline {
                let error =
                    last_error.unwrap_or_else(|| "telemetry never became ready".to_string());
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

    let rss_baseline = process_rss_kb(restream_pid).await.unwrap_or(0);
    if !env.skip_load {
        snapshot_mixed(env, restream_pid, cfg, "baseline (input live, 0 outputs)").await?;
    }

    let mut output_ids = Vec::with_capacity(total);
    let mut ffmpeg_srt_sinks = Vec::new();
    let mut next_ffmpeg_srt_sink = 0usize;
    let mut ffmpeg_signal_sinks = Vec::new();
    let mut next_ffmpeg_signal_sink = 0usize;
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
    verify_mixed_graph_stage_sharing(env, api, cfg, &pipeline_id, case, resume).await?;
    if !ffmpeg_signal_sinks.is_empty() {
        finish_ffmpeg_signal_sinks(env, &mut ffmpeg_signal_sinks, resume).await?;
    }

    let rss = record_mixed_rss_delta(env, cfg, restream_pid, rss_baseline, total, Some(2)).await?;

    if !ffmpeg_srt_sinks.is_empty() {
        finish_ffmpeg_srt_sinks(&mut ffmpeg_srt_sinks).await?;
    }

    verify_mixed_output_cases_inner(env, cfg, output_cases, resume, true, true).await?;

    let (sink_probe_result, sink_probe_failure) =
        run_optional_mixed_sink_probe(env, api, &pipeline_id, cfg, &mut output_ids, resume).await?;

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
    if let Some(probe) = sink_probe_result {
        result["sinkProbe"] = probe.summary;
        result["sinkProbePassed"] = json!(probe.passed);
    }
    Ok(result)
}

pub(super) async fn spawn_mixed_live_publisher(
    env: &MixedEnv,
    case: MixedInputCase,
    stream_key: &str,
) -> Result<Child, String> {
    let log_path = env
        .work_dir
        .join(format!("{}.publisher.log", case.scenario_id()));
    let fixture = mixed_input_fixture(case)?;
    let (url, format) = match case.protocol() {
        MixedInputProtocol::Rtmp => (
            format!("rtmp://127.0.0.1:{}/live/{stream_key}", env.restream_rtmp),
            "flv",
        ),
        MixedInputProtocol::Srt => (
            format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{stream_key}&latency=200000",
                env.restream_srt
            ),
            "mpegts",
        ),
        MixedInputProtocol::File => {
            return Err(format!(
                "{} uses file ingest and cannot spawn a live publisher",
                case.scenario_id()
            ));
        }
    };
    spawn_publisher_with_selection(
        &fixture,
        &url,
        format,
        PublishTrackSelection::PrimaryAv,
        Some(&log_path),
    )
}

pub(super) async fn spawn_mixed_srt_multi_publisher(
    env: &MixedEnv,
    case: MixedInputCase,
    stream_key: &str,
) -> Result<Child, String> {
    let log_path = env
        .work_dir
        .join(format!("{}.publisher.log", case.scenario_id()));
    let fixture = mixed_input_fixture(case)?;
    spawn_publisher_with_selection(
        &fixture,
        &format!(
            "srt://127.0.0.1:{}?streamid=publish:live/{stream_key}&latency=200000",
            env.restream_srt
        ),
        "mpegts",
        PublishTrackSelection::AllStreams,
        Some(&log_path),
    )
}

pub(super) async fn create_mixed_output(
    api: &RampApi,
    pipeline_id: &str,
    name: &str,
    url: &str,
    encoding: &str,
) -> Result<String, String> {
    let output = api
        .post_json(
            &format!("/api/v1/pipelines/{pipeline_id}/outputs"),
            output_create_payload(name, url, encoding),
        )
        .await?;
    output["output"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or("output create response missing output.id".to_string())
}

pub(super) async fn start_mixed_output(
    api: &RampApi,
    pipeline_id: &str,
    output_id: &str,
) -> Result<(), String> {
    api.post_json(
        &format!("/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/start"),
        Value::Null,
    )
    .await
    .map(|_| ())
}

pub(super) async fn run_optional_mixed_sink_probe(
    env: &MixedEnv,
    api: &RampApi,
    pipeline_id: &str,
    cfg: &str,
    output_ids: &mut Vec<String>,
    resume: &mut MixedResume,
) -> Result<(Option<SinkProbeResult>, Option<String>), String> {
    let probe_id = mixed_scenario_check_id(cfg, "sink_probe");
    if !env.check_selected("sink-probe") || !resume.allows(&probe_id) {
        return Ok((None, None));
    }

    let started = Instant::now();
    let sink_port = harness_port_defaults().sink;
    match run_sink_probe(api, pipeline_id, cfg, "source", sink_port, 30).await {
        Ok(probe) => {
            let status = if probe.passed { "pass" } else { "fail" };
            emit_mixed_result(
                env,
                cfg,
                &probe_id,
                status,
                started.elapsed(),
                Some(probe.summary.clone()),
            )?;
            output_ids.push(probe.output_id.clone());
            let failure = if probe.passed {
                None
            } else {
                Some(format!("{cfg}: sink probe failed: {}", probe.summary))
            };
            Ok((Some(probe), failure))
        }
        Err(error) => {
            emit_mixed_result(
                env,
                cfg,
                &probe_id,
                "fail",
                started.elapsed(),
                Some(json!({"error": error.clone()})),
            )?;
            Ok((None, Some(format!("{cfg}: sink probe error: {error}"))))
        }
    }
}

pub(super) struct MixedRssReport {
    pub(super) delta_kb: u64,
    pub(super) per_output_kb: u64,
    pub(super) ffmpeg: FfmpegStats,
}

pub(super) async fn record_mixed_rss_delta(
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

/// Parameters for creating a homogeneous group of mixed-matrix outputs.
pub(super) struct MixedGroupSpec<'a> {
    pub(super) cfg: &'a str,
    pub(super) group: &'a str,
    pub(super) count: usize,
    pub(super) encoding: &'a str,
}

/// Direct FFmpeg SRT listener used to validate SRT egress without MediaMTX.
pub(super) struct FfmpegSrtSink {
    pub(super) group: String,
    pub(super) index: usize,
    pub(super) port: u16,
    pub(super) log_path: PathBuf,
    pub(super) expected_dimensions: String,
    pub(super) expected_audio_tracks: usize,
    pub(super) child: Child,
}

/// Direct FFmpeg RTMP/SRT listener used for AV marker signal capture.
pub(super) struct FfmpegSignalSink {
    pub(super) cfg: String,
    pub(super) group: String,
    pub(super) index: usize,
    pub(super) publish_url: String,
    pub(super) capture_path: PathBuf,
    pub(super) child: Child,
}

pub(super) async fn add_mixed_group<F>(
    env: &MixedEnv,
    api: &RampApi,
    pipeline_id: &str,
    spec: MixedGroupSpec<'_>,
    url_for: F,
    output_ids: &mut Vec<String>,
) -> Result<(), String>
where
    F: Fn(usize) -> String,
{
    let started = Instant::now();
    for index in 1..=spec.count {
        let output_id = create_mixed_output(
            api,
            pipeline_id,
            &format!("{}-{index}", spec.group),
            &url_for(index),
            spec.encoding,
        )
        .await?;
        start_mixed_output(api, pipeline_id, &output_id).await?;
        output_ids.push(output_id);
    }
    println!(
        "[mixed-input] added {} {} outputs for {}",
        spec.count, spec.group, spec.cfg
    );
    emit_mixed_timing(
        env,
        spec.cfg,
        &format!("outputs.create.{}", spec.group),
        "pass",
        started.elapsed(),
        Some(json!({
            "group": spec.group,
            "count": spec.count,
            "encoding": spec.encoding,
        })),
    )?;
    Ok(())
}

pub(super) fn mixed_output_publish_url(
    env: &MixedEnv,
    cfg: &str,
    case: &MixedOutputCase,
    index: usize,
) -> String {
    let output_name = mixed_output_instance_name(cfg, case.id(), index);
    match case.protocol() {
        MixedOutputProtocol::Rtmp => {
            format!("rtmp://127.0.0.1:{}/live/{output_name}", env.mtx_rtmp)
        }
        MixedOutputProtocol::Srt => {
            format!(
                "srt://127.0.0.1:{}?streamid=publish:live/{output_name}",
                env.mtx_srt
            )
        }
    }
}

pub(super) fn mixed_output_read_url(
    env: &MixedEnv,
    cfg: &str,
    case: &MixedOutputCase,
    index: usize,
) -> String {
    let output_name = mixed_output_instance_name(cfg, case.id(), index);
    match case.protocol() {
        MixedOutputProtocol::Rtmp => mixed_output_publish_url(env, cfg, case, index),
        MixedOutputProtocol::Srt => {
            format!(
                "srt://127.0.0.1:{}?streamid=read:live/{output_name}&timeout=30000000",
                env.mtx_srt
            )
        }
    }
}

pub(super) fn mixed_output_matrix_json(cases: &[MixedOutputCase]) -> Vec<Value> {
    cases
        .iter()
        .map(|case| {
            let mut value = json!({
                "id": case.id(),
                "protocol": mixed_output_protocol_name(case.protocol()),
                "encoding": case.encoding(),
                "expectedDimensions": case.expected_dimensions(),
                "expectedAudioTracks": case.expected_audio_tracks(),
            });
            if let Some(track) = case.selected_audio_track() {
                value["selectedAudioTrack"] = json!(track);
            }
            value
        })
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn add_mixed_output_cases(
    env: &MixedEnv,
    api: &RampApi,
    pipeline_id: &str,
    restream_pid: u32,
    cfg: &str,
    cases: &[MixedOutputCase],
    signal_sinks: &mut Vec<FfmpegSignalSink>,
    next_signal_sink_offset: &mut usize,
    output_ids: &mut Vec<String>,
) -> Result<(), String> {
    for case in cases {
        let mut direct_urls = Vec::new();
        if env.use_direct_signal_sinks() {
            for index in 1..=env.n_per_group {
                let sink =
                    spawn_ffmpeg_signal_sink(env, cfg, case, index, *next_signal_sink_offset)
                        .await?;
                *next_signal_sink_offset += 1;
                direct_urls.push(sink.publish_url.clone());
                signal_sinks.push(sink);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        add_mixed_group(
            env,
            api,
            pipeline_id,
            MixedGroupSpec {
                cfg,
                group: case.id(),
                count: env.n_per_group,
                encoding: case.encoding(),
            },
            |index| {
                direct_urls
                    .get(index.saturating_sub(1))
                    .cloned()
                    .unwrap_or_else(|| mixed_output_publish_url(env, cfg, case, index))
            },
            output_ids,
        )
        .await?;
        if !env.skip_load {
            snapshot_mixed(
                env,
                restream_pid,
                cfg,
                &format!("after {} {} outputs", env.n_per_group, case.id()),
            )
            .await?;
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn add_mixed_multi_output_cases(
    env: &MixedEnv,
    api: &RampApi,
    pipeline_id: &str,
    restream_pid: u32,
    cfg: &str,
    cases: &[MixedOutputCase],
    sinks: &mut Vec<FfmpegSrtSink>,
    next_sink_offset: &mut usize,
    signal_sinks: &mut Vec<FfmpegSignalSink>,
    next_signal_sink_offset: &mut usize,
    output_ids: &mut Vec<String>,
) -> Result<(), String> {
    for case in cases {
        let mut direct_urls = Vec::new();
        if env.use_direct_signal_sinks() {
            for index in 1..=env.n_per_group {
                let sink =
                    spawn_ffmpeg_signal_sink(env, cfg, case, index, *next_signal_sink_offset)
                        .await?;
                *next_signal_sink_offset += 1;
                direct_urls.push(sink.publish_url.clone());
                signal_sinks.push(sink);
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        match case.protocol() {
            MixedOutputProtocol::Rtmp => {
                add_mixed_group(
                    env,
                    api,
                    pipeline_id,
                    MixedGroupSpec {
                        cfg,
                        group: case.id(),
                        count: env.n_per_group,
                        encoding: case.encoding(),
                    },
                    |index| {
                        direct_urls
                            .get(index.saturating_sub(1))
                            .cloned()
                            .unwrap_or_else(|| mixed_output_publish_url(env, cfg, case, index))
                    },
                    output_ids,
                )
                .await?;
            }
            MixedOutputProtocol::Srt => {
                add_mixed_srt_group(
                    api,
                    pipeline_id,
                    env,
                    MixedGroupSpec {
                        cfg,
                        group: case.id(),
                        count: env.n_per_group,
                        encoding: case.encoding(),
                    },
                    |index| mixed_output_publish_url(env, cfg, case, index),
                    MixedSrtGroupValidation {
                        label: case.id(),
                        expected_dimensions: case.expected_dimensions(),
                        expected_audio_tracks: case.expected_audio_tracks(),
                    },
                    sinks,
                    next_sink_offset,
                    if direct_urls.is_empty() {
                        None
                    } else {
                        Some(direct_urls.clone())
                    },
                    output_ids,
                )
                .await?;
            }
        }
        if !env.skip_load {
            snapshot_mixed(
                env,
                restream_pid,
                cfg,
                &format!("after {} {} outputs", env.n_per_group, case.id()),
            )
            .await?;
        }
    }
    Ok(())
}

pub(super) async fn verify_mixed_output_cases(
    env: &MixedEnv,
    cfg: &str,
    cases: &[MixedOutputCase],
    resume: &mut MixedResume,
) -> Result<(), String> {
    verify_mixed_output_cases_inner(env, cfg, cases, resume, false, false).await
}

pub(super) async fn verify_mixed_output_dimensions(
    env: &MixedEnv,
    cfg: &str,
    cases: &[MixedOutputCase],
    resume: &mut MixedResume,
) -> Result<(), String> {
    if !env.check_selected("ffprobe") {
        return Ok(());
    }
    let index = env.n_per_group;
    for case in cases {
        let url = mixed_output_read_url(env, cfg, case, index);
        verify_mixed_stream(
            env,
            MixedProbeSpec {
                cfg,
                id: mixed_output_check_id(cfg, case.id(), "ffprobe"),
                label: &format!("{} out{index}", case.id()),
                url: &url,
                expected: case.expected_dimensions(),
                cookie: None,
            },
            resume,
        )
        .await?;
    }
    Ok(())
}

pub(super) async fn verify_mixed_output_cases_inner(
    env: &MixedEnv,
    cfg: &str,
    cases: &[MixedOutputCase],
    resume: &mut MixedResume,
    skip_direct_srt_sinks: bool,
    decode_scan: bool,
) -> Result<(), String> {
    if !env.check_selected("ffprobe") && !env.check_selected("signal") {
        return Ok(());
    }
    let started = Instant::now();
    let index = env.n_per_group;
    let mut failures = Vec::new();
    for case in cases {
        if skip_direct_srt_sinks
            && env.ffmpeg_srt_sink
            && matches!(case.protocol(), MixedOutputProtocol::Srt)
        {
            continue;
        }
        let url = mixed_output_read_url(env, cfg, case, index);
        let label = format!("{} out{index}", case.id());
        let mut output_failed = false;
        if env.check_selected("ffprobe") {
            let ffprobe_id = mixed_output_check_id(cfg, case.id(), "ffprobe");
            let ffprobe_result = verify_mixed_stream(
                env,
                MixedProbeSpec {
                    cfg,
                    id: ffprobe_id,
                    label: &label,
                    url: &url,
                    expected: case.expected_dimensions(),
                    cookie: None,
                },
                resume,
            )
            .await;
            if let Err(error) = ffprobe_result {
                if env.collect_failures {
                    output_failed = true;
                    failures.push(error);
                } else {
                    return Err(error);
                }
            }
        }
        if env.check_selected("ffprobe") && !output_failed {
            let audio_id = mixed_output_check_id(cfg, case.id(), "audio_route");
            let audio_result = verify_mixed_audio_route(
                env,
                cfg,
                &audio_id,
                &label,
                &url,
                case.expected_dimensions(),
                case.expected_audio_tracks(),
                resume,
            )
            .await;
            if let Err(error) = audio_result {
                if env.collect_failures {
                    output_failed = true;
                    failures.push(error);
                } else {
                    return Err(error);
                }
            }
        }
        if env.check_selected("ffprobe") && decode_scan && !output_failed {
            let decode_id = mixed_output_check_id(cfg, case.id(), "decode_scan");
            let decode_result =
                verify_mixed_decode_scan(env, cfg, &decode_id, &label, &url, resume).await;
            if let Err(error) = decode_result {
                if env.collect_failures {
                    failures.push(error);
                } else {
                    return Err(error);
                }
            }
        }
        if env.check_selected("signal") && !env.use_direct_signal_sinks() {
            let signal_id = mixed_output_check_id(cfg, case.id(), "signal");
            let signal_result =
                verify_mixed_signal_quality(env, cfg, &signal_id, &label, &url, resume).await;
            if let Err(error) = signal_result {
                if env.collect_failures {
                    failures.push(error);
                } else {
                    return Err(error);
                }
            }
        }
    }
    let status = if failures.is_empty() { "pass" } else { "fail" };
    emit_mixed_timing(
        env,
        cfg,
        "outputs.verify",
        status,
        started.elapsed(),
        Some(json!({
            "cases": cases.len(),
            "nPerGroup": env.n_per_group,
            "failureCount": failures.len(),
        })),
    )?;
    if !failures.is_empty() {
        return Err(format!(
            "{} mixed output check(s) failed: {}",
            failures.len(),
            failures.join(" | ")
        ));
    }
    Ok(())
}

/// Expected probe shape for one direct SRT sink group.
pub(super) struct MixedSrtGroupValidation<'a> {
    pub(super) label: &'a str,
    pub(super) expected_dimensions: &'a str,
    pub(super) expected_audio_tracks: usize,
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn add_mixed_srt_group<F>(
    api: &RampApi,
    pipeline_id: &str,
    env: &MixedEnv,
    spec: MixedGroupSpec<'_>,
    mediamtx_url_for: F,
    validation: MixedSrtGroupValidation<'_>,
    sinks: &mut Vec<FfmpegSrtSink>,
    next_sink_offset: &mut usize,
    signal_direct_urls: Option<Vec<String>>,
    output_ids: &mut Vec<String>,
) -> Result<(), String>
where
    F: Fn(usize) -> String,
{
    let mut direct_urls = Vec::new();
    if env.ffmpeg_srt_sink {
        for index in 1..=spec.count {
            let sink = spawn_ffmpeg_srt_sink(
                env,
                spec.cfg,
                spec.group,
                index,
                &validation,
                *next_sink_offset,
            )
            .await?;
            *next_sink_offset += 1;
            direct_urls.push(format!(
                "srt://127.0.0.1:{}?pkt_size=1316&latency=200000",
                sink.port
            ));
            sinks.push(sink);
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    add_mixed_group(
        env,
        api,
        pipeline_id,
        spec,
        |index| {
            signal_direct_urls
                .as_ref()
                .and_then(|urls| urls.get(index.saturating_sub(1)).cloned())
                .or_else(|| direct_urls.get(index.saturating_sub(1)).cloned())
                .unwrap_or_else(|| mediamtx_url_for(index))
        },
        output_ids,
    )
    .await
}

pub(super) async fn spawn_ffmpeg_signal_sink(
    env: &MixedEnv,
    cfg: &str,
    case: &MixedOutputCase,
    index: usize,
    offset: usize,
) -> Result<FfmpegSignalSink, String> {
    let port = env
        .ffmpeg_signal_sink_base
        .checked_add(offset as u16)
        .ok_or("FFmpeg signal sink port range overflowed")?;
    let duration = if env.explicit_check_selected("soak-drift") {
        env.av_soak_seconds
    } else {
        env.av_signal_seconds
    };
    let stem = safe_artifact_stem(&format!("{cfg}-{}-out{index}", case.id()));
    let capture_path = env.work_dir.join(format!("{stem}.signal.mkv"));
    let publish_url = match case.protocol() {
        MixedOutputProtocol::Rtmp => format!("rtmp://127.0.0.1:{port}/live/{stem}"),
        MixedOutputProtocol::Srt => {
            format!("srt://127.0.0.1:{port}?pkt_size=1316&latency=200000")
        }
    };
    let listen_url = match case.protocol() {
        MixedOutputProtocol::Rtmp => publish_url.clone(),
        MixedOutputProtocol::Srt => {
            format!(
                "srt://127.0.0.1:{port}?mode=listener&transtype=live&timeout=30000000&latency=200000"
            )
        }
    };
    let mut command = Command::new("ffmpeg");
    command.args(["-y", "-nostdin", "-hide_banner", "-v", "warning"]);
    if case.protocol() == MixedOutputProtocol::Rtmp {
        command.args(["-listen", "1"]);
    }
    let duration_s = duration.to_string();
    command
        .arg("-i")
        .arg(&listen_url)
        .args([
            "-t",
            &duration_s,
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-f",
            "matroska",
        ])
        .arg(&capture_path);
    let log_path = env.work_dir.join(format!("{stem}.signal-sink.log"));
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let child = command
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| {
            format!(
                "failed to start FFmpeg signal sink {}[{index}]: {e}",
                case.id()
            )
        })?;

    Ok(FfmpegSignalSink {
        cfg: cfg.to_string(),
        group: case.id().to_string(),
        index,
        publish_url,
        capture_path,
        child,
    })
}

pub(super) async fn finish_ffmpeg_signal_sinks(
    env: &MixedEnv,
    sinks: &mut [FfmpegSignalSink],
    resume: &mut MixedResume,
) -> Result<(), String> {
    for sink in sinks {
        let label = format!("{} out{}", sink.group, sink.index);
        let id = mixed_output_check_id(&sink.cfg, &sink.group, "signal");
        if !resume.allows(&id) {
            continue;
        }
        let duration = if env.explicit_check_selected("soak-drift") {
            env.av_soak_seconds
        } else {
            env.av_signal_seconds
        };
        let started = Instant::now();
        let wait =
            tokio::time::timeout(Duration::from_secs(duration + 90), sink.child.wait()).await;
        let status = match wait {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => return Err(format!("{label}: signal sink wait failed: {error}")),
            Err(_) => {
                let _ = sink.child.kill().await;
                return Err(format!("{label}: signal sink timed out"));
            }
        };
        if !status.success() {
            return Err(format!("{label}: signal sink exited with {status}"));
        }
        let result = validate_signal_capture_artifact(
            env,
            &sink.cfg,
            &id,
            &label,
            &sink.publish_url,
            &sink.capture_path,
            duration,
            started,
        )
        .await;
        result?;
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn validate_signal_capture_artifact(
    env: &MixedEnv,
    cfg: &str,
    id: &str,
    label: &str,
    url: &str,
    capture_path: &Path,
    duration: u64,
    started: Instant,
) -> Result<(), String> {
    let stem = safe_artifact_stem(&format!("{cfg}-{label}"));
    let blackdetect_log = env.work_dir.join(format!("{stem}.blackdetect.log"));
    let silencedetect_log = env.work_dir.join(format!("{stem}.silencedetect.log"));
    let ashowinfo_log = env.work_dir.join(format!("{stem}.ashowinfo.log"));
    let astats_log = env.work_dir.join(format!("{stem}.astats.log"));
    let result = async {
        let black = run_ffmpeg_filter_log(
            capture_path,
            duration,
            &[
                "-vf",
                "blackdetect=d=0.05:pix_th=0.10",
                "-an",
                "-f",
                "null",
                "-",
            ],
            &blackdetect_log,
        )
        .await?;
        let silence = run_ffmpeg_filter_log(
            capture_path,
            duration,
            &[
                "-af",
                "silencedetect=n=-35dB:d=0.05",
                "-vn",
                "-f",
                "null",
                "-",
            ],
            &silencedetect_log,
        )
        .await?;
        let ashow = run_ffmpeg_filter_log(
            capture_path,
            duration,
            &["-af", "ashowinfo", "-vn", "-f", "null", "-"],
            &ashowinfo_log,
        )
        .await?;
        let astats = run_ffmpeg_filter_log(
            capture_path,
            duration,
            &["-af", "astats=metadata=1:reset=1", "-vn", "-f", "null", "-"],
            &astats_log,
        )
        .await?;
        let pcm = decode_pcm_quality(capture_path, duration).await?;
        validate_signal_quality(&black, &silence, &ashow, &astats, pcm)
    }
    .await;

    match result {
        Ok(report) => {
            emit_mixed_result(
                env,
                cfg,
                id,
                "pass",
                started.elapsed(),
                Some(signal_report_json(
                    label,
                    url,
                    duration,
                    capture_path,
                    &blackdetect_log,
                    &silencedetect_log,
                    &ashowinfo_log,
                    &astats_log,
                    &report,
                )),
            )?;
            log_mixed_ok(
                env,
                &format!(
                    "{label}: signal ok offset={:.1}ms drift={:.1}ms audio_gap={:.1}ms",
                    report.max_abs_offset_ms, report.drift_ms, report.max_audio_pts_gap_ms
                ),
            )?;
            Ok(())
        }
        Err(error) => {
            emit_mixed_result(
                env,
                cfg,
                id,
                "fail",
                started.elapsed(),
                Some(json!({
                    "label": label,
                    "url": url,
                    "durationSecs": duration,
                    "error": error,
                    "capture": capture_path,
                    "logs": {
                        "blackdetect": blackdetect_log,
                        "silencedetect": silencedetect_log,
                        "ashowinfo": ashowinfo_log,
                        "astats": astats_log,
                    },
                })),
            )?;
            Err(format!("{label}: signal validation failed: {error}"))
        }
    }
}

pub(super) async fn spawn_ffmpeg_srt_sink(
    env: &MixedEnv,
    cfg: &str,
    group: &str,
    index: usize,
    validation: &MixedSrtGroupValidation<'_>,
    offset: usize,
) -> Result<FfmpegSrtSink, String> {
    let port = env
        .ffmpeg_srt_sink_base
        .checked_add(offset as u16)
        .ok_or("FFmpeg SRT sink port range overflowed")?;
    let safe_label = validation
        .label
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect::<String>();
    let stem = format!("{cfg}-{group}-{index}-{safe_label}");
    let log_path = env.work_dir.join(format!("{stem}.ffmpeg-srt-sink.log"));
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let err = log.try_clone().map_err(|e| e.to_string())?;
    let listener_url =
        format!("srt://127.0.0.1:{port}?mode=listener&transtype=live&timeout=30000000");
    let probe_interval = format!("%+{}", env.ffmpeg_srt_sink_seconds);
    let child = Command::new("ffprobe")
        .args([
            "-v",
            "warning",
            "-probesize",
            "10000000",
            "-analyzeduration",
            "10000000",
            "-read_intervals",
            &probe_interval,
            "-show_entries",
            "program=:stream=index,codec_type,width,height:packet=stream_index,dts_time,pts_time",
            "-of",
            "compact=p=1:nk=0",
            &listener_url,
        ])
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| format!("failed to start FFmpeg SRT sink {group}[{index}]: {e}"))?;

    Ok(FfmpegSrtSink {
        group: group.to_string(),
        index,
        port,
        log_path,
        expected_dimensions: validation.expected_dimensions.to_string(),
        expected_audio_tracks: validation.expected_audio_tracks,
        child,
    })
}

pub(super) async fn finish_ffmpeg_srt_sinks(sinks: &mut [FfmpegSrtSink]) -> Result<(), String> {
    for sink in sinks {
        let wait = tokio::time::timeout(Duration::from_secs(30), sink.child.wait()).await;
        let status = match wait {
            Ok(Ok(status)) => status,
            Ok(Err(error)) => {
                return Err(format!(
                    "FFmpeg SRT sink {}[{}] wait failed: {error}",
                    sink.group, sink.index
                ));
            }
            Err(_) => {
                let _ = sink.child.kill().await;
                return Err(format!(
                    "FFmpeg SRT sink {}[{}] timed out waiting for probe",
                    sink.group, sink.index
                ));
            }
        };
        let stderr = std::fs::read_to_string(&sink.log_path).unwrap_or_default();
        if !status.success() {
            return Err(format!(
                "FFmpeg SRT sink {}[{}] probe failed status={status}: {}",
                sink.group,
                sink.index,
                stderr.lines().take(5).collect::<Vec<_>>().join(" | ")
            ));
        }
        let stderr_lower = stderr.to_ascii_lowercase();
        let bad_patterns = [
            "non-monoton",
            "non monoton",
            "invalid data",
            "error while decoding",
            "timestamp discontinuity",
            "queue input is backward in time",
            "too many packets buffered",
            "aac bitstream error",
            "missing picture",
        ];
        if let Some(pattern) = bad_patterns
            .iter()
            .find(|pattern| stderr_lower.contains(**pattern))
        {
            return Err(format!(
                "FFmpeg SRT sink {}[{}] probe log matched {pattern:?}: {}",
                sink.group,
                sink.index,
                stderr.lines().take(8).collect::<Vec<_>>().join(" | ")
            ));
        }
        let dimensions = ffprobe_compact_video_dimensions(&stderr).unwrap_or_default();
        let audio_tracks = ffprobe_compact_audio_track_count(&stderr);
        if dimensions != sink.expected_dimensions || audio_tracks != sink.expected_audio_tracks {
            return Err(format!(
                "FFmpeg SRT sink {}[{}] expected {} with {} audio tracks, got {} with {} audio tracks",
                sink.group,
                sink.index,
                sink.expected_dimensions,
                sink.expected_audio_tracks,
                if dimensions.is_empty() {
                    "<no video>"
                } else {
                    &dimensions
                },
                audio_tracks
            ));
        }
        let packet_count = ffprobe_compact_validate_dts(&stderr).map_err(|error| {
            format!(
                "FFmpeg SRT sink {}[{}] packet DTS validation failed: {error}",
                sink.group, sink.index
            )
        })?;
        if packet_count == 0 {
            return Err(format!(
                "FFmpeg SRT sink {}[{}] probe did not return any packets",
                sink.group, sink.index
            ));
        }
    }
    Ok(())
}

pub(super) fn ffprobe_compact_video_dimensions(log: &str) -> Option<String> {
    ffprobe_compact_stream_lines(log).find_map(|line| {
        let codec_type = ffprobe_compact_field(line, "codec_type")?;
        if codec_type != "video" {
            return None;
        }
        let width = ffprobe_compact_field(line, "width")?;
        let height = ffprobe_compact_field(line, "height")?;
        Some(format!("{width}x{height}"))
    })
}

pub(super) fn ffprobe_compact_audio_track_count(log: &str) -> usize {
    ffprobe_compact_stream_lines(log)
        .filter(|line| ffprobe_compact_field(line, "codec_type") == Some("audio"))
        .filter_map(|line| ffprobe_compact_field(line, "index"))
        .collect::<HashSet<_>>()
        .len()
}

pub(super) fn ffprobe_compact_validate_dts(log: &str) -> Result<usize, String> {
    let mut by_stream = HashMap::<usize, Vec<f64>>::new();
    let mut packet_count = 0usize;
    for line in log.lines().filter(|line| line.starts_with("packet|")) {
        let Some(stream_index) =
            ffprobe_compact_field(line, "stream_index").and_then(|value| value.parse().ok())
        else {
            continue;
        };
        let Some(dts) = ffprobe_compact_field(line, "dts_time").and_then(|value| {
            if value == "N/A" {
                None
            } else {
                value.parse().ok()
            }
        }) else {
            continue;
        };
        by_stream.entry(stream_index).or_default().push(dts);
        packet_count += 1;
    }
    for (stream_index, dts_values) in &mut by_stream {
        dts_values.sort_by(|left, right| left.total_cmp(right));
        for pair in dts_values.windows(2) {
            let previous = pair[0];
            let current = pair[1];
            let delta = current - previous;
            if delta <= f64::EPSILON {
                return Err(format!(
                    "stream {stream_index} has duplicate DTS: {previous:.6} >= {current:.6}"
                ));
            }
            if delta > 0.500 {
                return Err(format!(
                    "stream {stream_index} has DTS gap {delta:.6}s between {previous:.6} and {current:.6}"
                ));
            }
        }
    }
    Ok(packet_count)
}

pub(super) fn ffprobe_compact_stream_lines(log: &str) -> impl Iterator<Item = &str> {
    log.lines().filter(|line| line.starts_with("stream|"))
}

pub(super) fn ffprobe_compact_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split('|').find_map(|field| {
        let (field_key, value) = field.split_once('=')?;
        (field_key == key).then_some(value)
    })
}

pub(super) async fn snapshot_mixed(
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

/// Inputs for one ffprobe-based mixed output assertion.
pub(super) struct MixedProbeSpec<'a> {
    pub(super) cfg: &'a str,
    pub(super) id: String,
    pub(super) label: &'a str,
    pub(super) url: &'a str,
    pub(super) expected: &'a str,
    pub(super) cookie: Option<&'a str>,
}

pub(super) async fn verify_mixed_stream(
    env: &MixedEnv,
    spec: MixedProbeSpec<'_>,
    resume: &mut MixedResume,
) -> Result<(), String> {
    if !resume.allows(&spec.id) {
        return Ok(());
    }
    let started = Instant::now();
    let mut last = String::new();
    let mut last_error = String::new();
    for _attempt in 1..=30 {
        match probe_dims_ramp_with_cookie(spec.url, spec.cookie).await {
            Ok(dimensions) if dimensions == spec.expected => {
                emit_mixed_result(
                    env,
                    spec.cfg,
                    &spec.id,
                    "pass",
                    started.elapsed(),
                    Some(json!({
                        "label": spec.label,
                        "expected": spec.expected,
                        "got": dimensions,
                        "url": spec.url,
                    })),
                )?;
                log_mixed_ok(env, &format!("ffprobe: {} -> {dimensions}", spec.label))?;
                return Ok(());
            }
            Ok(dimensions) => {
                if !dimensions.is_empty() {
                    last = dimensions;
                }
            }
            Err(error) => {
                last_error = error.clone();
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    let message = format!(
        "ffprobe: {} - expected {}, got '{}'",
        spec.label,
        spec.expected,
        if last.is_empty() {
            "<no output>"
        } else {
            &last
        }
    );
    emit_mixed_result(
        env,
        spec.cfg,
        &spec.id,
        "fail",
        started.elapsed(),
        Some(json!({
            "message": message,
            "label": spec.label,
            "expected": spec.expected,
            "got": last,
            "url": spec.url,
            "ffprobe_stderr": last_error,
        })),
    )?;
    Err(message)
}

pub(super) async fn verify_mixed_hls_preview(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    expected_dimensions: &str,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<Value, String> {
    let id = mixed_scenario_check_id(cfg, "hls_preview");
    if !resume.allows(&id) {
        return Ok(json!({"skipped": true}));
    }
    let started = Instant::now();
    let (_status, playlist_body) =
        wait_for_hls_playlist_ready(api, pipeline_id, Duration::from_secs(30)).await?;
    let expected_audio_tracks = case.expected_audio_tracks();
    let audio_renditions = playlist_body.matches("#EXT-X-MEDIA:TYPE=AUDIO").count();
    if audio_renditions != expected_audio_tracks {
        let message = format!(
            "hls-preview {cfg}: expected {expected_audio_tracks} audio renditions, got {audio_renditions}"
        );
        emit_mixed_result(
            env,
            cfg,
            &id,
            "fail",
            started.elapsed(),
            Some(json!({
                "message": message,
                "expectedAudioRenditions": expected_audio_tracks,
                "audioRenditions": audio_renditions,
                "playlist": playlist_body,
            })),
        )?;
        return Err(message);
    }
    let preview =
        wait_for_api_hls_preview_state(api, pipeline_id, true, Duration::from_secs(10)).await?;
    let playlist_url = format!(
        "http://127.0.0.1:{}/hls/{pipeline_id}/master.m3u8",
        env.restream_http
    );
    match probe_dims_ramp_with_cookie(&playlist_url, api.cookie.as_deref()).await {
        Ok(dimensions) if dimensions == expected_dimensions => {
            let summary = json!({
                "inputCase": case.scenario_id(),
                "codec": case.codec_name(),
                "trackLayout": case.track_layout_name(),
                "playlistReady": playlist_body.contains("#EXTM3U"),
                "expectedAudioRenditions": expected_audio_tracks,
                "audioRenditions": audio_renditions,
                "preview": preview,
                "expected": expected_dimensions,
                "got": dimensions,
                "url": playlist_url,
            });
            emit_mixed_result(
                env,
                cfg,
                &id,
                "pass",
                started.elapsed(),
                Some(summary.clone()),
            )?;
            emit_mixed_timing(
                env,
                cfg,
                "check.hls_preview",
                "pass",
                started.elapsed(),
                Some(json!({
                    "expected": expected_dimensions,
                    "got": dimensions,
                })),
            )?;
            log_mixed_ok(env, &format!("hls-preview: {cfg} -> {dimensions}"))?;
            Ok(summary)
        }
        Ok(dimensions) => {
            let message =
                format!("hls-preview {cfg}: expected {expected_dimensions}, got {dimensions}");
            emit_mixed_result(
                env,
                cfg,
                &id,
                "fail",
                started.elapsed(),
                Some(json!({
                    "message": message,
                    "expected": expected_dimensions,
                    "got": dimensions,
                    "url": playlist_url,
                })),
            )?;
            emit_mixed_timing(
                env,
                cfg,
                "check.hls_preview",
                "fail",
                started.elapsed(),
                Some(json!({
                    "expected": expected_dimensions,
                    "got": dimensions,
                })),
            )?;
            Err(message)
        }
        Err(error) => {
            let message = format!("hls-preview {cfg}: ffprobe failed: {error}");
            emit_mixed_result(
                env,
                cfg,
                &id,
                "fail",
                started.elapsed(),
                Some(json!({
                    "message": message,
                    "error": error,
                    "url": playlist_url,
                })),
            )?;
            emit_mixed_timing(
                env,
                cfg,
                "check.hls_preview",
                "fail",
                started.elapsed(),
                Some(json!({"error": error})),
            )?;
            Err(message)
        }
    }
}

pub(super) async fn verify_optional_mixed_hls_preview(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<Option<Value>, String> {
    if env.check_selected("hls") {
        verify_mixed_hls_preview(
            env,
            api,
            cfg,
            pipeline_id,
            case.hls_preview_expected_dimensions(),
            case,
            resume,
        )
        .await
        .map(Some)
    } else {
        Ok(None)
    }
}

pub(super) async fn verify_mixed_recording(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    case: MixedInputCase,
    resume: &mut MixedResume,
) -> Result<Value, String> {
    if !env.check_selected("recording") {
        return Ok(json!({"skipped": true}));
    }
    let id = mixed_scenario_check_id(cfg, "recording");
    if !resume.allows(&id) {
        return Ok(json!({"skipped": true}));
    }

    let started = Instant::now();
    let before_files = media_dir_entries(&env.media_dir)?;
    api.post_json(
        &format!("/api/v1/pipelines/{pipeline_id}/recording/start"),
        json!({}),
    )
    .await?;
    wait_for_api_recording_state(api, pipeline_id, true, Duration::from_secs(10)).await?;
    tokio::time::sleep(Duration::from_secs(6)).await;
    api.post_json(
        &format!("/api/v1/pipelines/{pipeline_id}/recording/stop"),
        json!({}),
    )
    .await?;
    wait_for_api_recording_state(api, pipeline_id, false, Duration::from_secs(20)).await?;

    let recording_entry =
        wait_for_api_media_file(api, &before_files, ".mp4", Duration::from_secs(30)).await?;
    let recording_name = recording_entry["playName"]
        .as_str()
        .or_else(|| recording_entry["name"].as_str())
        .ok_or("recording entry missing playName/name")?;
    let recording_path = env.media_dir.join(recording_name);
    if !recording_path.exists() {
        return Err(format!(
            "recording listed by API but missing on disk: {}",
            recording_path.display()
        ));
    }

    let probe = ffprobe(recording_path.to_string_lossy().as_ref()).await?;
    let streams = normalized_streams(&probe)?;
    let stream_array = streams
        .as_array()
        .ok_or("recording normalized stream list missing array")?;
    let video_codec = stream_array
        .iter()
        .find(|stream| stream["type"] == "video")
        .and_then(|stream| stream["codec"].as_str())
        .unwrap_or_default();
    let audio_tracks = stream_array
        .iter()
        .filter(|stream| stream["type"] == "audio")
        .count();
    let expected_video_codec = case.expected_video_codec();
    let expected_audio_tracks = case.expected_audio_tracks();
    let passed = video_codec == expected_video_codec && audio_tracks == expected_audio_tracks;
    let summary = json!({
        "inputCase": case.scenario_id(),
        "recordingFile": recording_path,
        "expectedVideoCodec": expected_video_codec,
        "videoCodec": video_codec,
        "expectedAudioTracks": expected_audio_tracks,
        "audioTracks": audio_tracks,
        "entry": recording_entry,
        "normalizedStreams": streams,
        "probe": probe,
    });
    emit_mixed_result(
        env,
        cfg,
        &id,
        if passed { "pass" } else { "fail" },
        started.elapsed(),
        Some(summary.clone()),
    )?;
    emit_mixed_timing(
        env,
        cfg,
        "check.recording",
        if passed { "pass" } else { "fail" },
        started.elapsed(),
        Some(json!({
            "expectedVideoCodec": expected_video_codec,
            "videoCodec": video_codec,
            "expectedAudioTracks": expected_audio_tracks,
            "audioTracks": audio_tracks,
        })),
    )?;
    if passed {
        log_mixed_ok(
            env,
            &format!("recording: {cfg} -> {video_codec}, audio_tracks={audio_tracks}"),
        )?;
        Ok(summary)
    } else {
        Err(format!(
            "recording {cfg}: expected {expected_video_codec} with {expected_audio_tracks} audio tracks, got {video_codec} with {audio_tracks}"
        ))
    }
}

pub(super) async fn warm_mixed_stream(
    label: &str,
    url: &str,
    expected: &str,
    cookie: Option<&str>,
) {
    for _attempt in 1..=30 {
        match probe_dims_ramp_with_cookie(url, cookie).await {
            Ok(dimensions) if dimensions == expected => {
                println!("  warmup: {label} -> {dimensions}");
                return;
            }
            Ok(_) | Err(_) => {}
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }
    eprintln!(
        "    warmup: {label} did not reach {expected}; lifecycle will report if stop state is unhealthy"
    );
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn verify_mixed_audio_route(
    env: &MixedEnv,
    cfg: &str,
    id: &str,
    label: &str,
    url: &str,
    expected_dimensions: &str,
    expected_audio_tracks: usize,
    resume: &mut MixedResume,
) -> Result<(), String> {
    if !resume.allows(id) {
        return Ok(());
    }
    let started = Instant::now();
    let mut last_dimensions = String::new();
    let mut last_audio_tracks = None;
    let mut last_error = String::new();
    for _attempt in 1..=15 {
        match ffprobe(url).await {
            Ok(probe) => {
                let dimensions = video_dimensions(&probe).unwrap_or_default();
                let audio_tracks = probe_audio_track_count(&probe);
                if dimensions == expected_dimensions && audio_tracks == expected_audio_tracks {
                    emit_mixed_result(
                        env,
                        cfg,
                        id,
                        "pass",
                        started.elapsed(),
                        Some(json!({
                            "label": label,
                            "expected": expected_dimensions,
                            "got": dimensions,
                            "expected_audio_tracks": expected_audio_tracks,
                            "audio_tracks": audio_tracks,
                            "url": url,
                        })),
                    )?;
                    log_mixed_ok(
                        env,
                        &format!("{label}: {dimensions}, audio_tracks={audio_tracks}"),
                    )?;
                    return Ok(());
                }
                if !dimensions.is_empty() {
                    last_dimensions = dimensions;
                }
                last_audio_tracks = Some(audio_tracks);
            }
            Err(error) => {
                last_error = error.clone();
            }
        }
        tokio::time::sleep(Duration::from_secs(2)).await;
    }

    let message = format!(
        "{label}: expected {expected_dimensions} with {expected_audio_tracks} audio tracks, got '{}' with {} audio tracks",
        if last_dimensions.is_empty() {
            "<no video>"
        } else {
            &last_dimensions
        },
        last_audio_tracks
            .map(|count| count.to_string())
            .unwrap_or_else(|| "<unknown>".to_string())
    );
    emit_mixed_result(
        env,
        cfg,
        id,
        "fail",
        started.elapsed(),
        Some(json!({
            "message": message,
            "label": label,
            "expected": expected_dimensions,
            "got": last_dimensions,
            "expected_audio_tracks": expected_audio_tracks,
            "audio_tracks": last_audio_tracks,
            "url": url,
            "ffprobe_stderr": last_error,
        })),
    )?;
    Err(message)
}

pub(super) async fn verify_mixed_decode_scan(
    env: &MixedEnv,
    cfg: &str,
    id: &str,
    label: &str,
    url: &str,
    resume: &mut MixedResume,
) -> Result<(), String> {
    if !resume.allows(id) {
        return Ok(());
    }

    let started = Instant::now();
    let (passed, status, matched_pattern, stderr) = ffmpeg_decode_scan(label, url).await?;
    let mut fallback_validation = None;
    let tolerated_warning = if decode_scan_needs_video_dts_fallback(url, status, matched_pattern) {
        let packets_path = env.work_dir.join(format!(
            "{}.decode-scan.ffprobe.json",
            safe_artifact_stem(&format!("{cfg}-{label}"))
        ));
        match ffprobe_video_packets(url, &packets_path).await {
            Ok(packet_probe) => {
                let packet_count = count_video_packets(&packet_probe);
                let dts_monotone = video_dts_monotone(&packet_probe);
                fallback_validation = Some(json!({
                    "packetsPath": packets_path,
                    "packetCount": packet_count,
                    "videoDtsMonotone": dts_monotone,
                }));
                packet_count > 0 && dts_monotone
            }
            Err(error) => {
                fallback_validation = Some(json!({
                    "packetsPath": packets_path,
                    "error": error,
                }));
                false
            }
        }
    } else {
        false
    };
    emit_mixed_result(
        env,
        cfg,
        id,
        if passed || tolerated_warning {
            "pass"
        } else {
            "fail"
        },
        started.elapsed(),
        Some(json!({
            "label": label,
            "url": url,
            "status": status,
            "matchedPattern": matched_pattern,
            "toleratedWarning": tolerated_warning,
            "stderr": stderr.lines().take(20).collect::<Vec<_>>(),
            "videoDtsFallback": fallback_validation,
        })),
    )?;

    if passed || tolerated_warning {
        if tolerated_warning {
            log_mixed_ok(
                env,
                &format!(
                    "{label}: decode scan tolerated muxer DTS warning after packet DTS validation"
                ),
            )?;
        } else {
            log_mixed_ok(env, &format!("{label}: decode scan clean"))?;
        }
        Ok(())
    } else {
        Err(format!(
            "{label}: decode scan failed status={status:?} matched={matched_pattern:?}: {}",
            stderr.lines().take(5).collect::<Vec<_>>().join(" | ")
        ))
    }
}

pub(super) fn decode_scan_needs_video_dts_fallback(
    url: &str,
    status: Option<i32>,
    matched_pattern: Option<&'static str>,
) -> bool {
    (url.starts_with("rtmp://") || url.starts_with("srt://"))
        && status == Some(0)
        && matches!(matched_pattern, Some("non monoton" | "non-monoton"))
}

pub(super) async fn ffmpeg_decode_scan(
    label: &str,
    url: &str,
) -> Result<(bool, Option<i32>, Option<&'static str>, String), String> {
    let mut command = Command::new("ffmpeg");
    command.args([
        "-nostdin",
        "-hide_banner",
        "-v",
        "warning",
        "-i",
        url,
        "-t",
        "5",
        "-map",
        "0",
        "-f",
        "null",
        "-",
    ]);
    let child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let output = tokio::time::timeout(Duration::from_secs(45), child.wait_with_output())
        .await
        .map_err(|_| format!("decode scan timed out: {label}: {url}"))?
        .map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    let stderr_lower = stderr.to_ascii_lowercase();
    let bad_patterns = [
        "non-monoton",
        "non monoton",
        "invalid data",
        "error while decoding",
        "timestamp discontinuity",
        "queue input is backward in time",
        "too many packets buffered",
        "aac bitstream error",
        "missing picture",
    ];
    let matched_pattern = bad_patterns
        .iter()
        .find(|pattern| stderr_lower.contains(**pattern))
        .copied();
    let passed = output.status.success() && matched_pattern.is_none();
    Ok((passed, output.status.code(), matched_pattern, stderr))
}

/// Parsed marker positions and quality flags from AV signal captures.
#[derive(Debug, Clone)]
pub(super) struct MarkerQualityReport {
    pub(super) video_markers: Vec<f64>,
    pub(super) audio_markers: Vec<f64>,
    pub(super) offsets_ms: Vec<f64>,
    pub(super) max_abs_offset_ms: f64,
    pub(super) drift_ms: f64,
    pub(super) max_audio_pts_gap_ms: f64,
    pub(super) pcm: PcmQualityReport,
}

/// PCM-level audio quality statistics for signal-control assertions.
#[derive(Debug, Clone, Copy)]
pub(super) struct PcmQualityReport {
    pub(super) samples: usize,
    pub(super) clipping_samples: usize,
    pub(super) max_step: i32,
    pub(super) rms: f64,
}

#[allow(clippy::too_many_arguments)]
pub(super) fn signal_report_json(
    label: &str,
    url: &str,
    duration: u64,
    capture_path: &Path,
    blackdetect_log: &Path,
    silencedetect_log: &Path,
    ashowinfo_log: &Path,
    astats_log: &Path,
    report: &MarkerQualityReport,
) -> Value {
    json!({
        "label": label,
        "url": url,
        "durationSecs": duration,
        "capture": capture_path,
        "logs": {
            "blackdetect": blackdetect_log,
            "silencedetect": silencedetect_log,
            "ashowinfo": ashowinfo_log,
            "astats": astats_log,
        },
        "videoMarkers": report.video_markers,
        "audioMarkers": report.audio_markers,
        "offsetsMs": report.offsets_ms,
        "maxAbsOffsetMs": report.max_abs_offset_ms,
        "driftMs": report.drift_ms,
        "maxAudioPtsGapMs": report.max_audio_pts_gap_ms,
        "pcm": {
            "samples": report.pcm.samples,
            "clippingSamples": report.pcm.clipping_samples,
            "maxStep": report.pcm.max_step,
            "rms": report.pcm.rms,
        },
    })
}

pub(super) async fn verify_mixed_signal_quality(
    env: &MixedEnv,
    cfg: &str,
    id: &str,
    label: &str,
    url: &str,
    resume: &mut MixedResume,
) -> Result<(), String> {
    if !resume.allows(id) {
        return Ok(());
    }

    let started = Instant::now();
    let duration = if env.explicit_check_selected("soak-drift") {
        env.av_soak_seconds
    } else {
        env.av_signal_seconds
    };
    let stem = safe_artifact_stem(&format!("{cfg}-{label}"));
    let capture_path = env.work_dir.join(format!("{stem}.signal.mkv"));
    let blackdetect_log = env.work_dir.join(format!("{stem}.blackdetect.log"));
    let silencedetect_log = env.work_dir.join(format!("{stem}.silencedetect.log"));
    let ashowinfo_log = env.work_dir.join(format!("{stem}.ashowinfo.log"));
    let astats_log = env.work_dir.join(format!("{stem}.astats.log"));

    let result = async {
        capture_signal_sample(url, &capture_path, duration).await?;
        let black = run_ffmpeg_filter_log(
            &capture_path,
            duration,
            &[
                "-vf",
                "blackdetect=d=0.05:pix_th=0.10",
                "-an",
                "-f",
                "null",
                "-",
            ],
            &blackdetect_log,
        )
        .await?;
        let silence = run_ffmpeg_filter_log(
            &capture_path,
            duration,
            &[
                "-af",
                "silencedetect=n=-35dB:d=0.05",
                "-vn",
                "-f",
                "null",
                "-",
            ],
            &silencedetect_log,
        )
        .await?;
        let ashow = run_ffmpeg_filter_log(
            &capture_path,
            duration,
            &["-af", "ashowinfo", "-vn", "-f", "null", "-"],
            &ashowinfo_log,
        )
        .await?;
        let astats = run_ffmpeg_filter_log(
            &capture_path,
            duration,
            &["-af", "astats=metadata=1:reset=1", "-vn", "-f", "null", "-"],
            &astats_log,
        )
        .await?;
        let pcm = decode_pcm_quality(&capture_path, duration).await?;
        validate_signal_quality(&black, &silence, &ashow, &astats, pcm)
    }
    .await;

    match result {
        Ok(report) => {
            emit_mixed_result(
                env,
                cfg,
                id,
                "pass",
                started.elapsed(),
                Some(signal_report_json(
                    label,
                    url,
                    duration,
                    &capture_path,
                    &blackdetect_log,
                    &silencedetect_log,
                    &ashowinfo_log,
                    &astats_log,
                    &report,
                )),
            )?;
            log_mixed_ok(
                env,
                &format!(
                    "{label}: signal ok offset={:.1}ms drift={:.1}ms audio_gap={:.1}ms",
                    report.max_abs_offset_ms, report.drift_ms, report.max_audio_pts_gap_ms
                ),
            )?;
            Ok(())
        }
        Err(error) => {
            emit_mixed_result(
                env,
                cfg,
                id,
                "fail",
                started.elapsed(),
                Some(json!({
                    "label": label,
                    "url": url,
                    "durationSecs": duration,
                    "error": error,
                    "capture": capture_path,
                    "logs": {
                        "blackdetect": blackdetect_log,
                        "silencedetect": silencedetect_log,
                        "ashowinfo": ashowinfo_log,
                        "astats": astats_log,
                    },
                })),
            )?;
            Err(format!("{label}: signal validation failed: {error}"))
        }
    }
}

pub(super) async fn capture_signal_sample(
    url: &str,
    output_path: &Path,
    duration: u64,
) -> Result<(), String> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut last_error = String::new();
    for _attempt in 1..=15 {
        match capture_signal_sample_once(url, output_path, duration).await {
            Ok(()) => return Ok(()),
            Err(error) => {
                last_error = error;
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        }
    }
    Err(last_error)
}

pub(super) async fn capture_signal_sample_once(
    url: &str,
    output_path: &Path,
    duration: u64,
) -> Result<(), String> {
    let duration_s = duration.to_string();
    let child = Command::new("ffmpeg")
        .args([
            "-y",
            "-nostdin",
            "-hide_banner",
            "-v",
            "warning",
            "-i",
            url,
            "-t",
            &duration_s,
            "-map",
            "0:v:0",
            "-map",
            "0:a:0",
            "-c",
            "copy",
            "-f",
            "matroska",
        ])
        .arg(output_path)
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let output = tokio::time::timeout(Duration::from_secs(duration + 45), child.wait_with_output())
        .await
        .map_err(|_| format!("signal capture timed out: {url}"))?
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "signal capture failed for {url}: {}",
            String::from_utf8_lossy(&output.stderr)
        ))
    }
}

pub(super) async fn run_ffmpeg_filter_log(
    input: &Path,
    duration: u64,
    filter_args: &[&str],
    log_path: &Path,
) -> Result<String, String> {
    let input_s = input.to_string_lossy().to_string();
    let duration_s = duration.to_string();
    let mut command = Command::new("ffmpeg");
    command.args([
        "-nostdin",
        "-hide_banner",
        "-v",
        "info",
        "-i",
        &input_s,
        "-t",
        &duration_s,
    ]);
    command.args(filter_args);
    let child = command
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let output = tokio::time::timeout(Duration::from_secs(duration + 45), child.wait_with_output())
        .await
        .map_err(|_| format!("ffmpeg signal filter timed out: {}", input.display()))?
        .map_err(|e| e.to_string())?;
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    std::fs::write(log_path, &stderr).map_err(|e| e.to_string())?;
    if output.status.success() {
        Ok(stderr)
    } else {
        Err(format!(
            "ffmpeg signal filter failed for {}: {}",
            input.display(),
            stderr.lines().take(8).collect::<Vec<_>>().join(" | ")
        ))
    }
}

pub(super) async fn decode_pcm_quality(
    input: &Path,
    duration: u64,
) -> Result<PcmQualityReport, String> {
    let input_s = input.to_string_lossy().to_string();
    let duration_s = duration.to_string();
    let child = Command::new("ffmpeg")
        .args([
            "-nostdin",
            "-hide_banner",
            "-v",
            "error",
            "-i",
            &input_s,
            "-t",
            &duration_s,
            "-vn",
            "-ac",
            "1",
            "-ar",
            "48000",
            "-f",
            "s16le",
            "pipe:1",
        ])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    let output = tokio::time::timeout(Duration::from_secs(duration + 45), child.wait_with_output())
        .await
        .map_err(|_| format!("PCM decode timed out: {}", input.display()))?
        .map_err(|e| e.to_string())?;
    if !output.status.success() {
        return Err(format!(
            "PCM decode failed for {}: {}",
            input.display(),
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    Ok(analyze_pcm_s16le(&output.stdout))
}

pub(super) fn validate_signal_quality(
    blackdetect_log: &str,
    silencedetect_log: &str,
    ashowinfo_log: &str,
    astats_log: &str,
    pcm: PcmQualityReport,
) -> Result<MarkerQualityReport, String> {
    assert_no_signal_bad_patterns(astats_log)?;
    let video_markers = marker_gaps_from_intervals(&parse_blackdetect_intervals(blackdetect_log));
    let audio_markers =
        marker_gaps_from_intervals(&parse_silencedetect_intervals(silencedetect_log));
    if video_markers.len() < 3 || audio_markers.len() < 3 {
        return Err(format!(
            "expected at least 3 video/audio markers, got video={} audio={}",
            video_markers.len(),
            audio_markers.len()
        ));
    }
    let offsets_ms = nearest_marker_offsets_ms(&video_markers, &audio_markers, 1000.0);
    if offsets_ms.len() < 3 {
        return Err(format!(
            "expected at least 3 paired A/V markers, got {} from video={} audio={}",
            offsets_ms.len(),
            video_markers.len(),
            audio_markers.len()
        ));
    }
    let max_abs_offset_ms = offsets_ms
        .iter()
        .map(|value| value.abs())
        .fold(0.0, f64::max);
    let drift_ms = match (offsets_ms.first(), offsets_ms.last()) {
        (Some(first), Some(last)) => (last - first).abs(),
        _ => 0.0,
    };
    let max_audio_pts_gap_ms = max_audio_pts_gap_ms(ashowinfo_log);

    if max_abs_offset_ms > 120.0 {
        return Err(format!(
            "A/V marker offset too high: {max_abs_offset_ms:.1}ms"
        ));
    }
    if drift_ms > 80.0 {
        return Err(format!("A/V marker drift too high: {drift_ms:.1}ms"));
    }
    if max_audio_pts_gap_ms > 80.0 {
        return Err(format!(
            "audio PTS gap too high: {max_audio_pts_gap_ms:.1}ms"
        ));
    }
    if pcm.clipping_samples > 0 {
        return Err(format!(
            "PCM clipping detected: {} samples",
            pcm.clipping_samples
        ));
    }
    if pcm.max_step > 30_000 {
        return Err(format!(
            "PCM impulse/click detected: max step {}",
            pcm.max_step
        ));
    }

    Ok(MarkerQualityReport {
        video_markers,
        audio_markers,
        offsets_ms,
        max_abs_offset_ms,
        drift_ms,
        max_audio_pts_gap_ms,
        pcm,
    })
}

pub(super) fn nearest_marker_offsets_ms(
    video_markers: &[f64],
    audio_markers: &[f64],
    max_abs_ms: f64,
) -> Vec<f64> {
    video_markers
        .iter()
        .filter_map(|video| {
            audio_markers
                .iter()
                .map(|audio| (audio - video) * 1000.0)
                .min_by(|left, right| left.abs().total_cmp(&right.abs()))
                .filter(|offset| offset.abs() <= max_abs_ms)
        })
        .collect()
}

pub(super) fn assert_no_signal_bad_patterns(log: &str) -> Result<(), String> {
    let lower = log.to_ascii_lowercase();
    let bad_patterns = [
        "non-monoton",
        "non monoton",
        "invalid data",
        "error while decoding",
        "timestamp discontinuity",
        "queue input is backward in time",
        "too many packets buffered",
        "aac bitstream error",
        "missing picture",
    ];
    if let Some(pattern) = bad_patterns
        .iter()
        .find(|pattern| lower.contains(**pattern))
    {
        return Err(format!("signal ffmpeg log matched {pattern:?}"));
    }
    Ok(())
}

pub(super) fn parse_blackdetect_intervals(log: &str) -> Vec<(f64, f64)> {
    parse_interval_pairs(log, "black_start:", "black_end:")
}

pub(super) fn parse_silencedetect_intervals(log: &str) -> Vec<(f64, f64)> {
    parse_interval_pairs(log, "silence_start:", "silence_end:")
}

pub(super) fn parse_interval_pairs(log: &str, start_key: &str, end_key: &str) -> Vec<(f64, f64)> {
    let mut intervals = Vec::new();
    let mut pending_start = None;
    for line in log.lines() {
        if let Some(start) = value_after_key(line, start_key) {
            pending_start = Some(start);
        }
        if let Some(end) = value_after_key(line, end_key)
            && let Some(start) = pending_start.take()
            && end > start
        {
            intervals.push((start, end));
        }
    }
    intervals.sort_by(|left, right| left.0.total_cmp(&right.0));
    intervals
}

pub(super) fn marker_gaps_from_intervals(intervals: &[(f64, f64)]) -> Vec<f64> {
    intervals
        .windows(2)
        .filter_map(|pair| {
            let previous_end = pair[0].1;
            let next_start = pair[1].0;
            let gap = next_start - previous_end;
            (0.050..=0.500)
                .contains(&gap)
                .then_some(previous_end + gap / 2.0)
        })
        .collect()
}

pub(super) fn value_after_key(line: &str, key: &str) -> Option<f64> {
    let start = line.find(key)? + key.len();
    let value = line[start..]
        .trim_start()
        .split(|ch: char| ch.is_whitespace() || ch == '|' || ch == ']')
        .next()?;
    value.parse().ok()
}

pub(super) fn max_audio_pts_gap_ms(ashowinfo_log: &str) -> f64 {
    let mut times: Vec<f64> = ashowinfo_log
        .lines()
        .filter_map(|line| value_after_key(line, "pts_time:"))
        .collect();
    times.sort_by(|left, right| left.total_cmp(right));
    let mut deltas: Vec<f64> = times
        .windows(2)
        .filter_map(|pair| {
            let delta = pair[1] - pair[0];
            (delta > 0.0).then_some(delta)
        })
        .collect();
    if deltas.is_empty() {
        return 0.0;
    }
    deltas.sort_by(|left, right| left.total_cmp(right));
    let median = deltas[deltas.len() / 2];
    deltas
        .into_iter()
        .map(|delta| (delta - median).max(0.0) * 1000.0)
        .fold(0.0, f64::max)
}

pub(super) fn analyze_pcm_s16le(bytes: &[u8]) -> PcmQualityReport {
    let mut samples = 0usize;
    let mut clipping_samples = 0usize;
    let mut max_step = 0i32;
    let mut sum_sq = 0f64;
    let mut previous: Option<i32> = None;
    for chunk in bytes.chunks_exact(2) {
        let sample = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
        samples += 1;
        if sample.abs() >= 32_760 {
            clipping_samples += 1;
        }
        if let Some(prev) = previous {
            max_step = max_step.max((sample - prev).abs());
        }
        previous = Some(sample);
        sum_sq += (sample as f64) * (sample as f64);
    }
    let rms = if samples == 0 {
        0.0
    } else {
        (sum_sq / samples as f64).sqrt()
    };
    PcmQualityReport {
        samples,
        clipping_samples,
        max_step,
        rms,
    }
}

pub(super) fn safe_artifact_stem(value: &str) -> String {
    value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch.to_ascii_lowercase()
            } else {
                '-'
            }
        })
        .collect()
}

pub(super) async fn stop_mixed_outputs(api: &RampApi, pipeline_id: &str, output_ids: &[String]) {
    for output_id in output_ids {
        let _ = api
            .post_json(
                &format!("/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"),
                Value::Null,
            )
            .await;
    }
}

pub(super) async fn wait_for_outputs_stopped(
    api: &RampApi,
    pipeline_id: &str,
    output_ids: &[String],
    timeout: Duration,
) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        let config = api.get_json("/api/v1/settings").await?;
        let all_stopped = output_ids.iter().all(|output_id| {
            config["jobs"]
                .as_array()
                .and_then(|jobs| {
                    jobs.iter().find(|job| {
                        job["pipelineId"] == pipeline_id && job["outputId"] == output_id.as_str()
                    })
                })
                .and_then(|job| job["status"].as_str())
                .is_none_or(|status| status == "stopped")
        });
        if all_stopped {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err("lifecycle: outputs did not all stop within 60 s".to_string());
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

pub(super) fn emit_mixed_result(
    env: &MixedEnv,
    cfg: &str,
    id: &str,
    status: &str,
    elapsed: Duration,
    extra: Option<Value>,
) -> Result<(), String> {
    let Some(path) = &env.assertion_log else {
        return Ok(());
    };
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let mut object = serde_json::Map::new();
    object.insert("id".to_string(), json!(id));
    object.insert("suite".to_string(), json!("mixed"));
    object.insert("mode".to_string(), json!(cfg));
    object.insert("scenario".to_string(), json!(cfg));
    object.insert("status".to_string(), json!(status));
    object.insert("ms".to_string(), json!(elapsed.as_millis()));
    if let Some(Value::Object(extra)) = extra {
        object.extend(extra);
    }
    append_line(path, &format!("{}\n", Value::Object(object))).map_err(|e| e.to_string())
}

pub(super) fn emit_mixed_timing(
    env: &MixedEnv,
    cfg: &str,
    stage: &str,
    status: &str,
    elapsed: Duration,
    extra: Option<Value>,
) -> Result<(), String> {
    let mut object = serde_json::Map::new();
    object.insert("scenario".to_string(), json!(cfg));
    object.insert("stage".to_string(), json!(stage));
    object.insert("status".to_string(), json!(status));
    object.insert("ms".to_string(), json!(elapsed.as_millis()));
    if let Some(Value::Object(extra)) = extra {
        object.extend(extra);
    }
    append_line(&env.timing_log, &format!("{}\n", Value::Object(object))).map_err(|e| e.to_string())
}

pub(super) fn log_mixed_ok(env: &MixedEnv, message: &str) -> Result<(), String> {
    append_line(&env.summary_log, &format!("ok: {message}\n"))
}

pub(super) fn effective_log_paths(path: &Path) -> Vec<PathBuf> {
    let Some(parent) = path.parent() else {
        return vec![path.to_path_buf()];
    };
    let logs_dir = parent.join("logs");
    let mut entries: Vec<PathBuf> = std::fs::read_dir(&logs_dir)
        .ok()
        .into_iter()
        .flatten()
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|candidate| {
            candidate
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("restream.log"))
        })
        .collect();
    entries.sort();
    if entries.is_empty() {
        vec![path.to_path_buf()]
    } else {
        entries
    }
}

pub(super) fn count_log_matches(path: &Path, needle: &str) -> usize {
    effective_log_paths(path)
        .into_iter()
        .filter_map(|candidate| std::fs::read_to_string(candidate).ok())
        .map(|content| content.matches(needle).count())
        .sum()
}

pub(super) fn file_tail_lines(path: &Path, lines: usize) -> Vec<String> {
    let Some(target) = effective_log_paths(path).into_iter().last() else {
        return Vec::new();
    };
    let Ok(content) = std::fs::read_to_string(target) else {
        return Vec::new();
    };
    let mut tail = content.lines().rev().take(lines).collect::<Vec<_>>();
    tail.reverse();
    tail.into_iter().map(str::to_string).collect()
}

pub(super) fn mixed_stage_count_from_graph(graph: &Value) -> MixedStageCount {
    MixedStageCount {
        video: graph_active_node_count(graph, "transcoder"),
        audio: graph_active_node_count(graph, "audio_filter"),
        codec_edge: graph_active_node_count(graph, "codec_edge"),
    }
}

pub(super) async fn verify_mixed_graph_stage_sharing(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    pipeline_id: &str,
    case: MixedInputCase,
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
    let expected = expected_mixed_stage_count(case);
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
            "outputMatrix": mixed_output_matrix_json(mixed_output_cases_for_input(case)),
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

    api.post_json(&format!("/api/v1/ingests/{ingest_id}/start"), json!({}))
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
        wait_for_outputs_progress(api, &pipeline_id, &output_ids, Duration::from_secs(60)).await?;
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
        api.post_json(
            &format!("/api/v1/pipelines/{pipeline_id}/outputs/{output_id}/stop"),
            json!({}),
        )
        .await?;
        if i % 4 == 3 {
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    api.post_json(&format!("/api/v1/ingests/{ingest_id}/stop"), json!({}))
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
