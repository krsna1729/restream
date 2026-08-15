use super::*;
use crate::mediamtx_probe::{
    MediaMtxPathHealth, mediamtx_path_health_json, merge_mediamtx_path_health,
    verify_mediamtx_path_health,
};

#[path = "msr/dashboard.rs"]
mod dashboard;
#[path = "msr/plan.rs"]
mod plan;
#[path = "msr/verification.rs"]
mod verification;

#[cfg(test)]
#[path = "msr/tests.rs"]
mod tests;

pub(crate) use dashboard::msr_dashboard;
use plan::*;
use verification::*;

pub(crate) const MSR_MODE: &str = "msr";
pub(crate) const MSR_DASHBOARD_MODE: &str = "msr.dashboard";

/// Peer-verification results for one MSR checkpoint. Exactly one of
/// `path_health`/`post_sample_path_health` (mediamtx peer) or
/// `sink_verification` (sink peer) is populated, depending on `MSR_PEER`;
/// both are `Option` so the JSON artifact only carries the fields that
/// apply, keeping the mediamtx-mode shape byte-identical to before this
/// mode split existed.
struct MsrCheckpointAggregate {
    resource: ResourceAggregate,
    path_health: Option<MediaMtxPathHealth>,
    post_sample_path_health: Option<MediaMtxPathHealth>,
    sink_verification: Option<MsrSinkVerification>,
    ffprobe_checks: Vec<Value>,
}

fn msr_checkpoint_aggregate_json(aggregate: &MsrCheckpointAggregate) -> Value {
    let mut value = resource_aggregate_json(&aggregate.resource);
    if let Some(object) = value.as_object_mut() {
        if let Some(path_health) = &aggregate.path_health {
            object.insert(
                "mediamtxPathHealth".to_string(),
                mediamtx_path_health_json(MSR_MODE, &aggregate.resource.label, path_health),
            );
        }
        if let Some(post_sample_path_health) = &aggregate.post_sample_path_health {
            object.insert(
                "mediamtxPostSamplePathHealth".to_string(),
                mediamtx_path_health_json(
                    MSR_MODE,
                    &format!("{}-post-sample", aggregate.resource.label),
                    post_sample_path_health,
                ),
            );
        }
        if let Some(sink_verification) = &aggregate.sink_verification {
            object.insert(
                "sinkVerification".to_string(),
                msr_sink_verification_json(sink_verification),
            );
        }
        object.insert(
            "ffprobeSamples".to_string(),
            Value::Array(aggregate.ffprobe_checks.clone()),
        );
    }
    value
}

/// (ready/present, expected, bytes delta) from whichever peer verification
/// this checkpoint used, for the report table.
fn checkpoint_ready_summary(aggregate: &MsrCheckpointAggregate) -> (usize, usize, u64) {
    if let Some(health) = &aggregate.path_health {
        (
            health.ready_paths,
            health.expected_paths,
            health.bytes_received_delta,
        )
    } else if let Some(sink) = &aggregate.sink_verification {
        (
            sink.outputs_present,
            sink.outputs_expected,
            sink.bytes_out_delta,
        )
    } else {
        (0, 0, 0)
    }
}

fn human_kib(kb: u64) -> String {
    if kb >= 1024 * 1024 {
        format!("{:.2} GB", kb as f64 / 1024.0 / 1024.0)
    } else if kb >= 1024 {
        format!("{:.0} MB", kb as f64 / 1024.0)
    } else {
        format!("{kb} KB")
    }
}

fn human_bytes(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{:.2} GB", bytes as f64 / 1024.0 / 1024.0 / 1024.0)
    } else if bytes >= 1024 * 1024 {
        format!("{:.1} MB", bytes as f64 / 1024.0 / 1024.0)
    } else if bytes >= 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{bytes} B")
    }
}

