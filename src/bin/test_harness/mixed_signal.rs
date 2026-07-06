//! Signal-capture and A/V marker quality helpers for mixed scenarios.

use super::*;

/// Parsed marker positions and quality flags from AV signal captures.
#[derive(Debug, Clone)]
pub(crate) struct MarkerQualityReport {
    pub(crate) video_markers: Vec<f64>,
    pub(crate) audio_markers: Vec<f64>,
    pub(crate) offsets_ms: Vec<f64>,
    pub(crate) max_abs_offset_ms: f64,
    pub(crate) drift_ms: f64,
    pub(crate) max_audio_pts_gap_ms: f64,
    pub(crate) pcm: PcmQualityReport,
}

/// PCM-level audio quality statistics for signal-control assertions.
#[derive(Debug, Clone, Copy)]
pub(crate) struct PcmQualityReport {
    pub(crate) samples: usize,
    pub(crate) clipping_samples: usize,
    pub(crate) max_step: i32,
    pub(crate) rms: f64,
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn signal_report_json(
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

pub(crate) async fn verify_mixed_signal_quality(
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
        // H.265 scenarios use codec-edge (hevc→h264) transcoder stages that
        // introduce a brief startup gap in audio PTS and may accumulate slightly
        // more A/V drift than H.264 passthrough.  Use wider tolerances so these
        // known startup artifacts don't fail the gate.
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

pub(crate) async fn capture_signal_sample(
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

pub(crate) async fn capture_signal_sample_once(
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

pub(crate) async fn run_ffmpeg_filter_log(
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

pub(crate) async fn decode_pcm_quality(
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

/// Quality thresholds passed to `validate_signal_quality`.
/// Defaults are calibrated for H.264 passthrough; H.265/codec-edge scenarios
/// may need wider tolerances due to transcoder startup overhead.
#[derive(Debug, Clone, Copy)]
pub(crate) struct SignalTolerances {
    /// Maximum A/V marker offset (static sync error), ms.
    pub(crate) max_abs_offset_ms: f64,
    /// Maximum A/V marker drift (sync error growth over stream), ms.
    pub(crate) drift_ms: f64,
    /// Maximum audio PTS deviation above the median frame interval, ms.
    /// Raised for H.265 codec-edge scenarios where the transcoder startup
    /// creates a brief initial audio gap that is not present during steady-state.
    pub(crate) max_audio_pts_gap_ms: f64,
}

impl Default for SignalTolerances {
    fn default() -> Self {
        Self {
            max_abs_offset_ms: 120.0,
            drift_ms: 80.0,
            max_audio_pts_gap_ms: 80.0,
        }
    }
}

pub(crate) fn validate_signal_quality(
    blackdetect_log: &str,
    silencedetect_log: &str,
    ashowinfo_log: &str,
    astats_log: &str,
    pcm: PcmQualityReport,
) -> Result<MarkerQualityReport, String> {
    validate_signal_quality_with_tolerances(
        blackdetect_log,
        silencedetect_log,
        ashowinfo_log,
        astats_log,
        pcm,
        &SignalTolerances::default(),
    )
}

pub(crate) fn validate_signal_quality_with_tolerances(
    blackdetect_log: &str,
    silencedetect_log: &str,
    ashowinfo_log: &str,
    astats_log: &str,
    pcm: PcmQualityReport,
    tolerances: &SignalTolerances,
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

    if max_abs_offset_ms > tolerances.max_abs_offset_ms {
        return Err(format!(
            "A/V marker offset too high: {max_abs_offset_ms:.1}ms"
        ));
    }
    if drift_ms > tolerances.drift_ms {
        return Err(format!("A/V marker drift too high: {drift_ms:.1}ms"));
    }
    if max_audio_pts_gap_ms > tolerances.max_audio_pts_gap_ms {
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

pub(crate) fn nearest_marker_offsets_ms(
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

pub(crate) fn assert_no_signal_bad_patterns(log: &str) -> Result<(), String> {
    let lower = log.to_ascii_lowercase();
    let bad_patterns = [
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

pub(crate) fn parse_blackdetect_intervals(log: &str) -> Vec<(f64, f64)> {
    parse_interval_pairs(log, "black_start:", "black_end:")
}

pub(crate) fn parse_silencedetect_intervals(log: &str) -> Vec<(f64, f64)> {
    parse_interval_pairs(log, "silence_start:", "silence_end:")
}

pub(crate) fn parse_interval_pairs(log: &str, start_key: &str, end_key: &str) -> Vec<(f64, f64)> {
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

pub(crate) fn marker_gaps_from_intervals(intervals: &[(f64, f64)]) -> Vec<f64> {
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

pub(crate) fn value_after_key(line: &str, key: &str) -> Option<f64> {
    let start = line.find(key)? + key.len();
    let value = line[start..]
        .trim_start()
        .split(|ch: char| ch.is_whitespace() || ch == '|' || ch == ']')
        .next()?;
    value.parse().ok()
}

pub(crate) fn max_audio_pts_gap_ms(ashowinfo_log: &str) -> f64 {
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

pub(crate) fn analyze_pcm_s16le(bytes: &[u8]) -> PcmQualityReport {
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
