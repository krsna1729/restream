//! Mixed-runner output verification helpers.

use super::*;

#[allow(clippy::too_many_arguments)]
fn emit_mixed_output_cell_timing(
    env: &MixedEnv,
    cfg: &str,
    case: &MixedOutputCase,
    label: &str,
    index: usize,
    selected_checks: &[&str],
    failure_count: usize,
    started_at: chrono::DateTime<Utc>,
    started: Instant,
    status: &str,
) -> Result<(), String> {
    emit_mixed_timing_window(
        env,
        cfg,
        &format!("output.cell.{}", case.id()),
        status,
        started_at,
        Utc::now(),
        started.elapsed(),
        Some(json!({
            "cellId": case.id(),
            "label": label,
            "protocol": mixed_output_protocol_name(case.protocol()),
            "encoding": case.encoding(),
            "expectedDimensions": case.expected_dimensions(),
            "expectedAudioTracks": case.expected_audio_tracks(),
            "outputIndex": index,
            "checks": selected_checks,
            "failureCount": failure_count,
        })),
    )
}

pub(crate) async fn verify_mixed_output_dimensions(
    env: &MixedEnv,
    api: &RampApi,
    cfg: &str,
    cases: &[MixedOutputCase],
    resume: &mut MixedResume,
) -> Result<(), String> {
    if !env.check_selected("ffprobe") {
        return Ok(());
    }
    let index = env.probe_duplicate_index();
    for case in cases {
        let url = mixed_output_read_url(env, cfg, case, index);
        verify_mixed_stream(
            env,
            api,
            MixedProbeSpec {
                cfg,
                id: mixed_output_check_id(cfg, case.id(), "ffprobe"),
                label: &format!("{} out{index}", case.id()),
                url: &url,
                expected: case.expected_dimensions(),
                cookie: None,
                cell: env.output_cell(case.id(), index),
            },
            resume,
        )
        .await?;
    }
    Ok(())
}

pub(crate) async fn verify_mixed_output_cases_inner(
    env: &MixedEnv,
    api: &RampApi,
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
    let index = env.probe_duplicate_index();
    let mut failures = Vec::new();
    for case in cases {
        let cell_started_at = Utc::now();
        let cell_started = Instant::now();
        let cell_failures_before = failures.len();
        let mut selected_checks = Vec::new();
        if skip_direct_srt_sinks
            && env.ffmpeg_srt_sink
            && matches!(case.protocol(), MixedOutputProtocol::Srt)
        {
            continue;
        }
        let url = mixed_output_read_url(env, cfg, case, index);
        let label = format!("{} out{index}", case.id());
        let cell = env.output_cell(case.id(), index);
        let mut output_failed = false;
        if env.check_selected("ffprobe") {
            selected_checks.push("ffprobe");
            let ffprobe_id = mixed_output_check_id(cfg, case.id(), "ffprobe");
            let ffprobe_result = verify_mixed_stream(
                env,
                api,
                MixedProbeSpec {
                    cfg,
                    id: ffprobe_id,
                    label: &label,
                    url: &url,
                    expected: case.expected_dimensions(),
                    cookie: None,
                    cell: cell.clone(),
                },
                resume,
            )
            .await;
            if let Err(error) = ffprobe_result {
                if env.collect_failures {
                    output_failed = true;
                    failures.push(error);
                } else {
                    emit_mixed_output_cell_timing(
                        env,
                        cfg,
                        case,
                        &label,
                        index,
                        &selected_checks,
                        1,
                        cell_started_at,
                        cell_started,
                        "fail",
                    )?;
                    return Err(error);
                }
            }
        }
        if env.check_selected("ffprobe") && !output_failed {
            selected_checks.push("audio_route");
            let audio_id = mixed_output_check_id(cfg, case.id(), "audio_route");
            let audio_result = verify_mixed_audio_route(
                env,
                api,
                cfg,
                &audio_id,
                &label,
                &url,
                case.expected_dimensions(),
                case.expected_audio_tracks(),
                cell.clone(),
                resume,
            )
            .await;
            if let Err(error) = audio_result {
                if env.collect_failures {
                    output_failed = true;
                    failures.push(error);
                } else {
                    emit_mixed_output_cell_timing(
                        env,
                        cfg,
                        case,
                        &label,
                        index,
                        &selected_checks,
                        1,
                        cell_started_at,
                        cell_started,
                        "fail",
                    )?;
                    return Err(error);
                }
            }
        }
        if env.check_selected("ffprobe") && decode_scan && !output_failed {
            selected_checks.push("decode_scan");
            let decode_id = mixed_output_check_id(cfg, case.id(), "decode_scan");
            let decode_result = verify_mixed_decode_scan(
                env,
                api,
                MixedProbeSpec {
                    cfg,
                    id: decode_id,
                    label: &label,
                    url: &url,
                    expected: case.expected_dimensions(),
                    cookie: None,
                    cell,
                },
                resume,
            )
            .await;
            if let Err(error) = decode_result {
                if env.collect_failures {
                    failures.push(error);
                } else {
                    emit_mixed_output_cell_timing(
                        env,
                        cfg,
                        case,
                        &label,
                        index,
                        &selected_checks,
                        1,
                        cell_started_at,
                        cell_started,
                        "fail",
                    )?;
                    return Err(error);
                }
            }
        }
        if env.check_selected("signal") && !env.use_direct_signal_sinks() {
            selected_checks.push("signal");
            let signal_id = mixed_output_check_id(cfg, case.id(), "signal");
            let signal_result =
                verify_mixed_signal_quality(env, cfg, &signal_id, &label, &url, resume).await;
            if let Err(error) = signal_result {
                if env.collect_failures {
                    failures.push(error);
                } else {
                    emit_mixed_output_cell_timing(
                        env,
                        cfg,
                        case,
                        &label,
                        index,
                        &selected_checks,
                        1,
                        cell_started_at,
                        cell_started,
                        "fail",
                    )?;
                    return Err(error);
                }
            }
        }
        let cell_failure_count = failures.len().saturating_sub(cell_failures_before);
        emit_mixed_output_cell_timing(
            env,
            cfg,
            case,
            &label,
            index,
            &selected_checks,
            cell_failure_count,
            cell_started_at,
            cell_started,
            if cell_failure_count == 0 {
                "pass"
            } else {
                "fail"
            },
        )?;
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
            "probeSampling": {
                "policy": env.probe_sampling_policy.as_str(),
                "duplicateIndex": env.probe_duplicate_index(),
            },
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
