//! Mixed-runner output verification helpers.

use super::*;

pub(crate) async fn verify_mixed_output_dimensions(
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

pub(crate) async fn verify_mixed_output_cases_inner(
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
