//! External transcoder: shared pipeline stage using a subprocess FFmpeg.
//!
//! # Architecture
//!
//! The external transcoder is a **shared stage** in the media graph, not a
//! per-output process. One FFmpeg subprocess is spawned per live FFmpeg-backed
//! `StageKey` (`planner::select_backend`): a video preset per
//! codec variant, the HEVC-to-H.264 codec edge, non-lightweight audio routes
//! and the HEVC preview bridge. All egress outputs that request the same stage
//! on the same pipeline read from the shared output ring buffer.
//!
//! ```text
//! source_ring
//!     │  (Reader + TsMuxer → MPEG-TS bytes)
//!     ▼
//! FFmpeg stdin  ──►  [scale + libx264 + …]  ──►  FFmpeg stdout (MPEG-TS)
//!                                                       │
//!                                           (TsDemuxer → MediaPackets)
//!                                                       │
//!                                                 output_ring  ─── shared
//!                                                       │
//!                                    ┌─────────────────┼──────────────────┐
//!                                RTMP-out1          SRT-out1          HLS-out1
//! ```
//!
//! # Passthrough
//!
//! `source` encodings never enter a video-preset stage; on HEVC ingest, an
//! H.264-only destination still goes through the codec edge. Persisted
//! `custom` output rows also fall through as passthrough; output create/update
//! rejects custom output encodings until custom FFmpeg args are applied.
//!
//! Every non-passthrough FFmpeg stage runs here; lightweight audio track
//! selection runs in `audio_router` (`planner::select_backend`).

use std::process::Stdio;
use std::sync::Arc;
use tokio::process::Command;
use tracing::{error, info};

use crate::ffmpeg::backend::{BackendError, StageRunContext};
use crate::ffmpeg::stage_input::StageInputPump;
use crate::ffmpeg::stage_output::StageOutputNormalizer;
use crate::ffmpeg::stage_plan::{FfmpegStagePlan, VideoStageOp};
use crate::pipe_metrics::PipeMetrics;

use crate::stage_lifecycle::{StageBackendKind, StageLifecycleGuard, StagePhase};

mod ffmpeg_process;
pub(crate) use ffmpeg_process::{blocking_pipe, reap_external_child, spawn_external_stdin_writer};
pub use ffmpeg_process::{
    build_stage_ffmpeg_args, build_stage_ffmpeg_args_for_input,
    build_stage_ffmpeg_args_for_input_streams, build_stage_ffmpeg_args_for_observed_input_streams,
    build_stage_ffmpeg_video_only_args, build_stage_ffmpeg_video_only_args_for_input,
};
use ffmpeg_process::{spawn_external_stderr_logger, spawn_external_stdout_reader};

/// Stdin writes or stdout reads exceeding this threshold are counted as stalls/idles.
/// 1 ms filters normal async scheduling jitter while catching real back-pressure.
const PIPE_STALL_THRESHOLD_US: u64 = 1_000;

#[cfg(test)]
use restream_core::MediaType;

#[cfg(test)]
fn external_output_stream_idx(
    media_type: MediaType,
    track_index: u32,
    audio_tracks: &[restream_core::metadata::AudioMeta],
    include_audio: bool,
) -> Option<usize> {
    match media_type {
        MediaType::Video => Some(0),
        MediaType::Audio if include_audio => audio_tracks
            .iter()
            .position(|track| track.track_index == track_index)
            .map(|index| index + 1),
        MediaType::Audio => None,
    }
}

// ── Shared stage entry point ───────────────────────────────────────────────

