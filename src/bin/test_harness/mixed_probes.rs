//! ffprobe/ffmpeg decode-scan helpers for mixed output verification.

use super::*;

pub(crate) fn ffprobe_compact_video_dimensions(log: &str) -> Option<String> {
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

pub(crate) fn ffprobe_compact_audio_track_count(log: &str) -> usize {
    ffprobe_compact_stream_lines(log)
        .filter(|line| ffprobe_compact_field(line, "codec_type") == Some("audio"))
        .filter_map(|line| ffprobe_compact_field(line, "index"))
        .collect::<HashSet<_>>()
        .len()
}

pub(crate) fn ffprobe_compact_validate_dts(log: &str) -> Result<usize, String> {
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

fn ffprobe_compact_stream_lines(log: &str) -> impl Iterator<Item = &str> {
    log.lines().filter(|line| line.starts_with("stream|"))
}

fn ffprobe_compact_field<'a>(line: &'a str, key: &str) -> Option<&'a str> {
    line.split('|').find_map(|field| {
        let (field_key, value) = field.split_once('=')?;
        (field_key == key).then_some(value)
    })
}

/// Inputs for one ffprobe-based mixed output assertion.
pub(crate) struct MixedProbeSpec<'a> {
    pub(crate) cfg: &'a str,
    pub(crate) id: String,
    pub(crate) label: &'a str,
    pub(crate) url: &'a str,
    pub(crate) expected: &'a str,
    pub(crate) cookie: Option<&'a str>,
}

pub(crate) async fn verify_mixed_stream(
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

pub(crate) async fn warm_mixed_stream(
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
pub(crate) async fn verify_mixed_audio_route(
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

pub(crate) async fn verify_mixed_decode_scan(
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

pub(crate) fn decode_scan_needs_video_dts_fallback(
    url: &str,
    status: Option<i32>,
    matched_pattern: Option<&'static str>,
) -> bool {
    (url.starts_with("rtmp://") || url.starts_with("srt://"))
        && status == Some(0)
        && matches!(matched_pattern, Some("non monoton" | "non-monoton"))
}

pub(crate) async fn ffmpeg_decode_scan(
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
