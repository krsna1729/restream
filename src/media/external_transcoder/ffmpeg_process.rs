use std::sync::Arc;

use tokio::io::AsyncReadExt;
use tokio_util::sync::CancellationToken;
use tracing::error;

use crate::domain::audio_routing::{AudioRouting, parse_audio_operation, parse_audio_routing};
use crate::domain::output_spec::{StagePresetSpec, VideoCodecKind};
use crate::media::pipe_metrics::PipeMetrics;
use crate::media::startup_policy;

/// A Tokio child pipe as a blocking `File`, for a dedicated I/O thread.
pub(super) fn blocking_pipe(fd: std::os::fd::OwnedFd) -> std::io::Result<std::fs::File> {
    use std::os::fd::AsRawFd;
    let raw = fd.as_raw_fd();
    // SAFETY: `raw` is a valid descriptor owned by `fd` for this call.
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    // SAFETY: as above; F_SETFL takes no pointers.
    if flags < 0 || unsafe { libc::fcntl(raw, libc::F_SETFL, flags & !libc::O_NONBLOCK) } < 0 {
        return Err(std::io::Error::last_os_error());
    }
    Ok(std::fs::File::from(fd))
}

/// Feed the external FFmpeg child's stdin from its source ring on a dedicated
/// thread (WI11): the input pump pulls the ring and encodes TS on this
/// thread, and blocking pipe writes carry the back-pressure. Ends at end of
/// input, on cancellation or on a write error, then closes stdin so FFmpeg
/// sees EOF. The outcome is sent on `done`; the caller joins the returned
/// thread after reaping the child ([`reap_external_child`]).
pub(super) fn spawn_external_stdin_writer(
    stdin: tokio::process::ChildStdin,
    mut input: crate::media::ffmpeg::stage_input::StageInputRefill,
    pipe_metrics: Arc<PipeMetrics>,
    timing_clock: crate::media::timing::Clock,
    done: tokio::sync::oneshot::Sender<Result<(), String>>,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    use crate::media::avio::QueueRefill;
    use std::io::Write;

    let fd = stdin.into_owned_fd()?;
    // Increase the stdin pipe buffer so a full input burst fits without
    // back-pressure stalls.  256 KB accommodates ~90 packets (a 3-second
    // 18-stream burst) while staying well below the Linux 1 MB max.
    #[cfg(target_os = "linux")]
    {
        use std::os::fd::AsRawFd;
        const PIPE_BUF_SIZE: libc::c_int = 256 * 1024;
        // SAFETY: F_SETPIPE_SZ on a descriptor owned by `fd`; no pointers; failure is harmless.
        let _ = unsafe { libc::fcntl(fd.as_raw_fd(), libc::F_SETPIPE_SZ, PIPE_BUF_SIZE) };
    }
    let mut stdin = blocking_pipe(fd)?;
    std::thread::Builder::new()
        .name("restream-ffin".to_string())
        .spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut batch = Vec::with_capacity(crate::media::MEDIA_TS_BATCH_TARGET_BYTES);
                loop {
                    batch.clear();
                    if !input.refill(&mut batch) {
                        return Ok(());
                    }
                    let t0 = timing_clock.now();
                    stdin
                        .write_all(&batch)
                        .map_err(|error| format!("stdin write failed: {error}"))?;
                    let write_us = timing_clock.delta_us(t0);
                    if write_us > super::PIPE_STALL_THRESHOLD_US {
                        pipe_metrics.record_stall(write_us);
                    }
                }
            }))
            .unwrap_or_else(|_| Err("stdin writer panicked".to_string()));
            drop(stdin);
            let _ = done.send(result);
        })
}

/// Demux the external FFmpeg child's stdout into the stage's output ring on
/// a dedicated thread (WI11). Marks end of stream and cancels the stage when
/// FFmpeg closes stdout.
pub(super) fn spawn_external_stdout_reader(
    stdout: tokio::process::ChildStdout,
    mut output: crate::media::ffmpeg::stage_output::StageOutputNormalizer,
    pipe_metrics: Arc<PipeMetrics>,
    timing_clock: crate::media::timing::Clock,
    cancel: CancellationToken,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
    use crate::media::mpegts::TsDemuxer;
    use crate::media::ring_buffer::MEDIA_PRODUCER_BATCH_PACKETS;
    use std::io::Read;

    let mut stdout = blocking_pipe(stdout.into_owned_fd()?)?;
    std::thread::Builder::new()
        .name("restream-ffout".to_string())
        .spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let mut demuxer = TsDemuxer::new();
                let mut buf = vec![0u8; MEDIA_TS_BATCH_TARGET_BYTES];
                let mut pkts = Vec::with_capacity(MEDIA_PRODUCER_BATCH_PACKETS);
                loop {
                    let t0 = timing_clock.now();
                    let result = stdout.read(&mut buf);
                    let idle_us = timing_clock.delta_us(t0);
                    match result {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if idle_us > super::PIPE_STALL_THRESHOLD_US {
                                pipe_metrics.record_idle(idle_us);
                            }
                            demuxer.feed(&buf[..n]);
                            demuxer.drain_into(&mut pkts);
                            for pkt in pkts.drain(..) {
                                output.push(pkt);
                            }
                        }
                    }
                }
                demuxer.flush();
                demuxer.drain_into(&mut pkts);
                for pkt in pkts.drain(..) {
                    output.push(pkt);
                }
                output.mark_end_of_stream();
            }));
            cancel.cancel();
        })
}

/// How long FFmpeg may take to exit after its input ended normally (it
/// flushes the encoder and writes the tail).
pub(super) const EXTERNAL_EOF_GRACE: std::time::Duration = std::time::Duration::from_secs(30);
/// How long FFmpeg may take to exit once the stage is cancelled. Its output
/// is being torn down, and a stdin writer blocked on a stalled child is only
/// released when the child goes away.
pub(super) const EXTERNAL_CANCEL_GRACE: std::time::Duration = std::time::Duration::from_secs(1);

/// Reap the external FFmpeg child, then join its I/O threads. The child
/// gets [`EXTERNAL_EOF_GRACE`] after a normal end of input and
/// [`EXTERNAL_CANCEL_GRACE`] after cancellation, and is killed past that.
/// Once it is gone its pipes are broken, so a writer blocked in `write_all`
/// fails and the stdout reader sees EOF: the joins are bounded.
pub(super) async fn reap_external_child(
    child: &mut tokio::process::Child,
    cancelled: bool,
    io_threads: Vec<std::thread::JoinHandle<()>>,
) {
    let grace = if cancelled {
        EXTERNAL_CANCEL_GRACE
    } else {
        EXTERNAL_EOF_GRACE
    };
    if tokio::time::timeout(grace, child.wait()).await.is_err() {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    for thread in io_threads {
        let _ = tokio::task::spawn_blocking(move || thread.join()).await;
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