/// Run one external transcoder stage for `(pipeline_id, encoding)`.
///
/// Spawns an `ffmpeg` subprocess with stdin/stdout piped. Two concurrent tasks
/// manage the pipe ends:
///
/// * **stdin task** (runs in the caller's task): reads `input_buffer`, muxes
///   packets to MPEG-TS, writes to FFmpeg stdin.
/// * **stdout task** (separate Tokio task): reads FFmpeg stdout, feeds a
///   `TsDemuxer`, pushes demuxed `MediaPacket`s to `output_buffer`.
///
/// The stage shuts down when `cancel` fires or when the stdin/stdout pipe
/// closes. On exit the cancel token is triggered so the engine can clean up
/// the stage entry and restart it on the next reconciler cycle.
///
/// Run an external FFmpeg stage using the shared input pump and output
/// normalizer. This is the real `FfmpegStageBackend` implementation.
pub(crate) async fn run_external_ffmpeg_backend(
    plan: FfmpegStagePlan,
    input_pump: StageInputPump,
    output_normalizer: StageOutputNormalizer,
    ctx: StageRunContext,
) -> Result<(), BackendError> {
    let pipeline_id = ctx.pipeline_id.as_str();
    let encoding = ctx.stage_key.kind.to_string();
    let _lifecycle_guard = StageLifecycleGuard::new(ctx.lifecycle.clone());

    let Some(_ffmpeg_permit) = acquire_ffmpeg_capacity(&ctx).await? else {
        return Ok(());
    };

    let args = stage_ffmpeg_args(&plan, &encoding, &input_pump.codec_hint());
    info!(?args, "FFMPEG ARGS");
    let correlation_id = crate::correlation_id::next_correlation_id("stage");
    let ids = StageIds {
        correlation_id: &correlation_id,
        pipeline_id,
        encoding: &encoding,
    };
    ids.log_start(plan.include_audio);

    let FfmpegChild {
        mut child,
        stdin,
        stdout,
        stderr,
    } = spawn_ffmpeg(&args, &ctx, &ids).await?;

    let pipe_metrics = Arc::new(PipeMetrics::default());
    ctx.engine
        .stages
        .register_pipe_metrics(ctx.stage_key.clone(), pipe_metrics.clone())
        .await;
    spawn_external_stderr_logger(
        stderr,
        format!("{}:{}", pipeline_id, encoding),
        correlation_id.clone(),
        pipeline_id.to_string(),
        encoding.clone(),
    );

    let (io_threads, input_result, cancelled) = run_ffmpeg_io(
        stdin,
        stdout,
        input_pump,
        output_normalizer,
        pipe_metrics,
        &ctx.cancel,
    )
    .await;
    if let Err(e) = input_result {
        ids.log_input_failure(&e);
    }

    reap_external_child(&mut child, cancelled, io_threads).await;
    // A cancelled entry is never reused; the consumers' lease drops remove
    // it, and only if no newer stage took its key.
    ctx.cancel.cancel();
    emit_stage_stopped(&ctx, &ids);

    ids.log_exit();
    Ok(())
}

/// The stage's names, for its logs and events.
struct StageIds<'a> {
    correlation_id: &'a str,
    pipeline_id: &'a str,
    encoding: &'a str,
}

impl StageIds<'_> {
    fn log_start(&self, include_audio: bool) {
        info!(
            correlation_id = %self.correlation_id,
            pipeline_id = %self.pipeline_id,
            stage_encoding = %self.encoding,
            stage_backend = "external_ffmpeg",
            include_audio,
            "[ext-transcoder] stage start  pipeline={} encoding={}",
            self.pipeline_id,
            self.encoding
        );
    }

    fn log_input_failure(&self, error: &str) {
        error!(
            correlation_id=%self.correlation_id,
            pipeline_id=%self.pipeline_id,
            stage_encoding=%self.encoding,
            "[ext-transcoder] input pump failed ({}:{}): {}",
            self.pipeline_id,
            self.encoding,
            error
        );
    }

    fn log_exit(&self) {
        info!(
            correlation_id = %self.correlation_id,
            pipeline_id = %self.pipeline_id,
            stage_encoding = %self.encoding,
            stage_backend = "external_ffmpeg",
            "[ext-transcoder] stage exit   pipeline={} encoding={}",
            self.pipeline_id,
            self.encoding
        );
    }
}

/// Waits for an external FFmpeg slot. `None` when the stage is cancelled
/// while waiting.
async fn acquire_ffmpeg_capacity(
    ctx: &StageRunContext,
) -> Result<Option<tokio::sync::SemaphorePermit<'_>>, BackendError> {
    ctx.lifecycle.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });
    let permit = tokio::select! {
        permit = ctx.engine.runtime.external_ffmpeg_semaphore.acquire() => match permit {
            Ok(p) => p,
            Err(e) => {
                error!(
                    pipeline_id = %ctx.pipeline_id,
                    stage = %ctx.stage_key,
                    "external ffmpeg semaphore closed: {e}"
                );
                return Err(BackendError(e.to_string()));
            }
        },
        _ = ctx.cancel.cancelled() => {
            info!(
                pipeline_id = %ctx.pipeline_id,
                stage = %ctx.stage_key,
                "external ffmpeg wait cancelled"
            );
            return Ok(None);
        }
    };
    ctx.lifecycle.transition(StagePhase::CapacityAcquired {
        backend: StageBackendKind::ExternalFfmpeg,
    });
    Ok(Some(permit))
}

/// The stage's FFmpeg arguments. The input ring's codec hint wins over the
/// plan's input codec.
fn stage_ffmpeg_args(plan: &FfmpegStagePlan, encoding: &str, codec_hint: &str) -> Vec<String> {
    let probe_codec = match codec_hint {
        "" => plan.input.codec_hint.as_str(),
        hint => hint,
    };
    let observed_bitrate_bps = plan
        .input
        .video_meta
        .as_ref()
        .and_then(|video| video.bw)
        .filter(|bitrate| bitrate.is_finite() && *bitrate > 0.0)
        .map(|bitrate| bitrate.round() as u64);
    build_stage_ffmpeg_args_for_observed_input_streams(
        &external_stage_arg_preset(plan, encoding),
        plan.output_codec.as_str(),
        probe_codec,
        plan.include_audio,
        plan.input.audio_tracks.len(),
        observed_bitrate_bps,
    )
}

