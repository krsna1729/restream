//! Mixed-runner output verification helpers.

use super::*;
use std::time::SystemTime;

#[allow(clippy::too_many_arguments)]
fn emit_mixed_output_cell_timing(
    env: &MixedEnv,
    cfg: &str,
    case: &MixedOutputCase,
    label: &str,
    index: usize,
    selected_checks: &[&str],
    failure_count: usize,
    started_at: SystemTime,
    started: Instant,
    status: &str,
) -> Result<(), String> {
    emit_mixed_timing_window(
        env,
        cfg,
        &format!("output.cell.{}", case.id()),
        status,
        started_at,
        SystemTime::now(),
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

pub(crate) async fn preflight_mixed_rtmps_capabilities(
    api: &RampApi,
    cases: &[MixedOutputCase],
) -> Result<(), String> {
    if !cases
        .iter()
        .any(|case| case.protocol() == MixedOutputProtocol::Rtmps)
    {
        return Ok(());
    }
    preflight_rtmps_ktls_capabilities(api).await
}

pub(crate) async fn preflight_rtmps_ktls_capabilities(api: &RampApi) -> Result<(), String> {
    let system = api.get_json("/metrics/system?view=summary").await?;
    let capabilities = &system["rtmps"]["ktlsCapabilities"];
    let unsupported = unsupported_rtmps_ktls_suites(capabilities);
    if unsupported.is_empty() {
        println!("[rtmps-ktls] kernel TLS available: {capabilities}");
        Ok(())
    } else {
        Err(format!(
            "kTLS unavailable on this host (environment, not a test result): RTMPS live output \
             requires kernel TLS support for every AES-GCM TLS suite; unsupported: {unsupported:?}; \
             probe: {capabilities}. Load the kernel `tls` module on the host"
        ))
    }
}

// Live peers may negotiate any of these suites; a partial probe is not enough
// because production kTLS handoff requires the exact negotiated pair.
fn unsupported_rtmps_ktls_suites(capabilities: &serde_json::Value) -> Vec<&'static str> {
    [
        "tls12Aes128Gcm",
        "tls12Aes256Gcm",
        "tls13Aes128Gcm",
        "tls13Aes256Gcm",
    ]
    .into_iter()
    .filter(|name| !capabilities[*name].as_bool().unwrap_or(false))
    .collect()
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
    let input_case = mixed_input_case_for_command(cfg)
        .ok_or_else(|| format!("{cfg} is not a known mixed input case"))?;
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
                expected_video_codec: Some(case.expected_video_codec_for_input(input_case)),
                mediamtx_api: matches!(
                    case.protocol(),
                    MixedOutputProtocol::Rtmp | MixedOutputProtocol::Rtmps
                )
                .then_some(env.mtx_api),
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
    if !env.check_selected("ffprobe")
        && !env.check_selected("audio-route")
        && !env.check_selected("decode-scan")
        && !env.check_selected("signal")
    {
        return Ok(());
    }
    let started = Instant::now();
    let index = env.probe_duplicate_index();
    let mut failures = Vec::new();
    let input_case = mixed_input_case_for_command(cfg)
        .ok_or_else(|| format!("{cfg} is not a known mixed input case"))?;
    for case in cases {
        let cell_failures = verify_output_case(OutputCaseRun {
            env,
            api,
            cfg,
            case,
            input_case,
            resume,
            index,
            skip_direct_srt_sinks,
            decode_scan,
        })
        .await?;
        if let Some(cell_failures) = cell_failures {
            failures.extend(cell_failures);
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

/// One output cell of [`verify_mixed_output_cases_inner`].
struct OutputCaseRun<'a> {
    env: &'a MixedEnv,
    api: &'a RampApi,
    cfg: &'a str,
    case: &'a MixedOutputCase,
    input_case: MixedInputCase,
    resume: &'a mut MixedResume,
    index: usize,
    skip_direct_srt_sinks: bool,
    decode_scan: bool,
}

/// Run the selected checks for one output cell; `None` when the cell is
/// skipped. Returned failures are the cell's collected failures.
async fn verify_output_case(run: OutputCaseRun<'_>) -> Result<Option<Vec<String>>, String> {
    let OutputCaseRun {
        env,
        api,
        cfg,
        case,
        input_case,
        resume,
        index,
        skip_direct_srt_sinks,
        decode_scan,
    } = run;
    if skip_direct_srt_sinks
        && env.ffmpeg_srt_sink
        && matches!(case.protocol(), MixedOutputProtocol::Srt)
    {
        return Ok(None);
    }
    let cell_started_at = SystemTime::now();
    let cell_started = Instant::now();
    let url = mixed_output_read_url(env, cfg, case, index);
    let label = format!("{} out{index}", case.id());
    let cell = env.output_cell(case.id(), index);
    let mediamtx_publish_probe = matches!(
        case.protocol(),
        MixedOutputProtocol::Rtmp | MixedOutputProtocol::Rtmps
    ) && case.expected_video_codec_for_input(input_case) == "hevc";
    let mut output_failed = false;
    let mut checks = CellChecks::default();
    let cell_ctx = CellCheckContext {
        env,
        cfg,
        case,
        label: &label,
        index,
        cell_started_at,
        cell_started,
    };
    if env.check_selected("ffprobe") {
        let ffprobe_id = mixed_output_check_id(cfg, case.id(), "ffprobe");
        output_failed |= run_cell_check(
            &cell_ctx,
            &mut checks,
            "ffprobe",
            true,
            verify_mixed_stream(
                env,
                api,
                MixedProbeSpec {
                    cfg,
                    id: ffprobe_id,
                    label: &label,
                    url: &url,
                    expected: case.expected_dimensions(),
                    expected_video_codec: Some(case.expected_video_codec_for_input(input_case)),
                    mediamtx_api: matches!(
                        case.protocol(),
                        MixedOutputProtocol::Rtmp | MixedOutputProtocol::Rtmps
                    )
                    .then_some(env.mtx_api),
                    cookie: None,
                    cell: cell.clone(),
                },
                resume,
            ),
        )
        .await?;
    }
    if env.check_selected("audio-route") && !output_failed && !mediamtx_publish_probe {
        let audio_id = mixed_output_check_id(cfg, case.id(), "audio_route");
        output_failed |= run_cell_check(
            &cell_ctx,
            &mut checks,
            "audio_route",
            true,
            verify_mixed_audio_route(
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
            ),
        )
        .await?;
    }
    if env.check_selected("decode-scan") && decode_scan && !output_failed && !mediamtx_publish_probe
    {
        let decode_id = mixed_output_check_id(cfg, case.id(), "decode_scan");
        let _ = run_cell_check(
            &cell_ctx,
            &mut checks,
            "decode_scan",
            false,
            verify_mixed_decode_scan(
                env,
                api,
                MixedProbeSpec {
                    cfg,
                    id: decode_id,
                    label: &label,
                    url: &url,
                    expected: case.expected_dimensions(),
                    expected_video_codec: None,
                    mediamtx_api: None,
                    cookie: None,
                    cell,
                },
                resume,
            ),
        )
        .await?;
    }
    if env.check_selected("signal")
        && !env.direct_signal_sink(case.protocol())
        && !mediamtx_publish_probe
    {
        let signal_id = mixed_output_check_id(cfg, case.id(), "signal");
        let _ = run_cell_check(
            &cell_ctx,
            &mut checks,
            "signal",
            false,
            verify_mixed_signal_quality(env, cfg, &signal_id, &label, &url, resume),
        )
        .await?;
    }
    emit_output_cell_timing(&cell_ctx, &checks)?;
    Ok(Some(checks.failures))
}

/// Emit the per-cell timing row, with the cell's checks and failure count.
fn emit_output_cell_timing(ctx: &CellCheckContext<'_>, checks: &CellChecks) -> Result<(), String> {
    let cell_failure_count = checks.failures.len();
    emit_mixed_output_cell_timing(
        ctx.env,
        ctx.cfg,
        ctx.case,
        ctx.label,
        ctx.index,
        &checks.selected_checks,
        cell_failure_count,
        ctx.cell_started_at,
        ctx.cell_started,
        if cell_failure_count == 0 {
            "pass"
        } else {
            "fail"
        },
    )
}

/// Per-cell check bookkeeping, shared by the four optional checks.
#[derive(Default)]
struct CellChecks {
    selected_checks: Vec<&'static str>,
    failures: Vec<String>,
}

struct CellCheckContext<'a> {
    env: &'a MixedEnv,
    cfg: &'a str,
    case: &'a MixedOutputCase,
    label: &'a str,
    index: usize,
    cell_started_at: SystemTime,
    cell_started: Instant,
}

/// Record and run one output check for a cell, applying the shared
/// collect-or-fail policy.
async fn run_cell_check(
    ctx: &CellCheckContext<'_>,
    checks: &mut CellChecks,
    check: &'static str,
    invalidating: bool,
    result: impl std::future::Future<Output = Result<(), String>>,
) -> Result<bool, String> {
    checks.selected_checks.push(check);
    if let Err(error) = result.await {
        if ctx.env.collect_failures {
            checks.failures.push(error);
            return Ok(invalidating);
        } else {
            emit_mixed_output_cell_timing(
                ctx.env,
                ctx.cfg,
                ctx.case,
                ctx.label,
                ctx.index,
                &checks.selected_checks,
                1,
                ctx.cell_started_at,
                ctx.cell_started,
                "fail",
            )?;
            return Err(error);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    #[test]
    fn rtmps_preflight_rejects_partial_kernel_tls_suite_support() {
        let capabilities = serde_json::json!({
            "tls12Aes128Gcm": true,
            "tls12Aes256Gcm": true,
            "tls13Aes128Gcm": false,
            "tls13Aes256Gcm": true,
        });

        assert_eq!(
            super::unsupported_rtmps_ktls_suites(&capabilities),
            vec!["tls13Aes128Gcm"]
        );
    }
}
