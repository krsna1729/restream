use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::domain::audio_routing::{AudioRouting, parse_audio_operation, parse_audio_routing};
use crate::domain::output_spec::{StagePresetSpec, VideoCodecKind};
use crate::media::pipe_metrics::PipeMetrics;
use crate::media::startup_policy;

/// Byte sink that writes MPEG-TS batches to the external FFmpeg child's stdin.
pub(super) struct ExternalStdinSink {
    pub(super) stdin: tokio::process::ChildStdin,
    pipe_metrics: Arc<PipeMetrics>,
    timing_clock: crate::media::timing::Clock,
}

impl ExternalStdinSink {
    pub(super) fn new(
        stdin: tokio::process::ChildStdin,
        pipe_metrics: Arc<PipeMetrics>,
        timing_clock: crate::media::timing::Clock,
    ) -> Self {
        // Increase the stdin pipe buffer so a full input burst fits without
        // back-pressure stalls.  256 KB accommodates ~90 packets (a 3-second
        // 18-stream burst) while staying well below the Linux 1 MB max.
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let fd = stdin.as_raw_fd();
            const PIPE_BUF_SIZE: libc::c_int = 256 * 1024;
            let _ = unsafe { libc::fcntl(fd, libc::F_SETPIPE_SZ, PIPE_BUF_SIZE) };
        }
        Self {
            stdin,
            pipe_metrics,
            timing_clock,
        }
    }
}

impl crate::media::ffmpeg::stage_input::StageByteSink for ExternalStdinSink {
    async fn write_ts(&mut self, bytes: &[u8], cancel: &CancellationToken) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        let t0 = self.timing_clock.now();
        let mut remaining = bytes;
        loop {
            tokio::select! {
                biased;
                _ = cancel.cancelled() => {
                    let write_us = self.timing_clock.delta_us(t0);
                    if write_us > super::PIPE_STALL_THRESHOLD_US {
                        self.pipe_metrics.record_stall(write_us);
                    }
                    return Err("cancelled: stdin write interrupted".to_string());
                }
                result = self.stdin.write(remaining) => {
                    match result {
                        Ok(0) => return Err("stdin write returned 0 (pipe closed)".to_string()),
                        Ok(n) => {
                            remaining = &remaining[n..];
                            if remaining.is_empty() {
                                let write_us = self.timing_clock.delta_us(t0);
                                if write_us > super::PIPE_STALL_THRESHOLD_US {
                                    self.pipe_metrics.record_stall(write_us);
                                }
                                return Ok(());
                            }
                        }
                        Err(e) => return Err(format!("stdin write failed: {e}")),
                    }
                }
            }
        }
    }
}