/// A spawned FFmpeg child with its three pipes.
struct FfmpegChild {
    child: tokio::process::Child,
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    stderr: tokio::process::ChildStderr,
}

/// Spawns FFmpeg with piped stdio. A spawn failure or a missing stdin or
/// stdout pipe emits `StageStopped`; a child missing any pipe is reaped.
async fn spawn_ffmpeg(
    args: &[String],
    ctx: &StageRunContext,
    ids: &StageIds<'_>,
) -> Result<FfmpegChild, BackendError> {
    let mut child = match Command::new(crate::ffmpeg_binary::ffmpeg_bin_path())
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => {
            ctx.lifecycle.transition(StagePhase::BackendSpawned {
                backend: StageBackendKind::ExternalFfmpeg,
                pid: c.id(),
            });
            c
        }
        Err(e) => {
            error!(
                correlation_id = %ids.correlation_id,
                pipeline_id = %ids.pipeline_id,
                stage_encoding = %ids.encoding,
                stage_backend = "external_ffmpeg",
                "[ext-transcoder] failed to spawn ffmpeg ({}:{}): {}",
                ids.pipeline_id,
                ids.encoding,
                e
            );
            emit_stage_stopped(ctx, ids);
            return Err(BackendError(e.to_string()));
        }
    };
    match (child.stdin.take(), child.stdout.take(), child.stderr.take()) {
        (Some(stdin), Some(stdout), Some(stderr)) => Ok(FfmpegChild {
            child,
            stdin,
            stdout,
            stderr,
        }),
        (stdin, stdout, _) => {
            let (missing, stage_stopped) = if stdin.is_none() {
                ("stdin", true)
            } else if stdout.is_none() {
                ("stdout", true)
            } else {
                ("stderr", false)
            };
            error!(
                correlation_id=%ids.correlation_id,
                pipeline_id=%ids.pipeline_id,
                stage_encoding=%ids.encoding,
                "[ext-transcoder] ffmpeg {missing} unavailable"
            );
            crate::child_process::reap(&mut child, std::time::Duration::ZERO).await;
            if stage_stopped {
                emit_stage_stopped(ctx, ids);
            }
            Err(BackendError(format!("{missing} unavailable")))
        }
    }
}

/// Runs FFmpeg's I/O on dedicated threads, not Tokio: one demuxes stdout
/// into the output ring, one pulls the source ring and writes stdin. Waits
/// for the writer to finish or the stage to be cancelled; returns the
/// threads, the writer's result and whether the stage was cancelled (a
/// failed thread start counts as cancelled).
async fn run_ffmpeg_io(
    stdin: tokio::process::ChildStdin,
    stdout: tokio::process::ChildStdout,
    input_pump: StageInputPump,
    output_normalizer: StageOutputNormalizer,
    pipe_metrics: Arc<PipeMetrics>,
    cancel: &tokio_util::sync::CancellationToken,
) -> (Vec<std::thread::JoinHandle<()>>, Result<(), String>, bool) {
    let (input_done_tx, mut input_done_rx) = tokio::sync::oneshot::channel();
    let mut io_threads = Vec::with_capacity(2);
    let io_started = spawn_external_stdout_reader(
        stdout,
        output_normalizer,
        pipe_metrics.clone(),
        cancel.clone(),
    )
    .and_then(|reader| {
        io_threads.push(reader);
        spawn_external_stdin_writer(
            stdin,
            input_pump.into_queue_refill(cancel.clone()),
            pipe_metrics,
            input_done_tx,
        )
    })
    .map(|writer| io_threads.push(writer));
    // Cancellation (including the stdout reader seeing FFmpeg exit) stops
    // the writer at its next refill; a writer blocked on a stalled child is
    // released when `reap_external_child` kills it after the short cancel
    // grace.
    let (input_result, cancelled) = match io_started {
        Err(error) => (
            Err(format!("failed to start ffmpeg I/O threads: {error}")),
            true,
        ),
        Ok(()) => tokio::select! {
            result = &mut input_done_rx => (
                result.unwrap_or_else(|_| Err("stdin writer exited without a result".to_string())),
                false,
            ),
            _ = cancel.cancelled() => (Ok(()), true),
        },
    };
    (io_threads, input_result, cancelled)
}

fn emit_stage_stopped(ctx: &StageRunContext, ids: &StageIds<'_>) {
    ctx.engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id: ids.pipeline_id.to_string(),
            encoding: ids.encoding.to_string(),
        });
}

fn external_stage_arg_preset(plan: &FfmpegStagePlan, fallback: &str) -> String {
    match &plan.video {
        VideoStageOp::ScalePreset { preset } | VideoStageOp::Preview { preset } => preset.clone(),
        VideoStageOp::CodecEdge { .. } => "h264".to_string(),
        VideoStageOp::Passthrough => fallback.to_string(),
    }
}

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests;
