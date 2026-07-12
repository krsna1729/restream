//! External transcoder: shared pipeline stage using a subprocess FFmpeg.
//!
//! # Architecture
//!
//! The external transcoder is a **shared stage** in the media graph, not a
//! per-output process. One FFmpeg subprocess is spawned per (pipeline, preset)
//! pair. All egress outputs that request the same encoding on the same pipeline
//! read from the shared output ring buffer.
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
//! `source` encodings never enter the transcoder stage. Legacy `custom`
//! output rows also fall through as passthrough, but output create/update now
//! rejects new custom output encodings until custom FFmpeg args are applied.
//!
//! # Backend selection
//!
//! By default every non-passthrough encoding uses this external backend.
//! Individual stage families can be switched to the in-process FFmpeg backend
//! (`src/media/transcoder.rs`) via per-stage env flags:
//! `RESTREAM_INTERNAL_VIDEO_PRESETS`, `RESTREAM_INTERNAL_HEVC_TO_H264`,
//! `RESTREAM_INTERNAL_HLS_PREVIEW`, `RESTREAM_INTERNAL_AUDIO_COMPLEX`.
//! Prefer the external backend until the internal FFI layer reaches parity.

use std::process::Stdio;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::process::Command;
use tracing::{error, info};

use crate::media::ffmpeg::backend::{BackendError, StageRunContext};
use crate::media::ffmpeg::stage_input::StageInputPump;
use crate::media::ffmpeg::stage_output::StageOutputNormalizer;
use crate::media::ffmpeg::stage_plan::{FfmpegStagePlan, VideoStageOp};
use crate::media::mpegts::TsDemuxer;
use crate::media::pipe_metrics::PipeMetrics;

use crate::media::stage_lifecycle::{StageBackendKind, StageLifecycleGuard, StagePhase};
use crate::media::{MEDIA_PRODUCER_BATCH_PACKETS, MEDIA_TS_BATCH_TARGET_BYTES};

mod ffmpeg_process;
use ffmpeg_process::{ExternalStdinSink, spawn_external_stderr_logger};
pub use ffmpeg_process::{
    build_stage_ffmpeg_args, build_stage_ffmpeg_args_for_input,
    build_stage_ffmpeg_args_for_input_streams, build_stage_ffmpeg_video_only_args,
    build_stage_ffmpeg_video_only_args_for_input,
};

/// Stdin writes or stdout reads exceeding this threshold are counted as stalls/idles.
/// 1 ms filters normal async scheduling jitter while catching real back-pressure.
const PIPE_STALL_THRESHOLD_US: u64 = 1_000;

use crate::media::timing;

#[cfg(test)]
use crate::media::ring_buffer::MediaType;

