//! Mixed-runner sink orchestration helpers.

use super::*;

/// Direct FFmpeg SRT listener used to validate SRT egress without MediaMTX.
pub(crate) struct FfmpegSrtSink {
    pub(crate) group: String,
    pub(crate) index: usize,
    pub(crate) port: u16,
    pub(crate) log_path: PathBuf,
    pub(crate) expected_dimensions: String,
    pub(crate) expected_audio_tracks: usize,
    pub(crate) child: Child,
}

/// Direct FFmpeg RTMP/SRT listener used for AV marker signal capture.
pub(crate) struct FfmpegSignalSink {
    pub(crate) cfg: String,
    pub(crate) group: String,
    pub(crate) index: usize,
    pub(crate) publish_url: String,
    pub(crate) capture_path: PathBuf,
    pub(crate) child: Child,
}

pub(crate) async fn run_optional_mixed_sink_probe(
    env: &MixedEnv,
    api: &RampApi,
    pipeline_id: &str,
    cfg: &str,
    sink_port: u16,
    output_ids: &mut Vec<String>,
    resume: &mut MixedResume,
) -> Result<(Option<SinkProbeResult>, Option<String>), String> {
    let probe_id = mixed_scenario_check_id(cfg, "sink_probe");
    if !env.check_selected("sink-probe") || !resume.allows(&probe_id) {
        return Ok((None, None));
    }

    let started = Instant::now();
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

#[allow(clippy::too_many_arguments)]
pub(crate) async fn add_mixed_output_cases(
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
pub(crate) async fn add_mixed_multi_output_cases(
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

/// Expected probe shape for one direct SRT sink group.
pub(crate) struct MixedSrtGroupValidation<'a> {
    pub(crate) label: &'a str,
    pub(crate) expected_dimensions: &'a str,
    pub(crate) expected_audio_tracks: usize,
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn add_mixed_srt_group<F>(
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

pub(crate) async fn spawn_ffmpeg_signal_sink(
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

pub(crate) async fn finish_ffmpeg_signal_sinks(
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
pub(crate) async fn validate_signal_capture_artifact(
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
        let tolerances = if cfg.contains("h265") || cfg.contains("hevc") {
            SignalTolerances {
                drift_ms: 100.0,
                max_audio_pts_gap_ms: 300.0,
                ..SignalTolerances::default()
            }
        } else {
            SignalTolerances::default()
        };
        validate_signal_quality_with_tolerances(&black, &silence, &ashow, &astats, pcm, &tolerances)
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

pub(crate) async fn spawn_ffmpeg_srt_sink(
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

pub(crate) async fn finish_ffmpeg_srt_sinks(sinks: &mut [FfmpegSrtSink]) -> Result<(), String> {
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