/// Build FFmpeg arguments for a **shared transcoder stage**.
///
/// Input  : MPEG-TS read from stdin (`-i -`)
/// Output : MPEG-TS written to stdout (`pipe:1`)
///
/// `input_codec` selects the video encoder: `"hevc"` / `"h265"` -> `libx265`,
/// anything else -> `libx264`.  Pass the ingest codec so that H.265 sources
/// transcode to H.265 output (preserving codec across the preset stage)
/// and H.264 sources transcode to H.264 output.
fn build_stage_ffmpeg_args_inner(
    preset: &str,
    input_codec: &str,
    probe_codec: &str,
    include_audio: bool,
    audio_track_count: usize,
    observed_bitrate_bps: Option<u64>,
    threads: Option<u32>,
) -> Vec<String> {
    // Strip the internal stage-key prefix ("video:720p" -> "720p").
    // Audio stages receive the selected upstream video ring, so they copy video
    // while applying any channel-level audio filter.
    let stage_spec = StagePresetSpec::parse(preset);
    let encoding = stage_spec.video_encoding();
    let audio_routing = stage_audio_routing(preset);
    let profile = if matches!(encoding, "" | "source" | "custom") {
        None
    } else {
        Some(crate::media::profiles::try_get_cached(encoding))
    };
    let passthrough = matches!(stage_spec.video_encoding(), "source" | "");
    let full_stream_passthrough = passthrough && audio_routing.is_none();
    let probed_audio_track_count =
        probe_audio_track_count(&audio_routing, include_audio, audio_track_count);
    let (analyze_duration_us, probe_size_bytes) =
        startup_policy::ext_stage_probe_budget_for(startup_policy::ExtStageProbeContext {
            codec: VideoCodecKind::from_codec_name(probe_codec),
            include_audio,
            audio_track_count: probed_audio_track_count,
            passthrough: full_stream_passthrough,
            observed_bitrate_bps,
        });
    let ffmpeg_threads = threads.unwrap_or(2).max(1);

    let mut args = vec![
        "-nostdin".to_string(),
        "-hide_banner".to_string(),
        "-nostats".to_string(),
        "-loglevel".to_string(),
        "warning".to_string(),
        "-threads".to_string(),
        ffmpeg_threads.to_string(),
        "-flags".to_string(),
        "low_delay".to_string(),
        "-analyzeduration".to_string(),
        analyze_duration_us.to_string(),
        "-probesize".to_string(),
        probe_size_bytes.to_string(),
        "-f".to_string(),
        "mpegts".to_string(),
        "-i".to_string(),
        "pipe:0".to_string(),
    ];

    if !include_audio {
        args.extend(["-map".to_string(), "0:v:0".to_string()]);
    } else if let Some(filter) = audio_filter_complex(&audio_routing) {
        args.extend(["-filter_complex".to_string(), filter]);
        args.extend(["-map".to_string(), "0:v:0?".to_string()]);
        args.extend(["-map".to_string(), "[aout]".to_string()]);
    } else {
        args.extend(["-map".to_string(), "0:v:0".to_string()]);
        args.extend(["-map".to_string(), "0:a?".to_string()]);
    }

    // Video filter (scaling).
    if let Some(profile) = &profile
        && profile.width > 0
        && profile.height > 0
    {
        args.extend([
            "-vf".to_string(),
            format!("scale={}:{}", profile.width, profile.height),
        ]);
    }

    let is_passthrough = matches!(encoding, "" | "source" | "custom");
    if is_passthrough {
        args.extend(["-c:v".to_string(), "copy".to_string()]);
    } else {
        // Preserve codec: H.265 source -> libx265, H.264 source -> libx264.
        let encoder = if matches!(input_codec, "hevc" | "h265") {
            "libx265"
        } else {
            "libx264"
        };
        args.extend([
            "-c:v".to_string(),
            encoder.to_string(),
            "-preset".to_string(),
            profile
                .as_ref()
                .map(|profile| profile.preset.clone())
                .unwrap_or_else(|| "veryfast".to_string()),
        ]);
        if encoder == "libx265" {
            args.extend([
                "-x265-params".to_string(),
                "repeat-headers=1:log-level=none".to_string(),
            ]);
        }
        if let Some(profile) = &profile {
            if !profile.tune.is_empty() {
                args.extend(["-tune".to_string(), profile.tune.clone()]);
            }
            args.extend(["-g".to_string(), profile.gop.to_string()]);
            args.extend(["-bf".to_string(), profile.bframes.to_string()]);
            if profile.bitrate > 0 {
                args.extend(["-b:v".to_string(), profile.bitrate.to_string()]);
                if profile.max_bitrate > 0 {
                    args.extend(["-maxrate".to_string(), profile.max_bitrate.to_string()]);
                    args.extend(["-bufsize".to_string(), profile.max_bitrate.to_string()]);
                }
            } else {
                args.extend(["-crf".to_string(), profile.crf.to_string()]);
            }
        }
    }

    // atrack selection stays in the zero-copy audio router. Channel-level
    // remap/downmix stages arrive here and must decode/filter/re-encode audio.
    if !include_audio {
        // Video-only stages intentionally omit audio from the live pipe so FFmpeg
        // does not stall probing a high-track-count TS input when preview only
        // needs browser-safe video.
    } else if audio_routing.is_some() {
        args.extend([
            "-c:a".to_string(),
            "aac".to_string(),
            "-b:a".to_string(),
            "160k".to_string(),
            "-ac".to_string(),
            "2".to_string(),
        ]);
    } else {
        args.extend(["-c:a".to_string(), "copy".to_string()]);
    }

    args.extend([
        "-mpegts_flags".to_string(),
        "resend_headers+pat_pmt_at_frames".to_string(),
        "-pes_payload_size".to_string(),
        "0".to_string(),
        "-omit_video_pes_length".to_string(),
        "0".to_string(),
        "-max_interleave_delta".to_string(),
        "0".to_string(),
        "-flush_packets".to_string(),
        "1".to_string(),
        "-muxdelay".to_string(),
        "0".to_string(),
        "-muxpreload".to_string(),
        "0".to_string(),
        "-f".to_string(),
        "mpegts".to_string(),
        "pipe:1".to_string(),
    ]);

    args
}

pub fn build_stage_ffmpeg_args(preset: &str, input_codec: &str) -> Vec<String> {
    build_stage_ffmpeg_args_inner(preset, input_codec, input_codec, true, 1, None, None)
}

/// Like [`build_stage_ffmpeg_args`], but sizes FFmpeg's input probe budget from
/// the codec actually flowing into the stage rather than the encoder-selection
/// codec. On codec edges, an `hevc_to_h264` stage encodes H.264 while stdin
/// carries H.265 and needs the larger HEVC probe window.
pub fn build_stage_ffmpeg_args_for_input(
    preset: &str,
    input_codec: &str,
    probe_codec: &str,
) -> Vec<String> {
    build_stage_ffmpeg_args_inner(preset, input_codec, probe_codec, true, 1, None, None)
}

pub fn build_stage_ffmpeg_args_for_input_streams(
    preset: &str,
    input_codec: &str,
    probe_codec: &str,
    include_audio: bool,
    audio_track_count: usize,
) -> Vec<String> {
    build_stage_ffmpeg_args_for_observed_input_streams(
        preset,
        input_codec,
        probe_codec,
        include_audio,
        audio_track_count,
        None,
    )
}

