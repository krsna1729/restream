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
    let (mut analyze_duration_us, mut probe_size_bytes) =
        startup_policy::ext_stage_probe_budget(VideoCodecKind::from_codec_name(probe_codec));
    if matches!(stage_spec.video_encoding(), "source" | "") {
        // Source/passthrough stages copy streams without re-encoding, so they need
        // a larger probe budget to fully resolve all stream parameters (especially audio).
        analyze_duration_us = 1_000_000;
        probe_size_bytes = 512 * 1024;
    }
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
    build_stage_ffmpeg_args_inner(preset, input_codec, input_codec, true, None)
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
    build_stage_ffmpeg_args_inner(preset, input_codec, probe_codec, true, None)
}

pub fn build_stage_ffmpeg_video_only_args(preset: &str, input_codec: &str) -> Vec<String> {
    build_stage_ffmpeg_args_inner(preset, input_codec, input_codec, false, None)
}

pub fn build_stage_ffmpeg_video_only_args_for_input(
    preset: &str,
    input_codec: &str,
    probe_codec: &str,
) -> Vec<String> {
    build_stage_ffmpeg_args_inner(preset, input_codec, probe_codec, false, None)
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
mod tests {
    use super::*;
    use crate::domain::transcode_profile::TranscodeProfile;
    use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
    use crate::media::mpegts::TsDemuxer;

    fn write_temp_ts_artifact(name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "restream-external-transcoder-test-{}-{}",
            std::process::id(),
            name
        ));
        std::fs::create_dir_all(&dir).expect("create temp artifact dir");
        let path = dir.join("artifact.ts");
        std::fs::write(&path, bytes).expect("write temp TS artifact");
        path
    }

    #[test]
    fn stage_args_720p_reads_stdin_writes_stdout() {
        let args = build_stage_ffmpeg_args("720p", "h264");
        assert!(args.windows(2).any(|w| w == ["-threads", "2"]));
        assert!(args.iter().any(|a| a == "-i"));
        let i_pos = args.iter().position(|a| a == "-i").unwrap();
        assert_eq!(args[i_pos + 1], "pipe:0");
        assert!(args.iter().any(|a| a == "-vf"));
        let vf_pos = args.iter().position(|a| a == "-vf").unwrap();
        assert!(args[vf_pos + 1].contains("1280"));
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "libx264");
        assert!(args.windows(2).any(|w| w == ["-flush_packets", "1"]));
        assert!(args.windows(2).any(|w| w == ["-muxdelay", "0"]));
        assert!(args.windows(2).any(|w| w == ["-muxpreload", "0"]));
        assert!(args.windows(2).any(|w| w == ["-pes_payload_size", "0"]));
        let (analyze_duration_us, probe_size_bytes) =
            startup_policy::ext_stage_probe_budget(VideoCodecKind::H264);
        assert!(
            args.windows(2)
                .any(|w| { w[0] == "-analyzeduration" && w[1] == analyze_duration_us.to_string() })
        );
        assert!(
            args.windows(2)
                .any(|w| { w[0] == "-probesize" && w[1] == probe_size_bytes.to_string() })
        );
        assert!(args.last() == Some(&"pipe:1".to_string()));
    }

    #[test]
    fn external_stderr_filter_drops_expected_hevc_decoder_chatter() {
        let text = concat!(
            "[hevc @ 0x1] Could not find ref with POC 512\n",
            "[hevc @ 0x1] Error constructing the frame RPS.\n",
            "[hevc @ 0x1] Skipping invalid undecodable NALU: 1\n"
        );

        assert!(actionable_external_ffmpeg_stderr(text).is_empty());
    }

    #[test]
    fn external_stderr_filter_keeps_actionable_lines() {
        let text = concat!(
            "[hevc @ 0x1] Could not find ref with POC 512\n",
            "Conversion failed!\n"
        );

        assert_eq!(
            actionable_external_ffmpeg_stderr(text),
            "Conversion failed!"
        );
    }

    #[test]
    fn stage_args_hevc_raise_probe_budget() {
        let args = build_stage_ffmpeg_args("720p", "hevc");
        let (analyze_duration_us, probe_size_bytes) =
            startup_policy::ext_stage_probe_budget(VideoCodecKind::Hevc);
        assert!(
            args.windows(2)
                .any(|w| { w[0] == "-analyzeduration" && w[1] == analyze_duration_us.to_string() })
        );
        assert!(
            args.windows(2)
                .any(|w| { w[0] == "-probesize" && w[1] == probe_size_bytes.to_string() })
        );
    }

    #[test]
    fn stage_args_codec_edge_probes_input_codec_but_encodes_output_codec() {
        let args = build_stage_ffmpeg_args_for_input("h264", "h264", "hevc");
        let (analyze_duration_us, probe_size_bytes) =
            startup_policy::ext_stage_probe_budget(VideoCodecKind::Hevc);
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "libx264");
        assert!(
            args.windows(2)
                .any(|w| { w[0] == "-analyzeduration" && w[1] == analyze_duration_us.to_string() })
        );
        assert!(
            args.windows(2)
                .any(|w| { w[0] == "-probesize" && w[1] == probe_size_bytes.to_string() })
        );
    }

    #[test]
    fn stage_args_720p_hevc_uses_libx265() {
        for codec in &["hevc", "h265"] {
            let args = build_stage_ffmpeg_args("720p", codec);
            let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
            assert_eq!(args[cv_pos + 1], "libx265", "codec={codec}");
            let x265_pos = args.iter().position(|a| a == "-x265-params").unwrap();
            assert_eq!(args[x265_pos + 1], "repeat-headers=1:log-level=none");
            assert!(args.last() == Some(&"pipe:1".to_string()));
        }
    }

    #[test]
    fn stage_args_custom_profile_uses_profile_settings() {
        {
            let mut cache = crate::media::profiles::cache().blocking_write();
            cache.insert(
                "square_test".to_string(),
                TranscodeProfile {
                    preset: "superfast".to_string(),
                    tune: "zerolatency".to_string(),
                    crf: 21,
                    gop: 100,
                    bframes: 1,
                    bitrate: 1500000,
                    max_bitrate: 2000000,
                    width: 640,
                    height: 640,
                },
            );
        }

        let args = build_stage_ffmpeg_args("square_test", "h264");
        assert!(args.windows(2).any(|w| w == ["-vf", "scale=640:640"]));
        assert!(args.windows(2).any(|w| w == ["-preset", "superfast"]));
        assert!(args.windows(2).any(|w| w == ["-g", "100"]));
        assert!(args.windows(2).any(|w| w == ["-bf", "1"]));
        assert!(args.windows(2).any(|w| w == ["-b:v", "1500000"]));
        assert!(args.windows(2).any(|w| w == ["-maxrate", "2000000"]));
        assert!(!args.iter().any(|arg| arg == "-crf"));
    }

    #[test]
    fn stage_args_source_copies_video() {
        let args = build_stage_ffmpeg_args("source", "h264");
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "copy");
        assert!(!args.iter().any(|a| a == "-vf"));
        assert!(args.last() == Some(&"pipe:1".to_string()));
    }

    #[test]
    fn stage_args_h264_transcodes_without_scaling() {
        let args = build_stage_ffmpeg_args("h264", "h264");
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "libx264");
        assert!(!args.iter().any(|a| a == "-vf"));
    }

    #[test]
    fn stage_args_video_prefix_stripped() {
        let a = build_stage_ffmpeg_args("video:720p", "h264");
        let b = build_stage_ffmpeg_args("720p", "h264");
        assert_eq!(a, b);
    }

    #[test]
    fn stage_args_non_dsp_audio_is_copied() {
        for preset in &["720p", "1080p", "source"] {
            let args = build_stage_ffmpeg_args(preset, "h264");
            let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
            assert_eq!(args[ca_pos + 1], "copy", "preset={preset}");
        }
    }

    #[test]
    fn stage_args_remap_uses_pan_filter_and_audio_encode() {
        let args = build_stage_ffmpeg_args("audio:remap:1:0:2:from:720p", "h264");

        let filter_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
        assert_eq!(args[filter_pos + 1], "[0:a:2]pan=stereo|c0=c1|c1=c0[aout]");
        assert!(args.windows(2).any(|w| w == ["-map", "0:v:0?"]));
        assert!(args.windows(2).any(|w| w == ["-map", "[aout]"]));
        let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
        assert_eq!(args[ca_pos + 1], "aac");
        assert!(args.windows(2).any(|w| w == ["-ac", "2"]));
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "copy");
    }

    #[test]
    fn stage_args_downmix_uses_stereo_resample_filter() {
        let args = build_stage_ffmpeg_args("audio:downmix:1:from:source", "h264");

        let filter_pos = args.iter().position(|a| a == "-filter_complex").unwrap();
        assert_eq!(
            args[filter_pos + 1],
            "[0:a:1]aresample=out_chlayout=stereo[aout]"
        );
        let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
        assert_eq!(args[ca_pos + 1], "aac");
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "copy");
    }

    #[test]
    fn stage_args_atrack_stays_packet_copy() {
        let args = build_stage_ffmpeg_args("audio:atrack:0:from:720p", "h264");

        assert!(!args.iter().any(|a| a == "-filter_complex"));
        let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
        assert_eq!(args[ca_pos + 1], "copy");
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "copy");
    }

    #[test]
    fn stage_args_empty_preset_copies_video_and_audio() {
        let args = build_stage_ffmpeg_args("", "h264");
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "copy");
        let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
        assert_eq!(args[ca_pos + 1], "copy");
    }

    #[test]
    fn stage_args_custom_preset_copies_video_and_audio() {
        let args = build_stage_ffmpeg_args("custom", "h264");
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "copy");
        let ca_pos = args.iter().position(|a| a == "-c:a").unwrap();
        assert_eq!(args[ca_pos + 1], "copy");
    }

    #[test]
    fn stage_audio_routing_remap_is_some() {
        let r = stage_audio_routing("audio:remap:0:1:0:from:source");
        assert!(r.is_some());
        assert!(matches!(r, Some(AudioRouting::Remap { .. })));
    }

    #[test]
    fn stage_audio_routing_downmix_is_some() {
        let r = stage_audio_routing("audio:downmix:0:from:source");
        assert!(r.is_some());
        assert!(matches!(r, Some(AudioRouting::Downmix { .. })));
    }

    #[test]
    fn stage_audio_routing_atrack_returns_none() {
        let r = stage_audio_routing("audio:atrack:0:from:720p");
        assert!(r.is_none());
    }

    #[test]
    fn stage_audio_routing_video_preset_returns_none() {
        assert!(stage_audio_routing("720p").is_none());
        assert!(stage_audio_routing("source").is_none());
    }

    #[test]
    fn audio_filter_complex_remap_format() {
        let routing = Some(AudioRouting::Remap {
            left: 1,
            right: 0,
            track: 2,
        });
        let filter = audio_filter_complex(&routing).unwrap();
        assert_eq!(filter, "[0:a:2]pan=stereo|c0=c1|c1=c0[aout]");
    }

    #[test]
    fn audio_filter_complex_downmix_format() {
        let routing = Some(AudioRouting::Downmix { track: 1 });
        let filter = audio_filter_complex(&routing).unwrap();
        assert_eq!(filter, "[0:a:1]aresample=out_chlayout=stereo[aout]");
    }

    #[test]
    fn audio_filter_complex_none_for_no_routing() {
        assert!(audio_filter_complex(&None).is_none());
    }

    #[test]
    fn stage_args_profile_with_crf_when_bitrate_zero() {
        {
            let mut cache = crate::media::profiles::cache().blocking_write();
            cache.insert(
                "crf_test".to_string(),
                TranscodeProfile {
                    preset: "veryfast".to_string(),
                    tune: String::new(),
                    crf: 28,
                    gop: 60,
                    bframes: 0,
                    bitrate: 0,
                    max_bitrate: 0,
                    width: 1280,
                    height: 720,
                },
            );
        }
        let args = build_stage_ffmpeg_args("crf_test", "h264");
        assert!(args.windows(2).any(|w| w == ["-crf", "28"]));
        assert!(!args.iter().any(|a| a == "-b:v"));
        assert!(!args.iter().any(|a| a == "-maxrate"));
    }

    #[test]
    fn stage_args_audio_stage_strips_prefix_and_copies_video() {
        let args = build_stage_ffmpeg_args("audio:atrack:0:from:720p", "h264");
        let cv_pos = args.iter().position(|a| a == "-c:v").unwrap();
        assert_eq!(args[cv_pos + 1], "copy");
        assert!(!args.iter().any(|a| a == "-vf"));
    }

    #[tokio::test]
    async fn kill_and_wait_on_child_without_piped_stdin_does_not_hang() {
        let mut child = tokio::process::Command::new("true")
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .expect("failed to spawn 'true'");

        assert!(child.stdin.take().is_none());

        let _ = child.kill().await;
        let status = child.wait().await.expect("wait must not fail");
        let _ = status;
    }

    #[test]
    fn feeder_remuxed_single_audio_hevc_fixture_transcodes_as_file_input() {
        let (video, audio_tracks, packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h265")
                .expect("single-audio HEVC fixture");
        let mut feeder = TsPacketFeeder::new(
            Some(&video),
            std::sync::Arc::new(audio_tracks),
            PacketFeedConfig::default(),
        );
        let mut ts_bytes = Vec::new();
        let mut packet_buf = Vec::new();

        for packet in &packets {
            packet_buf.clear();
            if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
                ts_bytes.extend_from_slice(&packet_buf);
            }
        }

        let input_path = write_temp_ts_artifact("hevc-feeder-transcode-input", &ts_bytes);
        let output_path = input_path
            .parent()
            .expect("temp artifact dir")
            .join("output.ts");
        let ffmpeg = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
        let mut args = build_stage_ffmpeg_args_for_input("720p", "h264", "hevc");
        let input_pos = args
            .iter()
            .position(|arg| arg == "-i")
            .expect("stage args should contain input flag");
        args[input_pos + 1] = input_path.to_string_lossy().to_string();
        let last = args.last_mut().expect("stage args should contain output");
        *last = output_path.to_string_lossy().to_string();

        let output = std::process::Command::new(ffmpeg)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn bundled ffmpeg transcode");

        assert!(
            output.status.success(),
            "ffmpeg should transcode feeder-remuxed HEVC TS file input: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            std::fs::metadata(&output_path)
                .map(|meta| meta.len() > 0)
                .unwrap_or(false),
            "file-based transcode should produce a non-empty TS output"
        );
    }

    #[test]
    fn feeder_remuxed_h264_marker_fixture_transcodes_as_file_input() {
        let path =
            crate::test_fixtures::av_marker_transport_fixture("h264", false).expect("marker path");
        let file_bytes = std::fs::read(&path).expect("read H.264 marker fixture");
        let mut demuxer = TsDemuxer::new();
        let mut packets = Vec::new();
        for chunk in file_bytes.chunks(1316) {
            demuxer.feed(chunk);
            demuxer.drain_into(&mut packets);
        }
        demuxer.flush();
        demuxer.drain_into(&mut packets);

        let probe = demuxer.take_probe().expect("probe H.264 marker fixture");
        let video = probe.video.expect("marker fixture should contain video");
        let audio_tracks = probe.audio_tracks;

        let mut feeder = TsPacketFeeder::new(
            Some(&video),
            std::sync::Arc::new(audio_tracks),
            PacketFeedConfig::default(),
        );
        let mut ts_bytes = Vec::new();
        let mut packet_buf = Vec::new();

        for packet in &packets {
            packet_buf.clear();
            if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
                ts_bytes.extend_from_slice(&packet_buf);
            }
        }

        assert!(
            !ts_bytes.is_empty(),
            "remuxed H.264 marker fixture should produce TS bytes"
        );

        let input_path = write_temp_ts_artifact("h264-marker-transcode-input", &ts_bytes);
        let output_path = input_path
            .parent()
            .expect("temp artifact dir")
            .join("output.ts");
        let ffmpeg = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
        let mut args = build_stage_ffmpeg_args("720p", "h264");
        let input_pos = args
            .iter()
            .position(|arg| arg == "-i")
            .expect("stage args should contain input flag");
        args[input_pos + 1] = input_path.to_string_lossy().to_string();
        let last = args.last_mut().expect("stage args should contain output");
        *last = output_path.to_string_lossy().to_string();

        let output = std::process::Command::new(ffmpeg)
            .args(&args)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("spawn bundled ffmpeg transcode");

        assert!(
            output.status.success(),
            "ffmpeg should transcode feeder-remuxed H.264 marker TS file input: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            std::fs::metadata(&output_path)
                .map(|meta| meta.len() > 0)
                .unwrap_or(false),
            "file-based marker transcode should produce a non-empty TS output"
        );

        let video_only_path = input_path
            .parent()
            .expect("temp artifact dir")
            .join("output-video-only.ts");
        let decode_video = std::process::Command::new(ffmpeg)
            .args([
                "-v",
                "error",
                "-i",
                output_path.to_string_lossy().as_ref(),
                "-map",
                "0:v:0",
                "-c",
                "copy",
                "-f",
                "mpegts",
                video_only_path.to_string_lossy().as_ref(),
            ])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::piped())
            .output()
            .expect("probe transcoded marker TS video stream");
        assert!(
            decode_video.status.success(),
            "transcoded marker output should contain a decodable video stream: {}",
            String::from_utf8_lossy(&decode_video.stderr)
        );
    }
}
