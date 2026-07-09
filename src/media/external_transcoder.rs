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
use crate::media::ffmpeg::stage_plan::FfmpegStagePlan;
use crate::media::mpegts::TsDemuxer;
use crate::media::pipe_metrics::PipeMetrics;

use crate::media::stage_lifecycle::{StageBackendKind, StageLifecycleGuard, StagePhase};
use crate::media::{MEDIA_PRODUCER_BATCH_PACKETS, MEDIA_TS_BATCH_TARGET_BYTES};

mod ffmpeg_process;
use ffmpeg_process::{ExternalStdinSink, spawn_external_stderr_logger};
pub use ffmpeg_process::{
    build_stage_ffmpeg_args, build_stage_ffmpeg_args_for_input, build_stage_ffmpeg_video_only_args,
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

    let args = if include_audio {
        build_stage_ffmpeg_args_for_input(&encoding, output_codec, probe_codec)
    } else {
        build_stage_ffmpeg_video_only_args_for_input(&encoding, output_codec, probe_codec)
    };
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

// ── Tests ──────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::stage::{StageKey, StageKind};
    use crate::media::engine::{AudioMeta, MediaEngine};
    use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
    use crate::media::mpegts::TsDemuxer;
    use crate::media::ring_buffer::{DtsEnforcer, MediaType, Reader, RingBuffer};
    use crate::media::stage_runtime::wait_for_stage_metadata;
    use proptest::prelude::*;
    use proptest::test_runner::Config as ProptestConfig;
    use tokio_util::sync::CancellationToken;

    fn extract_2v16a_hevc_ts_sample_for_duration(seconds: u32) -> Vec<u8> {
        let ffmpeg = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
        let fixture = crate::test_fixtures::checked_in_fixture("media/colorbar-timer-2v16a.mp4")
            .expect("2v16a fixture should exist");
        let output = std::process::Command::new(ffmpeg)
            .args([
                "-v",
                "error",
                "-i",
                fixture.to_str().expect("utf-8 fixture path"),
                "-map",
                "0:v:1",
                "-map",
                "0:a",
                "-c:v",
                "copy",
                "-bsf:v",
                "hevc_mp4toannexb",
                "-c:a",
                "copy",
                "-t",
                &seconds.to_string(),
                "-f",
                "mpegts",
                "pipe:1",
            ])
            .output()
            .expect("spawn bundled ffmpeg for 2v16a HEVC sample extraction");
        assert!(
            output.status.success(),
            "ffmpeg 2v16a HEVC sample extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.stdout.is_empty(),
            "2v16a HEVC TS sample should not be empty"
        );
        output.stdout
    }

    fn extract_2v16a_hevc_ts_sample() -> Vec<u8> {
        extract_2v16a_hevc_ts_sample_for_duration(1)
    }

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

    fn assert_strict_video_dts<'a, I>(label: &str, packets: I)
    where
        I: IntoIterator<Item = &'a crate::media::ring_buffer::MediaPacket>,
    {
        let mut previous = None;
        let mut count = 0usize;
        for packet in packets
            .into_iter()
            .filter(|packet| packet.media_type == MediaType::Video)
        {
            if let Some(previous_dts) = previous {
                assert!(
                    packet.dts > previous_dts,
                    "{label} video DTS must be strictly increasing: {previous_dts} >= {}",
                    packet.dts
                );
            }
            previous = Some(packet.dts);
            count += 1;
        }
        assert!(count > 0, "{label} should include video packets");
    }

    fn test_audio_track(track_index: u32) -> AudioMeta {
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: Some("stereo".to_string()),
            track_index,
            pid: None,
            language: None,
            title: None,
            profile: None,
        }
    }

    #[test]
    fn external_output_stream_idx_routes_known_tracks_without_aliasing() {
        let audio_tracks = vec![
            test_audio_track(7),
            test_audio_track(2),
            test_audio_track(11),
        ];

        assert_eq!(
            external_output_stream_idx(MediaType::Video, 0, &audio_tracks, true),
            Some(0)
        );
        assert_eq!(
            external_output_stream_idx(MediaType::Audio, 7, &audio_tracks, true),
            Some(1)
        );
        assert_eq!(
            external_output_stream_idx(MediaType::Audio, 2, &audio_tracks, true),
            Some(2)
        );
        assert_eq!(
            external_output_stream_idx(MediaType::Audio, 11, &audio_tracks, true),
            Some(3)
        );
        assert_eq!(
            external_output_stream_idx(MediaType::Audio, 99, &audio_tracks, true),
            None
        );
        assert_eq!(
            external_output_stream_idx(MediaType::Audio, 7, &audio_tracks, false),
            None
        );
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn proptest_external_output_dts_routing_preserves_per_stream_monotonicity(
            track_set in proptest::collection::btree_set(0u32..64, 1..=6),
            events in proptest::collection::vec((0u8..4, 0usize..16, -10i64..40, -10i64..40), 1..160),
        ) {
            let audio_tracks = track_set
                .into_iter()
                .map(test_audio_track)
                .collect::<Vec<_>>();
            let mut enforcer = DtsEnforcer::new(1 + audio_tracks.len());
            let mut previous_by_stream = vec![None; 1 + audio_tracks.len()];

            for (kind, index_seed, pts, dts) in events {
                let (media_type, track_index, should_route) = match kind {
                    0 => (MediaType::Video, 0, true),
                    1 | 2 => {
                        let track = audio_tracks[index_seed % audio_tracks.len()].track_index;
                        (MediaType::Audio, track, true)
                    }
                    _ => (MediaType::Audio, 10_000 + index_seed as u32, false),
                };

                let stream_idx = external_output_stream_idx(
                    media_type,
                    track_index,
                    &audio_tracks,
                    true,
                );
                prop_assert_eq!(stream_idx.is_some(), should_route);

                if let Some(stream_idx) = stream_idx {
                    let (out_pts, out_dts) = enforcer.enforce(stream_idx, pts, dts);
                    prop_assert!(out_pts >= out_dts);
                    if let Some(previous) = previous_by_stream[stream_idx] {
                        prop_assert!(out_dts > previous);
                    }
                    previous_by_stream[stream_idx] = Some(out_dts);
                }
            }
        }
    }

    #[tokio::test]
    async fn stage_metadata_prefers_upstream_ring_tracks_and_codec_hint() {
        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-stage-meta", "stream-key", "srt")
            .await
            .unwrap();

        let ingest_audio = vec![
            crate::media::engine::AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48000,
                channels: 2,
                channel_layout: None,
                track_index: 0,
                pid: Some(0x101),
                language: None,
                title: None,
                profile: None,
            },
            crate::media::engine::AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48000,
                channels: 2,
                channel_layout: None,
                track_index: 1,
                pid: Some(0x102),
                language: None,
                title: None,
                profile: None,
            },
        ];
        engine
            .update_ingest_meta(
                "pipe-stage-meta",
                Some(crate::media::engine::VideoMeta {
                    codec: "hevc".to_string(),
                    width: 1920,
                    height: 1080,
                    fps: 30.0,
                    bw: None,
                    pid: Some(0x100),
                    language: None,
                    title: None,
                    profile: None,
                    level: None,
                    pixel_format: None,
                }),
                ingest_audio.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-stage-meta", ingest_audio)
            .await;

        let upstream_ring = Arc::new(RingBuffer::new(1024));
        upstream_ring.set_codec_hint("h264");
        upstream_ring.set_video_parameter_sets(vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80,
        ]);
        upstream_ring.set_audio_tracks(vec![crate::media::engine::AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: Some(0x101),
            language: None,
            title: None,
            profile: None,
        }]);

        let cancel = CancellationToken::new();
        let (video, audio_tracks) = wait_for_stage_metadata(
            &engine,
            "pipe-stage-meta",
            &upstream_ring,
            true,
            true,
            None,
            &cancel,
        )
        .await
        .expect("stage metadata");

        assert_eq!(video.codec, "h264");
        assert_eq!(audio_tracks.len(), 1);
        assert_eq!(audio_tracks[0].track_index, 0);
        assert_eq!(audio_tracks[0].pid, Some(0x101));
    }

    #[tokio::test]
    async fn stage_metadata_waits_for_complete_audio_tracks() {
        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-stage-audio-ready", "stream-key", "srt")
            .await
            .unwrap();

        engine
            .update_ingest_meta(
                "pipe-stage-audio-ready",
                Some(crate::media::engine::VideoMeta {
                    codec: "hevc".to_string(),
                    width: 1920,
                    height: 1080,
                    fps: 30.0,
                    bw: None,
                    pid: Some(0x100),
                    language: None,
                    title: None,
                    profile: None,
                    level: None,
                    pixel_format: None,
                }),
                Some(crate::media::engine::AudioMeta {
                    codec: "aac".to_string(),
                    sample_rate: 0,
                    channels: 0,
                    channel_layout: None,
                    track_index: 0,
                    pid: Some(0x101),
                    language: None,
                    title: None,
                    profile: None,
                }),
                None,
            )
            .await;

        let upstream_ring = Arc::new(RingBuffer::new(1024));
        upstream_ring.set_video_parameter_sets(vec![
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        ]);
        let cancel = CancellationToken::new();
        let engine_for_wait = engine.clone();
        let ring_for_wait = upstream_ring.clone();
        let cancel_for_wait = cancel.clone();
        let wait = tokio::spawn(async move {
            wait_for_stage_metadata(
                &engine_for_wait,
                "pipe-stage-audio-ready",
                &ring_for_wait,
                true,
                true,
                None,
                &cancel_for_wait,
            )
            .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !wait.is_finished(),
            "stage metadata should wait until audio sample rate and channels are known"
        );

        let ready_audio = crate::media::engine::AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: Some(0x101),
            language: None,
            title: None,
            profile: None,
        };
        engine
            .update_ingest_meta(
                "pipe-stage-audio-ready",
                None,
                Some(ready_audio.clone()),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-stage-audio-ready", vec![ready_audio.clone()])
            .await;

        let (video, audio_tracks) = wait
            .await
            .expect("wait task should join")
            .expect("stage metadata should become ready");
        assert_eq!(video.width, 1920);
        assert_eq!(audio_tracks.len(), 1);
        assert_eq!(audio_tracks[0].sample_rate, 48000);
        assert_eq!(audio_tracks[0].channels, 2);
    }

    #[tokio::test]
    async fn stage_metadata_waits_for_raw_parameter_sets_on_srt_inputs() {
        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-stage-params", "stream-key", "srt")
            .await
            .unwrap();

        let ready_audio = crate::media::engine::AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: Some(0x101),
            language: None,
            title: None,
            profile: None,
        };
        engine
            .update_ingest_meta(
                "pipe-stage-params",
                Some(crate::media::engine::VideoMeta {
                    codec: "hevc".to_string(),
                    width: 1920,
                    height: 1080,
                    fps: 30.0,
                    bw: None,
                    pid: Some(0x100),
                    language: None,
                    title: None,
                    profile: None,
                    level: None,
                    pixel_format: None,
                }),
                Some(ready_audio.clone()),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-stage-params", vec![ready_audio])
            .await;

        let upstream_ring = Arc::new(RingBuffer::new(1024));
        upstream_ring.set_codec_hint("hevc");
        let cancel = CancellationToken::new();
        let engine_for_wait = engine.clone();
        let ring_for_wait = upstream_ring.clone();
        let cancel_for_wait = cancel.clone();
        let wait = tokio::spawn(async move {
            wait_for_stage_metadata(
                &engine_for_wait,
                "pipe-stage-params",
                &ring_for_wait,
                true,
                true,
                None,
                &cancel_for_wait,
            )
            .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !wait.is_finished(),
            "stage metadata should wait until raw parameter sets are cached on the source ring"
        );

        upstream_ring.set_video_parameter_sets(vec![
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        ]);

        let (video, audio_tracks) = wait
            .await
            .expect("wait task should join")
            .expect("stage metadata should become ready");
        assert_eq!(video.codec, "hevc");
        assert_eq!(audio_tracks.len(), 1);
        assert_eq!(audio_tracks[0].track_index, 0);
    }

    #[tokio::test]
    async fn stage_metadata_waits_for_raw_parameter_sets_on_file_inputs() {
        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-stage-file-params", "stream-key", "file")
            .await
            .unwrap();

        let ready_audio = crate::media::engine::AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: Some(0x101),
            language: None,
            title: None,
            profile: None,
        };
        engine
            .update_ingest_meta(
                "pipe-stage-file-params",
                Some(crate::media::engine::VideoMeta {
                    codec: "h264".to_string(),
                    width: 1920,
                    height: 1080,
                    fps: 30.0,
                    bw: None,
                    pid: Some(0x100),
                    language: None,
                    title: None,
                    profile: None,
                    level: None,
                    pixel_format: None,
                }),
                Some(ready_audio.clone()),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-stage-file-params", vec![ready_audio])
            .await;

        let upstream_ring = Arc::new(RingBuffer::new(1024));
        upstream_ring.set_codec_hint("h264");
        let cancel = CancellationToken::new();

        let engine_for_wait = engine.clone();
        let ring_for_wait = upstream_ring.clone();
        let cancel_for_wait = cancel.clone();
        let wait = tokio::spawn(async move {
            wait_for_stage_metadata(
                &engine_for_wait,
                "pipe-stage-file-params",
                &ring_for_wait,
                true,
                true,
                None,
                &cancel_for_wait,
            )
            .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !wait.is_finished(),
            "file stage metadata should wait until raw parameter sets are cached on the source ring"
        );

        upstream_ring.set_video_parameter_sets(vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80,
        ]);

        let (video, audio_tracks) = wait
            .await
            .expect("wait task should join")
            .expect("stage metadata should become ready");

        assert_eq!(video.codec, "h264");
        assert_eq!(audio_tracks.len(), 1);
        assert_eq!(audio_tracks[0].track_index, 0);
    }

    #[tokio::test]
    async fn stage_metadata_requires_raw_parameter_sets_for_hevc_codec_edge_stages() {
        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-stage-codec-edge", "stream-key", "file")
            .await
            .unwrap();

        let ready_audio = crate::media::engine::AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: Some(0x101),
            language: None,
            title: None,
            profile: None,
        };
        engine
            .update_ingest_meta(
                "pipe-stage-codec-edge",
                Some(crate::media::engine::VideoMeta {
                    codec: "hevc".to_string(),
                    width: 1280,
                    height: 720,
                    fps: 30.0,
                    bw: None,
                    pid: Some(0x100),
                    language: None,
                    title: None,
                    profile: None,
                    level: None,
                    pixel_format: None,
                }),
                Some(ready_audio.clone()),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-stage-codec-edge", vec![ready_audio])
            .await;

        let upstream_ring = Arc::new(RingBuffer::new(1024));
        upstream_ring.set_codec_hint("hevc");
        let cancel = CancellationToken::new();
        let engine_for_wait = engine.clone();
        let ring_for_wait = upstream_ring.clone();
        let cancel_for_wait = cancel.clone();
        let wait = tokio::spawn(async move {
            wait_for_stage_metadata(
                &engine_for_wait,
                "pipe-stage-codec-edge",
                &ring_for_wait,
                true,
                true,
                None,
                &cancel_for_wait,
            )
            .await
        });

        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !wait.is_finished(),
            "HEVC codec-edge stages should wait until upstream raw parameter sets are cached"
        );

        assert!(
            crate::media::codec::annexb_parameter_sets(&[
                0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
            ])
            .is_none(),
            "partial HEVC parameter sets should be rejected before they reach the ring cache"
        );
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !wait.is_finished(),
            "HEVC codec-edge stages should keep waiting until VPS/SPS/PPS are all cached"
        );

        upstream_ring.set_video_parameter_sets(vec![
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        ]);

        let (video, audio_tracks) = wait
            .await
            .expect("wait task should join")
            .expect("codec-edge stage metadata should become ready once parameter sets exist");

        assert_eq!(video.codec, "hevc");
        assert_eq!(audio_tracks.len(), 1);
        assert_eq!(audio_tracks[0].track_index, 0);
    }

    #[tokio::test]
    async fn external_720p_stage_emits_live_packets_for_hevc_sample() {
        let ts_sample = extract_2v16a_hevc_ts_sample();
        let mut demuxer = TsDemuxer::new();
        let mut packets = Vec::new();
        for chunk in ts_sample.chunks(1316) {
            demuxer.feed(chunk);
            demuxer.drain_into(&mut packets);
        }
        demuxer.flush();
        demuxer.drain_into(&mut packets);

        let probe = demuxer.take_probe().expect("probe 2v16a HEVC sample");
        let video = probe.video.expect("sample should contain video");
        let audio_tracks = probe.audio_tracks;

        let _ = tracing_subscriber::fmt::try_init();
        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-ext-preview", "stream-key", "srt")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-ext-preview",
                Some(video),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-ext-preview", audio_tracks.clone())
            .await;

        let source_ring = Arc::new(RingBuffer::new(16_384));
        source_ring.set_codec_hint("hevc");
        source_ring.set_audio_tracks(audio_tracks);
        // Extract parameter sets from the pre-demuxed packets so the stage's
        // metadata wait loop can find them (required for HEVC).
        let found_ps = packets.iter().find_map(|p| {
            (p.media_type == MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&p.payload))
                .flatten()
        });
        if let Some(ps) = found_ps {
            source_ring.set_video_parameter_sets(ps);
        }
        let stage_key = StageKey::new(
            "pipe-ext-preview",
            StageKind::codec_edge("hevc_to_h264", StageKind::source()),
        );
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
        let (handle, is_new) = manager
            .ensure_stage(stage_key.clone(), source_ring.clone(), None)
            .await;
        assert!(is_new);
        let output_ring = handle.ring.clone();
        let mut reader = Reader::new_live("test_ext_720p_output".to_string(), output_ring);
        let cancel = handle.cancel.clone();

        manager.spawn_codec_edge_stage(handle, source_ring.clone());

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if source_ring
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < ready_deadline,
                "external 720p stage reader did not attach to the source ring in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        // Feed all input.  With 18 streams (2v + 16a) FFmpeg holds output
        // until stdin closes, so we cancel to send EOF and trigger a flush.
        source_ring.push_batch(packets.drain(..));
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        cancel.cancel();

        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut output_packets = Vec::new();
        loop {
            while let Ok(Some(packet)) = reader.pull() {
                output_packets.push(packet);
            }
            if output_packets
                .iter()
                .any(|p| p.media_type == MediaType::Video && p.is_keyframe)
            {
                break;
            }
            if tokio::time::Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "external 720p HEVC preview stage should emit video packets after close (got {} packets)",
            output_packets.len()
        );
        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
            "external 720p HEVC preview stage should emit a keyframe after close (got {} packets)",
            output_packets.len()
        );
    }

    #[tokio::test]
    #[ignore = "diagnostic: current live HEVC + 16 audio preview stage still stalls without EOF"]
    async fn chained_hevc_preview_stages_emit_live_h264_packets() {
        let ts_sample = extract_2v16a_hevc_ts_sample();
        let mut demuxer = TsDemuxer::new();
        let mut packets = Vec::new();
        for chunk in ts_sample.chunks(1316) {
            demuxer.feed(chunk);
            demuxer.drain_into(&mut packets);
        }
        demuxer.flush();
        demuxer.drain_into(&mut packets);

        let probe = demuxer.take_probe().expect("probe 2v16a HEVC sample");
        let video = probe.video.expect("sample should contain video");
        let audio_tracks = probe.audio_tracks;

        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-ext-preview-chain", "stream-key", "srt")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-ext-preview-chain",
                Some(video),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-ext-preview-chain", audio_tracks.clone())
            .await;

        let source_ring = engine
            .get_or_create_pipeline("pipe-ext-preview-chain")
            .await;
        source_ring.set_codec_hint("hevc");
        source_ring.set_audio_tracks(audio_tracks);

        let hevc_preview_upstream = engine
            .get_or_create_transcoder(
                "pipe-ext-preview-chain",
                StageKind::video_preset("1080p"),
                source_ring.clone(),
                Some("hevc"),
            )
            .await;
        let h264_preview_ring = engine
            .get_or_create_h264_transcoder(
                "pipe-ext-preview-chain",
                StageKind::video_preset("1080p"),
                hevc_preview_upstream.clone(),
            )
            .await;
        let mut hevc_reader = Reader::new_live(
            "test_ext_preview_chain_mid".to_string(),
            hevc_preview_upstream.clone(),
        );
        let mut reader = Reader::new_live(
            "test_ext_preview_chain_output".to_string(),
            h264_preview_ring.clone(),
        );

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
        loop {
            let source_attached = source_ring.reader_snapshots().iter().any(|snapshot| {
                snapshot
                    .name
                    .contains("ext-stage:pipe-ext-preview-chain:video:1080p")
            });
            let chained_attached =
                hevc_preview_upstream
                    .reader_snapshots()
                    .iter()
                    .any(|snapshot| {
                        snapshot
                            .name
                            .contains("ext-stage:pipe-ext-preview-chain:720p")
                    });
            if source_attached && chained_attached {
                break;
            }
            assert!(
                tokio::time::Instant::now() < ready_deadline,
                "preview chain readers did not both attach in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        source_ring.push_batch(packets.drain(..));
        let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(12);
        let mut hevc_packets = Vec::new();
        let mut output_packets = Vec::new();
        loop {
            while let Ok(Some(packet)) = hevc_reader.pull() {
                hevc_packets.push(packet);
            }
            while let Ok(Some(packet)) = reader.pull() {
                output_packets.push(packet);
            }
            if output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video)
            {
                break;
            }
            if tokio::time::Instant::now() >= output_deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        assert_eq!(
            h264_preview_ring.codec_hint_str(),
            "h264",
            "preview codec-edge ring should advertise H.264 output"
        );
        assert!(
            hevc_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "preview chain should first emit live HEVC packets from the 1080p stage"
        );
        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "chained HEVC preview stages should emit live video packets"
        );
        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
            "chained HEVC preview stages should emit a live keyframe"
        );
    }

    #[tokio::test]
    async fn external_720p_stage_emits_live_packets_for_h264_marker_fixture() {
        let path = crate::test_fixtures::av_marker_transport_fixture("h264", false)
            .expect("H.264 marker fixture");
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

        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-ext-h264-marker", "stream-key", "file")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-ext-h264-marker",
                Some(video),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-ext-h264-marker", audio_tracks.clone())
            .await;

        let source_ring = Arc::new(RingBuffer::new(16_384));
        source_ring.set_codec_hint("h264");
        source_ring.set_audio_tracks(audio_tracks);
        if let Some(parameter_sets) = packets.iter().find_map(|packet| {
            (packet.media_type == MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
                .flatten()
        }) {
            source_ring.set_video_parameter_sets(parameter_sets);
        }
        let stage_key = StageKey::new("pipe-ext-h264-marker", StageKind::video_preset("720p"));
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
        let (handle, is_new) = manager
            .ensure_stage(stage_key.clone(), source_ring.clone(), None)
            .await;
        assert!(is_new);
        let output_ring = handle.ring.clone();
        let mut reader = Reader::new_live("test_ext_h264_marker_output".to_string(), output_ring);
        let cancel = handle.cancel.clone();

        manager.spawn_stage(handle, source_ring.clone(), None);

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if source_ring
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < ready_deadline,
                "external H.264 marker stage reader did not attach in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        source_ring.push_batch(packets.drain(..));
        let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut output_packets = Vec::new();
        loop {
            while let Ok(Some(packet)) = reader.pull() {
                output_packets.push(packet);
            }
            if output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video)
            {
                break;
            }
            if tokio::time::Instant::now() >= output_deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        cancel.cancel();

        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "external H.264 marker stage should emit live video packets"
        );
        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
            "external H.264 marker stage should emit a live keyframe"
        );
        assert!(
            source_ring.video_parameter_sets().is_some(),
            "source ring should cache raw parameter sets for the marker fixture"
        );
        assert!(
            reader.current_ring().video_parameter_sets().is_some(),
            "external H.264 marker stage output ring should cache raw parameter sets"
        );
    }

    #[tokio::test]
    async fn external_1080p_stage_remuxes_marker_fixture_with_monotone_dts() {
        let path = crate::test_fixtures::av_marker_transport_fixture("h264", false)
            .expect("H.264 marker fixture");
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

        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-ext-h264-marker-1080p", "stream-key", "file")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-ext-h264-marker-1080p",
                Some(video.clone()),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-ext-h264-marker-1080p", audio_tracks.clone())
            .await;

        let source_ring = Arc::new(RingBuffer::new(16_384));
        source_ring.set_codec_hint("h264");
        source_ring.set_audio_tracks(audio_tracks.clone());
        if let Some(parameter_sets) = packets.iter().find_map(|packet| {
            (packet.media_type == MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
                .flatten()
        }) {
            source_ring.set_video_parameter_sets(parameter_sets);
        }
        let stage_key = StageKey::new(
            "pipe-ext-h264-marker-1080p",
            StageKind::video_preset("1080p"),
        );
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
        let (handle, is_new) = manager
            .ensure_stage(stage_key.clone(), source_ring.clone(), None)
            .await;
        assert!(is_new);
        let output_ring = handle.ring.clone();
        let mut reader =
            Reader::new_live("test_ext_h264_marker_1080p_output".to_string(), output_ring);
        let cancel = handle.cancel.clone();

        manager.spawn_stage(handle, source_ring.clone(), None);

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if source_ring
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < ready_deadline,
                "external H.264 marker 1080p stage reader did not attach in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        source_ring.push_batch(packets.drain(..));
        let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(8);
        let mut output_packets = Vec::new();
        loop {
            while let Ok(Some(packet)) = reader.pull() {
                output_packets.push(packet);
            }
            if output_packets
                .iter()
                .filter(|packet| packet.media_type == MediaType::Video)
                .count()
                >= 120
            {
                break;
            }
            if tokio::time::Instant::now() >= output_deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }
        cancel.cancel();

        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "external 1080p H.264 marker stage should emit live video packets"
        );

        let mut feeder = TsPacketFeeder::new(
            Some(&video),
            std::sync::Arc::new(audio_tracks),
            PacketFeedConfig::default(),
        );
        let mut ts_bytes = Vec::new();
        let mut packet_buf = Vec::new();
        for packet in &output_packets {
            packet_buf.clear();
            if feeder.extend_ts_for_packet(packet, &mut packet_buf) {
                ts_bytes.extend_from_slice(&packet_buf);
            }
        }
        assert_strict_video_dts(
            "stage output",
            output_packets.iter().map(std::sync::Arc::as_ref),
        );

        let mut remux_demuxer = TsDemuxer::new();
        let mut remuxed_packets = Vec::new();
        for chunk in ts_bytes.chunks(1316) {
            remux_demuxer.feed(chunk);
            remux_demuxer.drain_into(&mut remuxed_packets);
        }
        remux_demuxer.flush();
        remux_demuxer.drain_into(&mut remuxed_packets);
        assert_strict_video_dts("remuxed output", remuxed_packets.iter());
    }

    #[tokio::test]
    async fn external_720p_stage_emits_live_packets_for_single_audio_hevc_fixture() {
        let (video, audio_tracks, mut packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h265")
                .expect("single-audio HEVC fixture");

        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-ext-hevc-single-audio", "stream-key", "srt")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-ext-hevc-single-audio",
                Some(video),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-ext-hevc-single-audio", audio_tracks.clone())
            .await;

        let source_ring = Arc::new(RingBuffer::new(16_384));
        source_ring.set_codec_hint("hevc");
        source_ring.set_audio_tracks(audio_tracks);
        if let Some(parameter_sets) = packets.iter().find_map(|packet| {
            (packet.media_type == MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
                .flatten()
        }) {
            source_ring.set_video_parameter_sets(parameter_sets);
        }
        let stage_key = StageKey::new(
            "pipe-ext-hevc-single-audio",
            StageKind::codec_edge("hevc_to_h264", StageKind::source()),
        );
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
        let (handle, is_new) = manager
            .ensure_stage(stage_key.clone(), source_ring.clone(), None)
            .await;
        assert!(is_new);
        let output_ring = handle.ring.clone();
        let mut reader =
            Reader::new_live("test_ext_720p_single_audio_output".to_string(), output_ring);
        let cancel = handle.cancel.clone();

        manager.spawn_codec_edge_stage(handle, source_ring.clone());

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if source_ring
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < ready_deadline,
                "external 720p single-audio HEVC stage reader did not attach in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        source_ring.push_batch(packets.drain(..));
        let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
        let mut output_packets = Vec::new();
        loop {
            while let Ok(Some(packet)) = reader.pull() {
                output_packets.push(packet);
            }
            if output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video)
            {
                break;
            }
            if tokio::time::Instant::now() >= output_deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        cancel.cancel();

        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "external 720p single-audio HEVC stage should emit live video packets"
        );
        assert!(
            reader.current_ring().video_parameter_sets().is_some(),
            "external 720p HEVC stage output ring should cache raw parameter sets for chained stages"
        );
    }

    #[tokio::test]
    async fn external_h264_stage_emits_live_packets_for_single_audio_hevc_fixture() {
        let (video, audio_tracks, mut packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h265")
                .expect("single-audio HEVC fixture");

        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-ext-hevc-source-h264", "stream-key", "srt")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-ext-hevc-source-h264",
                Some(video),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-ext-hevc-source-h264", audio_tracks.clone())
            .await;

        let source_ring = Arc::new(RingBuffer::new(16_384));
        source_ring.set_codec_hint("hevc");
        source_ring.set_audio_tracks(audio_tracks);
        if let Some(parameter_sets) = packets.iter().find_map(|packet| {
            (packet.media_type == MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
                .flatten()
        }) {
            source_ring.set_video_parameter_sets(parameter_sets);
        }
        let stage_key = StageKey::new(
            "pipe-ext-hevc-source-h264",
            StageKind::codec_edge("hevc_to_h264", StageKind::source()),
        );
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
        let (handle, is_new) = manager
            .ensure_stage(stage_key.clone(), source_ring.clone(), None)
            .await;
        assert!(is_new);
        let output_ring = handle.ring.clone();
        let mut reader =
            Reader::new_live("test_ext_h264_single_audio_output".to_string(), output_ring);
        let cancel = handle.cancel.clone();

        manager.spawn_codec_edge_stage(handle, source_ring.clone());

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if source_ring
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < ready_deadline,
                "external source-h264 single-audio HEVC stage reader did not attach in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        source_ring.push_batch(packets.drain(..));
        let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
        let mut output_packets = Vec::new();
        loop {
            while let Ok(Some(packet)) = reader.pull() {
                output_packets.push(packet);
            }
            if output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video)
            {
                break;
            }
            if tokio::time::Instant::now() >= output_deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        cancel.cancel();

        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "external source-h264 single-audio HEVC stage should emit live video packets"
        );
    }

    #[tokio::test]
    async fn external_720p_stage_emits_video_for_prebuffered_single_audio_hevc_fixture() {
        let (video, audio_tracks, mut packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h265")
                .expect("single-audio HEVC fixture");
        let continuation = packets
            .iter()
            .rev()
            .take(96)
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect::<Vec<_>>();

        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-ext-hevc-prebuffered", "stream-key", "srt")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-ext-hevc-prebuffered",
                Some(video.clone()),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-ext-hevc-prebuffered", audio_tracks.clone())
            .await;

        let source_ring = Arc::new(RingBuffer::new(16_384));
        source_ring.set_codec_hint("hevc");
        source_ring.set_audio_tracks(audio_tracks.clone());
        if let Some(parameter_sets) = packets.iter().find_map(|packet| {
            (packet.media_type == MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&packet.payload))
                .flatten()
        }) {
            source_ring.set_video_parameter_sets(parameter_sets);
        }
        source_ring.push_batch(packets.drain(..));

        let stage_key = StageKey::new(
            "pipe-ext-hevc-prebuffered",
            StageKind::codec_edge("hevc_to_h264", StageKind::source()),
        );
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
        let (handle, is_new) = manager
            .ensure_stage(stage_key.clone(), source_ring.clone(), None)
            .await;
        assert!(is_new);
        let output_ring = handle.ring.clone();
        let mut reader =
            Reader::new_live("test_ext_720p_prebuffered_output".to_string(), output_ring);
        let cancel = handle.cancel.clone();

        manager.spawn_codec_edge_stage(handle, source_ring.clone());
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        source_ring.push_batch(continuation);

        let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(6);
        let mut output_packets = Vec::new();
        loop {
            while let Ok(Some(packet)) = reader.pull() {
                output_packets.push(packet);
            }
            if output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video)
            {
                break;
            }
            if tokio::time::Instant::now() >= output_deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        cancel.cancel();

        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "external 720p HEVC stage should emit video once a prebuffered join receives live continuation"
        );
    }

    #[tokio::test]
    async fn external_720p_stage_emits_live_packets_for_2v16a_hevc_with_longer_input() {
        let ts_sample = extract_2v16a_hevc_ts_sample_for_duration(5);
        let mut demuxer = TsDemuxer::new();
        let mut packets = Vec::new();
        for chunk in ts_sample.chunks(1316) {
            demuxer.feed(chunk);
            demuxer.drain_into(&mut packets);
        }
        demuxer.flush();
        demuxer.drain_into(&mut packets);

        let probe = demuxer
            .take_probe()
            .expect("probe longer 2v16a HEVC sample");
        let video = probe.video.expect("sample should contain video");
        let audio_tracks = probe.audio_tracks;

        let engine = Arc::new(MediaEngine::new());
        engine
            .try_register_ingest("pipe-ext-preview-long", "stream-key", "srt")
            .await
            .unwrap();
        engine
            .update_ingest_meta(
                "pipe-ext-preview-long",
                Some(video),
                audio_tracks.first().cloned(),
                None,
            )
            .await;
        engine
            .update_ingest_audio_tracks("pipe-ext-preview-long", audio_tracks.clone())
            .await;

        let source_ring = Arc::new(RingBuffer::new(32_768));
        source_ring.set_codec_hint("hevc");
        source_ring.set_audio_tracks(audio_tracks);
        // Extract parameter sets from the pre-demuxed packets so the stage's
        // metadata wait loop can find them (required for HEVC).
        if let Some(ps) = packets.iter().find_map(|p| {
            (p.media_type == MediaType::Video)
                .then(|| crate::media::codec::annexb_parameter_sets(&p.payload))
                .flatten()
        }) {
            source_ring.set_video_parameter_sets(ps);
        }
        let stage_key = StageKey::new(
            "pipe-ext-preview-long",
            StageKind::codec_edge("hevc_to_h264", StageKind::source()),
        );
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(engine);
        let (handle, is_new) = manager
            .ensure_stage(stage_key.clone(), source_ring.clone(), None)
            .await;
        assert!(is_new);
        let output_ring = handle.ring.clone();
        let mut reader = Reader::new_live("test_ext_720p_long_output".to_string(), output_ring);
        let cancel = handle.cancel.clone();

        manager.spawn_codec_edge_stage(handle, source_ring.clone());

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        loop {
            if source_ring
                .reader_snapshots()
                .iter()
                .any(|snapshot| snapshot.name.contains(&stage_key.to_string()))
            {
                break;
            }
            assert!(
                tokio::time::Instant::now() < ready_deadline,
                "external 720p long-input HEVC stage reader did not attach in time"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        // Feed all input; wait for the pump to drain the ring to stdin
        // before cancelling, otherwise FFmpeg gets EOF with no input data.
        source_ring.push_batch(packets.drain(..));
        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        cancel.cancel();

        // The 18-stream (2v + 16a) MPEG-TS muxer is slow to flush; 5 seconds
        // of input also takes longer to process.
        let output_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(20);
        let mut output_packets = Vec::new();
        loop {
            while let Ok(Some(packet)) = reader.pull() {
                output_packets.push(packet);
            }
            if output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video)
            {
                break;
            }
            if tokio::time::Instant::now() >= output_deadline {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        }

        assert!(
            output_packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "external 720p HEVC preview stage should emit live video packets with longer 2v16a input (got {} packets)",
            output_packets.len()
        );
    }

    #[test]
    fn feeder_remuxed_single_audio_hevc_fixture_decodes_as_ts_file() {
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

        assert!(
            !ts_bytes.is_empty(),
            "remuxed HEVC fixture should produce TS bytes"
        );

        let ts_path = write_temp_ts_artifact("hevc-feeder-remux", &ts_bytes);
        let output = std::process::Command::new("ffmpeg")
            .args([
                "-v",
                "error",
                "-i",
                ts_path.to_str().expect("utf-8 ts path"),
                "-f",
                "null",
                "-",
            ])
            .output()
            .expect("spawn ffmpeg decode check");

        assert!(
            output.status.success(),
            "ffmpeg should decode feeder-remuxed HEVC TS: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}