pub fn build_stage_ffmpeg_args_for_observed_input_streams(
    preset: &str,
    input_codec: &str,
    probe_codec: &str,
    include_audio: bool,
    audio_track_count: usize,
    observed_bitrate_bps: Option<u64>,
) -> Vec<String> {
    build_stage_ffmpeg_args_inner(
        preset,
        input_codec,
        probe_codec,
        include_audio,
        audio_track_count,
        observed_bitrate_bps,
        None,
    )
}

pub fn build_stage_ffmpeg_video_only_args(preset: &str, input_codec: &str) -> Vec<String> {
    build_stage_ffmpeg_args_inner(preset, input_codec, input_codec, false, 0, None, None)
}

pub fn build_stage_ffmpeg_video_only_args_for_input(
    preset: &str,
    input_codec: &str,
    probe_codec: &str,
) -> Vec<String> {
    build_stage_ffmpeg_args_inner(preset, input_codec, probe_codec, false, 0, None, None)
}

fn stage_audio_routing(preset: &str) -> Option<AudioRouting> {
    let operation = StagePresetSpec::parse(preset)
        .audio_operation()
        .map(str::to_string);

    let routing = if let Some(operation) = operation {
        parse_audio_operation(&operation)
    } else {
        parse_audio_routing(preset)
    };

    match routing {
        AudioRouting::Remap { .. } | AudioRouting::Downmix { .. } => Some(routing),
        _ => None,
    }
}

fn audio_filter_complex(routing: &Option<AudioRouting>) -> Option<String> {
    match routing {
        Some(AudioRouting::Remap { left, right, track }) => Some(format!(
            "[0:a:{track}]pan=stereo|c0=c{left}|c1=c{right}[aout]"
        )),
        Some(AudioRouting::Downmix { track }) => {
            Some(format!("[0:a:{track}]aresample=out_chlayout=stereo[aout]"))
        }
        _ => None,
    }
}

fn probe_audio_track_count(
    routing: &Option<AudioRouting>,
    include_audio: bool,
    observed_audio_track_count: usize,
) -> usize {
    if !include_audio {
        return 0;
    }

    match routing {
        Some(AudioRouting::Remap { track, .. }) | Some(AudioRouting::Downmix { track }) => {
            track.saturating_add(1)
        }
        Some(AudioRouting::SelectTracks { tracks }) => tracks
            .iter()
            .copied()
            .max()
            .map(|track| track.saturating_add(1))
            .unwrap_or(0),
        Some(AudioRouting::Passthrough) | None => observed_audio_track_count,
    }
}

pub(super) fn spawn_external_stderr_logger(
    mut stderr: tokio::process::ChildStderr,
    label: String,
    correlation_id: String,
    pipeline_id: String,
    encoding: String,
) {
    const STDERR_CAP: usize = 1 << 20;
    tokio::spawn(async move {
        let mut buf = [0u8; 4096];
        let mut all: Vec<u8> = Vec::new();
        let mut truncated = false;
        loop {
            match stderr.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let chunk = &buf[..n];
                    let remaining = STDERR_CAP.saturating_sub(all.len());
                    if remaining > 0 {
                        all.extend_from_slice(&chunk[..n.min(remaining)]);
                    } else if !truncated {
                        truncated = true;
                        error!(
                            correlation_id = %correlation_id,
                            pipeline_id = %pipeline_id,
                            stage_encoding = %encoding,
                            stage_backend = "external_ffmpeg",
                            "[ext-transcoder] ffmpeg stderr ({}) truncated at 1 MB",
                            label
                        );
                    }
                }
            }
        }
        if !all.is_empty() {
            let text = String::from_utf8_lossy(&all).trim().to_string();
            let text = actionable_external_ffmpeg_stderr(&text);
            if text.is_empty() {
                return;
            }

            error!(
                correlation_id = %correlation_id,
                pipeline_id = %pipeline_id,
                stage_encoding = %encoding,
                stage_backend = "external_ffmpeg",
                "[ext-transcoder] ffmpeg stderr ({}): {}",
                label,
                text
            );
        }
    });
}

fn expected_external_ffmpeg_decoder_chatter(line: &str) -> bool {
    const PATTERNS: [&str; 5] = [
        "PPS id out of range",
        "Could not find ref with POC",
        "Error constructing the frame RPS.",
        "Skipping invalid undecodable NALU",
        "Error parsing NAL",
    ];

    PATTERNS.iter().any(|pattern| line.contains(pattern))
}

fn actionable_external_ffmpeg_stderr(text: &str) -> String {
    text.lines()
        .filter(|line| !expected_external_ffmpeg_decoder_chatter(line))
        .collect::<Vec<_>>()
        .join("\n")
}

#[cfg(test)]
#[path = "ffmpeg_process_tests.rs"]
mod tests;