fn format_msr_report(
    executed_outputs: usize,
    audio_tracks: usize,
    rtmp_outputs: usize,
    enhanced_rtmp_outputs: usize,
    srt_outputs: usize,
    aggregates: &[MsrCheckpointAggregate],
) -> String {
    let sink_mode = aggregates
        .iter()
        .any(|aggregate| aggregate.sink_verification.is_some());
    let proof_phrase = if sink_mode {
        "loopback engine health API byte-growth proof"
    } else {
        "loopback MediaMTX path API byte-growth proof"
    };
    let mut report = format!(
        "Status: PASS at every checkpoint including {executed_outputs} outputs \
         (1 SRT ingest, {audio_tracks} audio tracks, Zipf fan-out, {rtmp_outputs} RTMP \
         with {enhanced_rtmp_outputs} Enhanced RTMP / {srt_outputs} SRT, \
         1080p30 H.264 passthrough, {proof_phrase}).\n\n"
    );
    report.push_str("| Outputs | Egress mix | MediaMTX ready | MediaMTX bytes delta | CPU avg % | CPU peak % | RSS peak | AVIO HWM peak | Samples |\n");
    report.push_str("|---:|---|---:|---:|---:|---:|---:|---:|---:|\n");
    for aggregate in aggregates {
        let resource = &aggregate.resource;
        let (ready, expected, bytes_delta) = checkpoint_ready_summary(aggregate);
        report.push_str(&format!(
            "| {} | {} | {}/{} | {} | {:.1} | {:.1} | {} | {} | {} |\n",
            resource.outputs,
            resource.egress_mix,
            ready,
            expected,
            human_bytes(bytes_delta),
            resource.total_cpu_avg_pct,
            resource.total_cpu_peak_pct,
            human_kib(resource.rss_peak_kb),
            human_kib(resource.avio_hwm_peak_kb),
            resource.sample_count,
        ));
    }
    let cores = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    let proof_detail = if sink_mode {
        "Peer proof is from the restream engine health API (`/api/v1/engine/health`): every expected output must be present and `bytesOut` must grow across the sample window before a checkpoint can pass."
    } else {
        "MediaMTX proof is from `/v3/paths/list`: every expected path must be ready and `bytesReceived` must grow across the sample window before a checkpoint can pass."
    };
    report.push_str(&format!(
        "\nCPU % is of a single core ({}% available on this host). {proof_detail}\n",
        cores * 100
    ));
    report
}

fn configure_msr_env(mut env: ResourceSweepEnv, profile: MsrRunProfile) -> ResourceSweepEnv {
    env.lifecycle = ResourceSweepLifecycle::Continuous;
    env.sample_secs = env_secs("MSR_SAMPLE_SECS", 6);
    env.sample_interval_ms = env_secs("MSR_SAMPLE_INTERVAL_MS", 1000);
    env.settle_secs = env_secs("MSR_SETTLE_SECS", 4);
    env.summary_json = env
        .work_dir
        .join(format!("{}-results.json", profile.output_prefix()));
    env.summary_csv = env
        .work_dir
        .join(format!("{}-results.csv", profile.output_prefix()));
    env.samples_jsonl = env
        .work_dir
        .join(format!("{}-samples.jsonl", profile.output_prefix()));
    env.restream_log = env
        .work_dir
        .join(format!("{}-restream.log", profile.output_prefix()));
    env.mediamtx_log = env
        .work_dir
        .join(format!("{}-mediamtx.log", profile.output_prefix()));
    env.mediamtx_config = env
        .work_dir
        .join(format!("{}-mediamtx.yml", profile.output_prefix()));
    if std::env::var_os("RESTREAM_DB_PATH").is_none() {
        env.restream_db_path = env.work_dir.join(format!("{}.db", profile.output_prefix()));
    }
    env
}

/// Verify expected mediamtx paths for a checkpoint, grouped by the peer
/// instance each output actually publishes to (`env.peer_count` peers), and
/// merge the per-instance results into one aggregate. With `peer_count == 1`
/// this issues exactly one `verify_mediamtx_path_health` call against
/// `env.mtx_api`, matching prior single-instance behavior byte-for-byte.
async fn verify_msr_grouped_path_health(
    env: &ResourceSweepEnv,
    outputs: &[MsrOutputSpec],
    sample_secs: u64,
    timeout: Duration,
) -> Result<MediaMtxPathHealth, String> {
    let groups = msr_group_expected_paths_by_instance(outputs, env.peer_count);
    let mut accumulated: Option<MediaMtxPathHealth> = None;
    for (instance, paths) in groups {
        let api_port = env.mtx_api + instance as u16;
        let health = verify_mediamtx_path_health(api_port, &paths, sample_secs, timeout).await?;
        accumulated = Some(match accumulated {
            None => health,
            Some(previous) => merge_mediamtx_path_health(previous, health),
        });
    }
    accumulated.ok_or_else(|| "no expected mediamtx paths for checkpoint".to_string())
}

struct MsrPhaseResult {
    env: ResourceSweepEnv,
    report_md: PathBuf,
    plan_json: Value,
    executed_outputs: usize,
    aggregates: Vec<MsrCheckpointAggregate>,
    signal_checks: Vec<Value>,
}