#[cfg(test)]
fn external_output_stream_idx(
    media_type: MediaType,
    track_index: u32,
    audio_tracks: &[crate::media::engine::AudioMeta],
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
    mut output_normalizer: StageOutputNormalizer,
    ctx: StageRunContext,
) -> Result<(), BackendError> {
    let pipeline_id = ctx.pipeline_id.clone();
    let stage_key = ctx.stage_key.clone();
    let encoding = stage_key.kind.to_string();
    let lifecycle = ctx.lifecycle.clone();
    let _lifecycle_guard = StageLifecycleGuard::new(lifecycle.clone());
    let include_audio = plan.include_audio;
    let input_codec = plan.input.codec_hint.as_str();
    let output_codec = plan.output_codec.as_str();
    let input_codec_hint = input_pump.codec_hint();
    let probe_codec = match input_codec_hint.as_str() {
        "" => input_codec,
        hint => hint,
    };

    lifecycle.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });
    let _ffmpeg_permit = tokio::select! {
        permit = ctx.engine.runtime.external_ffmpeg_semaphore.acquire() => match permit {
            Ok(p) => p,
            Err(e) => {
                error!(
                    pipeline_id = %pipeline_id,
                    stage = %stage_key,
                    "external ffmpeg semaphore closed: {e}"
                );
                return Err(BackendError(e.to_string()));
            }
        },
        _ = ctx.cancel.cancelled() => {
            info!(
                pipeline_id = %pipeline_id,
                stage = %stage_key,
                "external ffmpeg wait cancelled"
            );
            return Ok(());
        }
    };
    lifecycle.transition(StagePhase::CapacityAcquired {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    let ffmpeg_preset = external_stage_arg_preset(&plan, &encoding);
    let args = build_stage_ffmpeg_args_for_input_streams(
        &ffmpeg_preset,
        output_codec,
        probe_codec,
        include_audio,
        plan.input.audio_tracks.len(),
    );
    info!(?args, "FFMPEG ARGS");
    let correlation_id = crate::logging::next_correlation_id("stage");

    info!(
        correlation_id = %correlation_id,
        pipeline_id = %pipeline_id,
        stage_encoding = %encoding,
        stage_backend = "external_ffmpeg",
        include_audio,
        "[ext-transcoder] stage start  pipeline={} encoding={}",
        pipeline_id,
        encoding
    );

    let ffmpeg_bin = crate::ffmpeg_extract::ffmpeg_bin_path();
    let mut child = match Command::new(ffmpeg_bin)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
    {
        Ok(c) => {
            lifecycle.transition(StagePhase::BackendSpawned {
                backend: StageBackendKind::ExternalFfmpeg,
                pid: c.id(),
            });
            c
        }
        Err(e) => {
            error!(
                correlation_id = %correlation_id,
                pipeline_id = %pipeline_id,
                stage_encoding = %encoding,
                stage_backend = "external_ffmpeg",
                "[ext-transcoder] failed to spawn ffmpeg ({}:{}): {}",
                pipeline_id,
                encoding,
                e
            );
            ctx.engine
                .runtime
                .event_log
                .emit(crate::events::EventKind::StageStopped {
                    pipeline_id: pipeline_id.clone(),
                    encoding: encoding.clone(),
                });
            return Err(BackendError(e.to_string()));
        }
    };

    let stdin = match child.stdin.take() {
        Some(s) => s,
        None => {
            error!(correlation_id=%correlation_id, pipeline_id=%pipeline_id, stage_encoding=%encoding, "[ext-transcoder] ffmpeg stdin unavailable");
            let _ = child.kill().await;
            let _ = child.wait().await;
            ctx.engine
                .runtime
                .event_log
                .emit(crate::events::EventKind::StageStopped {
                    pipeline_id: pipeline_id.clone(),
                    encoding: encoding.clone(),
                });
            return Err(BackendError("stdin unavailable".into()));
        }
    };
    let stdout = match child.stdout.take() {
        Some(s) => s,
        None => {
            error!(correlation_id=%correlation_id, pipeline_id=%pipeline_id, stage_encoding=%encoding, "[ext-transcoder] ffmpeg stdout unavailable");
            let _ = child.kill().await;
            let _ = child.wait().await;
            ctx.engine
                .runtime
                .event_log
                .emit(crate::events::EventKind::StageStopped {
                    pipeline_id: pipeline_id.clone(),
                    encoding: encoding.clone(),
                });
            return Err(BackendError("stdout unavailable".into()));
        }
    };
    let stderr = match child.stderr.take() {
        Some(s) => s,
        None => {
            error!(correlation_id=%correlation_id, pipeline_id=%pipeline_id, stage_encoding=%encoding, "[ext-transcoder] ffmpeg stderr unavailable");
            let _ = child.kill().await;
            let _ = child.wait().await;
            return Err(BackendError("stderr unavailable".into()));
        }
    };

    if !timing::calibrate() {
        info!(correlation_id=%correlation_id, pipeline_id=%pipeline_id, stage_encoding=%encoding, "[ext-transcoder] pipe timing: Instant fallback");
    }
    let timing_clock = timing::clock();
    let pipe_metrics = Arc::new(PipeMetrics::default());
    ctx.engine
        .register_pipe_metrics(stage_key.clone(), pipe_metrics.clone())
        .await;

    // stderr logger
    spawn_external_stderr_logger(
        stderr,
        format!("{}:{}", pipeline_id, encoding),
        correlation_id.clone(),
        pipeline_id.clone(),
        encoding.clone(),
    );

    // stdout demux task → output normalizer
    let cancel_out = ctx.cancel.clone();
    let out_pipe_metrics = pipe_metrics.clone();
    let out_timing_clock = timing_clock;
    tokio::spawn(async move {
        let mut stdout = stdout;
        let mut demuxer = TsDemuxer::new();
        let mut buf = vec![0u8; MEDIA_TS_BATCH_TARGET_BYTES];
        let mut pkts = Vec::with_capacity(MEDIA_PRODUCER_BATCH_PACKETS);
        loop {
            let t0 = out_timing_clock.now();
            let result = stdout.read(&mut buf).await;
            let idle_us = out_timing_clock.delta_us(t0);
            match result {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if idle_us > PIPE_STALL_THRESHOLD_US {
                        out_pipe_metrics.record_idle(idle_us);
                    }
                    demuxer.feed(&buf[..n]);
                    demuxer.drain_into(&mut pkts);
                    for pkt in pkts.drain(..) {
                        output_normalizer.push(pkt);
                    }
                }
            }
        }
        demuxer.flush();
        demuxer.drain_into(&mut pkts);
        for pkt in pkts.drain(..) {
            output_normalizer.push(pkt);
        }
        output_normalizer.mark_end_of_stream();
        cancel_out.cancel();
    });

    let mut input_pump = input_pump;

    let mut stdin_sink = ExternalStdinSink::new(stdin, pipe_metrics.clone(), timing_clock);
    if let Err(e) = input_pump.pump_to(&mut stdin_sink, &ctx.cancel).await {
        error!(
            correlation_id=%correlation_id,
            pipeline_id=%pipeline_id,
            stage_encoding=%encoding,
            "[ext-transcoder] input pump failed ({}:{}): {}",
            pipeline_id,
            encoding,
            e
        );
    }

    let _ = stdin_sink.stdin.shutdown().await;
    drop(stdin_sink);
    if tokio::time::timeout(std::time::Duration::from_secs(30), child.wait())
        .await
        .is_err()
    {
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    ctx.cancel.cancel();

    ctx.engine.remove_stage_metrics(&stage_key).await;
    ctx.engine.remove_pipe_metrics(&stage_key).await;
    ctx.engine.remove_stage_lifecycle(&stage_key).await;
    ctx.engine.remove_stage_runtime(&stage_key).await;
    ctx.engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id: pipeline_id.clone(),
            encoding: encoding.clone(),
        });

    info!(
        correlation_id = %correlation_id,
        pipeline_id = %pipeline_id,
        stage_encoding = %encoding,
        stage_backend = "external_ffmpeg",
        "[ext-transcoder] stage exit   pipeline={} encoding={}",
        pipeline_id,
        encoding
    );
    Ok(())
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
#[path = "external_transcoder_tests.rs"]
mod tests;