async fn run_msr_phase(
    env: ResourceSweepEnv,
    protocol_mix: MsrProtocolMix,
    checkpoints: &[usize],
    profile: MsrRunProfile,
) -> Result<MsrPhaseResult, String> {
    let plan = msr_output_plan_for_mix_and_profile(protocol_mix, profile);
    let plan_json = msr_plan_json(&plan, checkpoints, protocol_mix, profile);

    std::fs::create_dir_all(&env.work_dir).map_err(|error| error.to_string())?;
    let report_md = env
        .work_dir
        .join(format!("{}-report.md", profile.output_prefix()));
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.samples_jsonl);
    let _ = std::fs::remove_file(&report_md);

    let mut stack = start_resource_sweep_stack(&env).await?;
    let stream_key = profile.stream_key();
    let pipeline_id =
        create_resource_pipeline(&stack.api, profile.pipeline_name(), stream_key).await?;
    let mut publisher = spawn_msr_publisher(&env, stream_key, profile, false)?;
    wait_for_api_input_live(&stack.api, &pipeline_id, Duration::from_secs(60)).await?;
    let standby_input = create_backup_input(&stack.api, &pipeline_id).await?;
    let mut standby_publisher =
        spawn_msr_publisher(&env, &standby_input.stream_key, profile, true)?;
    wait_for_input_state(
        &stack.api,
        &pipeline_id,
        &standby_input.id,
        "standby",
        Duration::from_secs(60),
    )
    .await?;

    let max_outputs = *checkpoints
        .last()
        .ok_or("MSR checkpoint list unexpectedly empty".to_string())?;
    let mut output_ids = Vec::with_capacity(max_outputs);
    let mut aggregates = Vec::with_capacity(checkpoints.len());
    let mut signal_checks = Vec::new();

    for output in plan.iter().take(max_outputs) {
        let url = msr_output_url(&env, output);
        let output_id = create_output_with_rtmp_mode(
            &stack.api,
            &pipeline_id,
            &output.name,
            &url,
            &output.encoding,
            output.rtmp_mode,
        )
        .await?;
        start_output(&stack.api, &pipeline_id, &output_id).await?;
        output_ids.push(output_id);

        if checkpoints.binary_search(&output.ordinal).is_ok() {
            wait_for_outputs_progress(
                &stack.api,
                &pipeline_id,
                &output_ids,
                msr_progress_timeout(output_ids.len()),
            )
            .await?;
            let rtmp_count = plan
                .iter()
                .take(output.ordinal)
                .filter(|spec| spec.protocol == MsrProtocol::Rtmp)
                .count();
            let srt_count = output.ordinal - rtmp_count;
            let label = format!("{}-outputs", output.ordinal);

            let (path_health, sink_verification) = match env.peer_mode {
                ResourceSweepPeer::Mediamtx => {
                    let health = verify_msr_grouped_path_health(
                        &env,
                        &plan[..output.ordinal],
                        env_secs("MSR_SINK_SAMPLE_SECS", 3),
                        Duration::from_secs(env_secs("MSR_SINK_TIMEOUT_SECS", 60)),
                    )
                    .await?;
                    append_line(
                        &env.samples_jsonl,
                        &format!(
                            "{}\n",
                            serde_json::to_string(&mediamtx_path_health_json(
                                MSR_MODE, &label, &health
                            ))
                            .unwrap()
                        ),
                    )?;
                    (Some(health), None)
                }
                ResourceSweepPeer::Sink => {
                    // 5s, not 2s: real video is delivered at GOP cadence
                    // (bursts up to ~4.2s apart at this fixture's keyframe
                    // interval), so a leaf just past its last burst can
                    // show a genuinely flat 2s window with nothing wrong.
                    // Live-evidenced: a 2s window false-failed 5/700 leaves
                    // at the n=700 checkpoint of an otherwise-clean 1,200
                    // -output ramp; 5s cleared the same ramp with zero
                    // false positives end to end.
                    let verification = verify_msr_sink_checkpoint(
                        &stack.api,
                        &pipeline_id,
                        &output_ids,
                        env_secs("MSR_SINK_ENGINE_SAMPLE_SECS", 5),
                    )
                    .await?;
                    append_line(
                        &env.samples_jsonl,
                        &format!(
                            "{}\n",
                            serde_json::to_string(&msr_sink_verification_json(&verification))
                                .unwrap()
                        ),
                    )?;
                    (None, Some(verification))
                }
            };

            // Sink peers discard data at the transport layer, so there is
            // nothing for ffprobe/signal capture to read back from — force
            // the skip regardless of MSR_SKIP_FFPROBE.
            let skip_ffprobe = env.peer_mode == ResourceSweepPeer::Sink
                || std::env::var("MSR_SKIP_FFPROBE")
                    .map(|v| v.eq_ignore_ascii_case("true") || v == "1")
                    .unwrap_or(false);
            let ffprobe_checks = if skip_ffprobe {
                Vec::new()
            } else {
                run_msr_ffprobe_checkpoint(&env, output.ordinal, &plan[..output.ordinal]).await?
            };
            if profile == MsrRunProfile::SignalCalibration
                && env.peer_mode != ResourceSweepPeer::Sink
            {
                signal_checks.extend(
                    run_msr_signal_checkpoint(&env, output.ordinal, &plan[..output.ordinal])
                        .await?,
                );
            }
            let resource = sample_resource_window(
                &env,
                &mut stack,
                ResourceScenarioMeta {
                    scenario: MSR_MODE,
                    label,
                    pipelines: 1,
                    outputs: output.ordinal,
                    ingest_types: profile.ingest_types().to_string(),
                    egress_mix: format!("rtmp:{rtmp_count},srt:{srt_count}"),
                    transcode: "no",
                },
            )
            .await?;
            let post_sample_path_health = match env.peer_mode {
                ResourceSweepPeer::Mediamtx => {
                    let health = verify_msr_grouped_path_health(
                        &env,
                        &plan[..output.ordinal],
                        env_secs("MSR_SINK_POST_SAMPLE_SECS", 2),
                        Duration::from_secs(env_secs("MSR_SINK_TIMEOUT_SECS", 60)),
                    )
                    .await?;
                    append_line(
                        &env.samples_jsonl,
                        &format!(
                            "{}\n",
                            serde_json::to_string(&mediamtx_path_health_json(
                                MSR_MODE,
                                &format!("{}-post-sample", output.ordinal),
                                &health
                            ))
                            .unwrap()
                        ),
                    )?;
                    Some(health)
                }
                ResourceSweepPeer::Sink => None,
            };
            aggregates.push(MsrCheckpointAggregate {
                resource,
                path_health,
                post_sample_path_health,
                sink_verification,
                ffprobe_checks,
            });
        }
    }

    let resource_aggregates = aggregates
        .iter()
        .map(|aggregate| aggregate.resource.clone())
        .collect::<Vec<_>>();
    write_resource_sweep_csv(&env.summary_csv, &resource_aggregates)?;
    let rtmp_outputs = plan
        .iter()
        .take(output_ids.len())
        .filter(|output| output.protocol == MsrProtocol::Rtmp)
        .count();
    let enhanced_rtmp_outputs = plan
        .iter()
        .take(output_ids.len())
        .filter(|output| output.protocol == MsrProtocol::Rtmp)
        .filter(|output| output.rtmp_mode == RtmpOutputMode::Enhanced)
        .count();
    let srt_outputs = output_ids.len().saturating_sub(rtmp_outputs);
    std::fs::write(
        &report_md,
        format_msr_report(
            output_ids.len(),
            profile.audio_tracks(),
            rtmp_outputs,
            enhanced_rtmp_outputs,
            srt_outputs,
            &aggregates,
        ),
    )
    .map_err(|error| error.to_string())?;
    let result = json!({
        "mode": MSR_MODE,
        "status": "PASS",
        "profile": profile.label(),
        "plan": plan_json.clone(),
        "executedOutputs": output_ids.len(),
        "bufferedStandby": {
            "inputId": standby_input.id,
            "connected": true,
            "forwardingState": "standby",
        },
        "artifacts": {
            "summaryJson": env.summary_json.clone(),
            "summaryCsv": env.summary_csv.clone(),
            "reportMd": report_md.clone(),
            "samplesJsonl": env.samples_jsonl.clone(),
            "publisherLog": env.work_dir.join(format!("publisher-{}.log", profile.output_prefix())),
            "restreamLog": env.restream_log.clone(),
            "mediamtxLog": env.mediamtx_log.clone(),
        },
        "aggregates": aggregates.iter().map(msr_checkpoint_aggregate_json).collect::<Vec<_>>(),
        "signalChecks": signal_checks.clone(),
    });
    std::fs::write(
        &env.summary_json,
        serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;

    if env.no_cleanup {
        println!("MSR no-cleanup: leaving the live stack running");
        std::mem::forget(publisher);
        std::mem::forget(standby_publisher);
        std::mem::forget(stack);
    } else {
        stop_child(&mut publisher).await;
        stop_child(&mut standby_publisher).await;
        delete_resource_pipeline(&stack.api, &pipeline_id).await;
        stop_child(&mut stack.restream).await;
        stop_children(&mut stack.mediamtx).await;
        stop_harness_sink_peers(&mut stack.sink_peers).await;
    }
    Ok(MsrPhaseResult {
        env,
        report_md,
        plan_json,
        executed_outputs: output_ids.len(),
        aggregates,
        signal_checks,
    })
}

pub(crate) async fn msr() -> Result<Value, String> {
    let protocol_mix = MsrProtocolMix::from_env()?;
    let checkpoints = msr_checkpoints()?;
    let canonical_plan = msr_output_plan_for_mix(protocol_mix);
    let canonical_plan_json = msr_plan_json(
        &canonical_plan,
        &checkpoints,
        protocol_mix,
        MsrRunProfile::Canonical,
    );
    let signal_calibration = msr_signal_calibration_enabled();
    if std::env::var("MSR_PLAN_ONLY").ok().as_deref() == Some("1") {
        return Ok(json!({
            "status": "PLAN",
            "plan": canonical_plan_json,
            "signalCalibration": signal_calibration.then(|| {
                let signal_plan = msr_output_plan_for_mix_and_profile(
                    protocol_mix,
                    MsrRunProfile::SignalCalibration,
                );
                msr_plan_json(
                    &signal_plan,
                    &checkpoints,
                    protocol_mix,
                    MsrRunProfile::SignalCalibration,
                )
            }),
        }));
    }

    let mut base_env = ResourceSweepEnv::from_env_with_default_dir(".local/artifacts/msr")?;
    base_env.no_cleanup = std::env::var("MSR_NO_CLEANUP")
        .ok()
        .is_some_and(|value| value == "1");

    let calibration = if signal_calibration {
        let mut signal_env = base_env.clone();
        signal_env.work_dir = base_env.work_dir.join("signal-calibration");
        signal_env.no_cleanup = false;
        let signal_env = configure_msr_env(signal_env, MsrRunProfile::SignalCalibration);
        Some(
            run_msr_phase(
                signal_env,
                protocol_mix,
                &checkpoints,
                MsrRunProfile::SignalCalibration,
            )
            .await?,
        )
    } else {
        None
    };

    let canonical_env = configure_msr_env(base_env, MsrRunProfile::Canonical);
    let canonical = run_msr_phase(
        canonical_env,
        protocol_mix,
        &checkpoints,
        MsrRunProfile::Canonical,
    )
    .await?;

    let final_summary_json = canonical.env.summary_json.clone();
    let result = json!({
        "mode": MSR_MODE,
        "status": "PASS",
        "plan": canonical.plan_json.clone(),
        "executedOutputs": canonical.executed_outputs,
        "artifacts": {
            "summaryJson": canonical.env.summary_json.clone(),
            "summaryCsv": canonical.env.summary_csv.clone(),
            "reportMd": canonical.report_md.clone(),
            "samplesJsonl": canonical.env.samples_jsonl.clone(),
            "publisherLog": canonical.env.work_dir.join("publisher-msr.log"),
            "restreamLog": canonical.env.restream_log.clone(),
            "mediamtxLog": canonical.env.mediamtx_log.clone(),
            "signalCalibration": calibration.as_ref().map(|phase| json!({
                "summaryJson": phase.env.summary_json.clone(),
                "summaryCsv": phase.env.summary_csv.clone(),
                "reportMd": phase.report_md.clone(),
                "samplesJsonl": phase.env.samples_jsonl.clone(),
                "publisherLog": phase.env.work_dir.join("publisher-msr-signal.log"),
                "restreamLog": phase.env.restream_log.clone(),
                "mediamtxLog": phase.env.mediamtx_log.clone(),
            })),
        },
        "aggregates": canonical
            .aggregates
            .iter()
            .map(msr_checkpoint_aggregate_json)
            .collect::<Vec<_>>(),
        "signalCalibration": calibration.as_ref().map(|phase| json!({
            "profile": MsrRunProfile::SignalCalibration.label(),
            "plan": phase.plan_json.clone(),
            "executedOutputs": phase.executed_outputs,
            "signalChecks": phase.signal_checks.clone(),
            "aggregates": phase
                .aggregates
                .iter()
                .map(msr_checkpoint_aggregate_json)
                .collect::<Vec<_>>(),
        })),
    });
    std::fs::write(
        &final_summary_json,
        serde_json::to_vec_pretty(&result).map_err(|error| error.to_string())?,
    )
    .map_err(|error| error.to_string())?;
    Ok(result)
}
