//! First-class stage runtime scheduler.
//!
//! Centralizes stage creation, lifecycle registration, metrics, and backend
//! selection so that outputs, HLS preview, recording, and diagnostics all use
//! the same admission path. This is the runtime scheduler layer described in
//! `docs/architecture.md` and `docs/implementation.md`.

use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::domain::audio_routing::{AudioRouting, parse_audio_operation};
use crate::domain::stage::{StageKey, StageKind};
use crate::media::engine::{AudioMeta, MediaEngine, VideoMeta};
use crate::media::engine_registries::StageRuntime;
use crate::media::ffmpeg::backend::{
    ExternalFfmpegBackend, FfmpegStageBackend, InternalFfmpegBackend, StageRunContext,
};
use crate::media::ffmpeg::stage_input::StageInputPump;
use crate::media::ffmpeg::stage_output::StageOutputNormalizer;
use crate::media::ffmpeg::stage_plan::{
    AudioStageOp, CodecEdgeOp, FfmpegStagePlan, StageInputSpec, StageStartupPolicy, TimelinePolicy,
    VideoCodecKind, VideoStageOp,
};
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_lifecycle::{StageBackendKind, StageLifecycle, StagePhase};
use crate::media::stage_metrics::StageMetrics;
use crate::planner::backend_policy::{BackendPolicy, StageBackend};
use crate::runtime::stage::StageRuntimeSnapshot;

/// Handle to an admitted stage. Consumers read from `ring`; the runtime manager
/// owns the lifecycle, metrics, and cancellation token.
#[derive(Clone)]
pub struct StageHandle {
    pub key: StageKey,
    pub ring: Arc<RingBuffer>,
    pub cancel: CancellationToken,
    pub lifecycle: Arc<StageLifecycle>,
    pub metrics: Arc<StageMetrics>,
}

/// Runtime scheduler for media graph stages.
#[derive(Clone)]
pub struct StageRuntimeManager {
    engine: Arc<MediaEngine>,
    /// Backend selection policy, injected from `AppConfig` at construction time.
    /// Never re-reads env vars during execution.
    policy: BackendPolicy,
}

impl StageRuntimeManager {
    /// Create a manager using the engine's embedded `AppConfig` as the policy source.
    pub fn new(engine: Arc<MediaEngine>) -> Self {
        let policy = engine.backend_policy();
        Self { engine, policy }
    }

    /// Create a manager with an explicitly provided policy (useful in tests).
    pub fn with_policy(engine: Arc<MediaEngine>, policy: BackendPolicy) -> Self {
        Self { engine, policy }
    }

    /// Ensure a stage exists for the given key, creating its output ring,
    /// lifecycle, and metrics if absent. Returns the existing handle and `false`
    /// if the stage is already alive, or a freshly-created handle and `true`.
    ///
    /// `input_codec_override` is the codec hint of the *source* ring when it
    /// differs from the ingest codec (e.g. an HEVC source ring feeding a
    /// downstream H.264 stage).
    pub async fn ensure_stage(
        &self,
        key: StageKey,
        source_ring: Arc<RingBuffer>,
        input_codec_override: Option<&str>,
    ) -> (StageHandle, bool) {
        let backend_kind = backend_kind_for_stage(&key.kind, &self.policy);
        let lifecycle = self
            .engine
            .get_or_create_stage_lifecycle_with_backend(
                key.clone(),
                StagePhase::Registered,
                backend_kind,
            )
            .await;
        let metrics = self.engine.get_or_create_stage_metrics(key.clone()).await;

        // Single write-lock acquisition to atomically check-and-insert, avoiding
        // the TOCTOU race where two callers create duplicate stages.
        let mut runtimes = self.engine.stages.runtimes.write().await;
        if let Some(runtime) = runtimes.get(&key)
            && !runtime.cancel.is_cancelled()
            && let Some(ring) = runtime.ring.as_ref()
        {
            return (
                StageHandle {
                    key,
                    ring: ring.clone(),
                    cancel: runtime.cancel.clone(),
                    lifecycle: runtime.lifecycle.clone(),
                    metrics: runtime.metrics.clone(),
                },
                false,
            );
        }

        let output_ring = Arc::new(RingBuffer::new(self.engine.config.transcoder_ring_capacity));
        let cancel = CancellationToken::new();
        runtimes.insert(
            key.clone(),
            StageRuntime {
                ring: Some(output_ring.clone()),
                cancel: cancel.clone(),
                lifecycle: lifecycle.clone(),
                metrics: metrics.clone(),
                input_queue: None,
                pipe_metrics: None,
            },
        );
        drop(runtimes); // release write locks before any await-heavy setup

        self.initialize_stage_metadata(&key, &source_ring, input_codec_override, &output_ring)
            .await;

        info!(
            pipeline_id = %key.pipeline,
            stage = %key,
            "stage registered"
        );
        self.engine
            .runtime
            .event_log
            .emit(crate::events::EventKind::StageRegistered {
                pipeline_id: key.pipeline.to_string(),
                encoding: key.kind.to_string(),
            });

        (
            StageHandle {
                key,
                ring: output_ring,
                cancel,
                lifecycle,
                metrics,
            },
            true,
        )
    }

    /// Compute the backend policy choice for a stage without acquiring permits.
    pub fn select_backend(&self, kind: &StageKind) -> StageBackend {
        self.policy.select_backend(kind)
    }

    /// Spawn the selected backend for a freshly-created stage. This consumes the
    /// handle and starts the long-lived worker; existing stages must not be
    /// respawned.
    pub fn spawn_stage(
        &self,
        handle: StageHandle,
        source_ring: Arc<RingBuffer>,
        input_codec_override: Option<&str>,
    ) {
        self.spawn_ffmpeg(handle, source_ring, input_codec_override, true);
    }

    /// Spawn a codec-edge (HEVC→H.264) stage with audio passthrough.
    pub fn spawn_codec_edge_stage(&self, handle: StageHandle, source_ring: Arc<RingBuffer>) {
        self.spawn_ffmpeg(handle, source_ring, None, true);
    }

    /// Spawn a browser-preview stage that converts to H.264 video only.
    pub fn spawn_preview_stage(&self, handle: StageHandle, source_ring: Arc<RingBuffer>) {
        self.spawn_ffmpeg(handle, source_ring, None, false);
    }

    fn spawn_ffmpeg(
        &self,
        handle: StageHandle,
        source_ring: Arc<RingBuffer>,
        input_codec_override: Option<&str>,
        include_audio: bool,
    ) {
        let key = handle.key.clone();
        let backend = self.select_backend(&key.kind);

        if let Some(audio_op) = key.kind.audio_operation()
            && backend == StageBackend::AudioRouter
        {
            let pipeline_id = key.pipeline.to_string();
            let output_ring = handle.ring.clone();
            let engine = self.engine.clone();
            let cancel = handle.cancel.clone();
            let routing = parse_audio_operation(audio_op);
            info!(
                pipeline_id = %pipeline_id,
                stage = %key,
                "spawning audio-router stage"
            );
            tokio::spawn(async move {
                crate::media::transcoder::start_audio_router(
                    pipeline_id,
                    routing,
                    source_ring,
                    output_ring,
                    engine,
                    cancel,
                    key,
                )
                .await;
            });
            return;
        }

        let pipeline_id = key.pipeline.to_string();
        let output_ring = handle.ring.clone();
        let engine = self.engine.clone();
        let cancel = handle.cancel.clone();
        let lifecycle = handle.lifecycle.clone();
        let metrics = handle.metrics.clone();

        let input_codec_override = input_codec_override.map(|s| s.to_string());

        tokio::spawn(async move {
            let input_codec_str = input_codec_override.as_deref().unwrap_or("h264");
            let probe_codec = match source_ring.codec_hint_str() {
                "" => input_codec_str,
                hint => hint,
            };
            let eager_raw_parameter_sets = codec_needs_parameter_sets(probe_codec);

            let Some((video_meta, audio_tracks)) = wait_for_stage_metadata(
                &engine,
                &pipeline_id,
                &source_ring,
                include_audio,
                eager_raw_parameter_sets,
                input_codec_override.as_deref(),
                &cancel,
            )
            .await
            else {
                engine.remove_stage_metrics(&key).await;
                engine.remove_pipe_metrics(&key).await;
                engine.remove_stage_lifecycle(&key).await;
                engine.remove_stage_runtime(&key).await;
                return;
            };

            if include_audio {
                output_ring.set_audio_tracks((*audio_tracks).clone());
            }

            let Some(plan) = build_ffmpeg_stage_plan(
                &key,
                Some(video_meta.clone()),
                (*audio_tracks).clone(),
                input_codec_override.as_deref(),
                include_audio,
            ) else {
                tracing::warn!(stage = %key, "no ffmpeg plan for stage");
                return;
            };

            // Fetch the video sequence header from the engine's ingest state.
            // For RTMP/FLV sources this contains the AVCC SPS/PPS needed by
            // TsPacketFeeder; for TS/SRT sources it is typically None (the
            // feeder obtains raw annex-B parameter sets from the ring or
            // per-packet payload instead).
            let (video_seq_header, _) = engine.get_sequence_headers(&pipeline_id).await;

            let input_pump = StageInputPump::new(
                key.to_string(),
                source_ring.clone(),
                plan.startup.keyframe_preroll_packets,
                plan.input.video_meta.as_ref(),
                &plan.input.audio_tracks,
                include_audio,
                metrics.clone(),
            )
            .with_video_sequence_header(video_seq_header)
            .with_engine(engine.clone(), pipeline_id.clone())
            .with_lifecycle(lifecycle.clone());

            let stream_count = 1 + plan.input.audio_tracks.len();
            let output_normalizer =
                StageOutputNormalizer::new(output_ring, stream_count, metrics.clone())
                    .with_lifecycle(lifecycle.clone());

            let ctx = StageRunContext {
                stage_key: key.clone(),
                pipeline_id: pipeline_id.clone(),
                cancel: cancel.clone(),
                lifecycle,
                metrics,
                engine: engine.clone(),
            };

            match backend {
                StageBackend::InternalFfmpeg => {
                    info!(
                        pipeline_id = %pipeline_id,
                        stage = %key,
                        "spawning internal ffmpeg stage"
                    );
                    if let Err(e) = InternalFfmpegBackend
                        .run(plan, input_pump, output_normalizer, ctx)
                        .await
                    {
                        tracing::error!(stage = %key, error = %e, "internal ffmpeg stage failed");
                    }
                }
                StageBackend::ExternalFfmpeg => {
                    info!(
                        pipeline_id = %pipeline_id,
                        stage = %key,
                        "spawning external ffmpeg stage"
                    );
                    if let Err(e) = ExternalFfmpegBackend
                        .run(plan, input_pump, output_normalizer, ctx)
                        .await
                    {
                        tracing::error!(stage = %key, error = %e, "external ffmpeg stage failed");
                    }
                }
                StageBackend::AudioRouter
                | StageBackend::HlsSegmenter
                | StageBackend::Recording => {}
            }
        });
    }

    /// Return a runtime snapshot for a stage, if one is registered.
    pub async fn snapshot(&self, key: &StageKey) -> Option<StageRuntimeSnapshot> {
        self.engine.stage_runtime_snapshot(key).await
    }

    async fn initialize_stage_metadata(
        &self,
        key: &StageKey,
        source_ring: &Arc<RingBuffer>,
        input_codec_override: Option<&str>,
        output_ring: &Arc<RingBuffer>,
    ) {
        // Codec hint: video presets re-encode, so output is always H.264 unless
        // the source is HEVC and we are preserving it. Codec-edge stages always
        // emit H.264. Preview stages always emit H.264.
        if key.kind.is_video_preset() {
            output_ring.set_codec_hint(
                key.kind
                    .video_output_codec()
                    .or(input_codec_override)
                    .unwrap_or("h264"),
            );
        } else if key.kind.is_preview() || matches!(key.kind, StageKind::CodecEdge { .. }) {
            output_ring.set_codec_hint("h264");
        } else if let Some(oc) = input_codec_override {
            output_ring.set_codec_hint(oc);
        } else {
            let hint = source_ring.codec_hint_str();
            if !hint.is_empty() {
                output_ring.set_codec_hint(hint);
            }
        }

        // Audio track propagation. Only set when we have real data; empty
        // tracks would poison late-binding audio routers.
        let input_tracks = if let Some(tracks) = source_ring.audio_tracks() {
            std::sync::Arc::new(tracks.to_vec())
        } else {
            let ingests = self.engine.ingests.active.read().await;
            ingests
                .get(key.pipeline.as_str())
                .map(|i| {
                    let lock = i.audio_tracks.lock().unwrap_or_else(|e| e.into_inner());
                    if lock.is_empty()
                        && let Some(audio) = i.audio.clone()
                    {
                        std::sync::Arc::new(vec![audio])
                    } else {
                        std::sync::Arc::clone(&lock)
                    }
                })
                .unwrap_or_default()
        };

        // Preview stages are video-only — never propagate audio tracks.
        if key.kind.is_preview() {
            output_ring.set_audio_tracks(Vec::new());
            return;
        }

        if !input_tracks.is_empty() {
            let output_tracks = if let Some(audio_op) = key.kind.audio_operation() {
                let routing = parse_audio_operation(audio_op);
                crate::media::transcoder::apply_audio_routing(&routing, &input_tracks)
            } else {
                (*input_tracks).clone()
            };
            if !output_tracks.is_empty() {
                output_ring.set_audio_tracks(output_tracks);
            }
        }
    }
}

fn codec_needs_parameter_sets(codec: &str) -> bool {
    matches!(
        VideoCodecKind::from_codec_name(codec),
        VideoCodecKind::H264 | VideoCodecKind::Hevc
    )
}

fn stage_video_meta_ready(video: &VideoMeta) -> bool {
    !video.codec.is_empty()
        && (!codec_needs_parameter_sets(&video.codec) || (video.width > 0 && video.height > 0))
}

fn stage_audio_tracks_ready(audio_tracks: &[AudioMeta]) -> bool {
    !audio_tracks.is_empty()
        && audio_tracks
            .iter()
            .all(|track| track.sample_rate > 0 && track.channels > 0)
}

pub(crate) async fn wait_for_stage_metadata(
    engine: &Arc<MediaEngine>,
    pipeline_id: &str,
    source_buffer: &Arc<RingBuffer>,
    include_audio: bool,
    eager_raw_parameter_sets: bool,
    input_codec_override: Option<&str>,
    cancel: &CancellationToken,
) -> Option<(VideoMeta, std::sync::Arc<Vec<AudioMeta>>)> {
    loop {
        if cancel.is_cancelled() {
            return None;
        }

        let ingest_result = {
            let ingests = engine.ingests.active.read().await;
            ingests.get(pipeline_id).and_then(|ingest| {
                let mut video = ingest.video.clone()?;
                if let Some(codec) = input_codec_override {
                    video.codec = codec.to_string();
                } else {
                    let hint = source_buffer.codec_hint_str();
                    if !hint.is_empty() {
                        video.codec = hint.to_string();
                    }
                }
                if !stage_video_meta_ready(&video) {
                    return None;
                }
                let needs_raw_parameter_sets =
                    eager_raw_parameter_sets && codec_needs_parameter_sets(&video.codec);
                if needs_raw_parameter_sets {
                    // Accept parameter sets from either the ring buffer (annex-B
                    // for SRT/file-ingest sources), the engine's ingest video
                    // sequence header (AVCC for RTMP/FLV sources), or check if
                    // there are already packets present in the ring buffer (where
                    // SPS/PPS are sent in-band inside the media payloads, e.g. HEVC RTMP).
                    let ring_has_params = source_buffer.video_parameter_sets().is_some();
                    let engine_has_seq_header = ingest
                        .video_sequence_header
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .is_some();
                    let ring_has_packets = source_buffer.get_write_idx() > 0;
                    if !ring_has_params && !engine_has_seq_header && !ring_has_packets {
                        return None;
                    }
                }

                let audio_tracks = if include_audio {
                    let ingest_audio_tracks = {
                        let lock = ingest
                            .audio_tracks
                            .lock()
                            .unwrap_or_else(|e| e.into_inner());
                        if lock.is_empty() {
                            ingest
                                .audio
                                .clone()
                                .map(|audio| std::sync::Arc::new(vec![audio]))
                                .unwrap_or_default()
                        } else {
                            std::sync::Arc::clone(&lock)
                        }
                    };
                    source_buffer
                        .audio_tracks()
                        .filter(|tracks| !tracks.is_empty())
                        .map(|tracks| std::sync::Arc::new(tracks.to_vec()))
                        .filter(|tracks| stage_audio_tracks_ready(tracks))
                        .unwrap_or(ingest_audio_tracks)
                } else {
                    std::sync::Arc::new(Vec::new())
                };

                if include_audio && !stage_audio_tracks_ready(&audio_tracks) {
                    return None;
                }

                // Size the external probe from the stream Restream has already
                // demuxed, not from resolution/codec guesses. The source ring
                // normally contains at least one reconciler interval of media
                // by the time a stage is created; short/just-started streams
                // retain the conservative codec floor in startup_policy.
                if video.bw.is_none()
                    && let Some(observed_bitrate_bps) = source_buffer.observed_payload_bitrate_bps()
                {
                    video.bw = Some(observed_bitrate_bps as f64);
                }

                Some((video, audio_tracks))
            })
        };

        if let Some(meta) = ingest_result {
            return Some(meta);
        }

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

fn backend_kind_for_stage(kind: &StageKind, policy: &BackendPolicy) -> StageBackendKind {
    use crate::planner::backend_policy::StageBackend;
    match policy.select_backend(kind) {
        StageBackend::AudioRouter => StageBackendKind::AudioRouter,
        StageBackend::InternalFfmpeg => StageBackendKind::InternalFfmpeg,
        StageBackend::ExternalFfmpeg => StageBackendKind::ExternalFfmpeg,
        StageBackend::HlsSegmenter => StageBackendKind::HlsSegmenter,
        StageBackend::Recording => StageBackendKind::Recording,
    }
}

/// Build a backend-neutral FFmpeg plan from a stage key and source ring.
/// Returns `None` for stages that are not FFmpeg-backed (e.g. pure audio-router).
pub fn build_ffmpeg_stage_plan(
    key: &StageKey,
    video_meta: Option<VideoMeta>,
    audio_tracks: Vec<AudioMeta>,
    input_codec_override: Option<&str>,
    include_audio: bool,
) -> Option<FfmpegStagePlan> {
    let input_codec = input_codec_override
        .map(VideoCodecKind::from_codec_name)
        .unwrap_or_else(|| {
            let codec_name = video_meta.as_ref().map(|v| v.codec.as_str()).unwrap_or("");
            VideoCodecKind::from_codec_name(codec_name)
        });
    let input = StageInputSpec {
        codec_hint: input_codec.clone(),
        video_meta: video_meta.clone(),
        audio_tracks,
    };

    match &key.kind {
        StageKind::Source => Some(FfmpegStagePlan {
            stage_key: key.clone(),
            pipeline_id: key.pipeline.to_string(),
            input,
            video: VideoStageOp::Passthrough,
            audio: AudioStageOp::Passthrough,
            output_codec: input_codec,
            output_profile: None,
            include_audio,
            startup: StageStartupPolicy {
                keyframe_preroll_packets: 0,
                require_video_parameter_sets: true,
                wait_for_first_keyframe: true,
            },
            timeline: TimelinePolicy::default(),
        }),
        StageKind::VideoPreset { preset, .. } => Some(FfmpegStagePlan {
            stage_key: key.clone(),
            pipeline_id: key.pipeline.to_string(),
            input,
            video: VideoStageOp::ScalePreset {
                preset: preset.clone(),
            },
            audio: AudioStageOp::Passthrough,
            output_codec: input_codec,
            output_profile: None,
            include_audio,
            startup: StageStartupPolicy {
                keyframe_preroll_packets: 64,
                require_video_parameter_sets: true,
                wait_for_first_keyframe: true,
            },
            timeline: TimelinePolicy::default(),
        }),
        StageKind::CodecEdge { operation, .. } if operation == "hevc_to_h264" => {
            Some(FfmpegStagePlan {
                stage_key: key.clone(),
                pipeline_id: key.pipeline.to_string(),
                input,
                video: VideoStageOp::CodecEdge {
                    op: CodecEdgeOp::HevcToH264,
                },
                audio: AudioStageOp::Passthrough,
                output_codec: VideoCodecKind::H264,
                output_profile: None,
                include_audio,
                startup: StageStartupPolicy {
                    keyframe_preroll_packets: 128,
                    require_video_parameter_sets: true,
                    wait_for_first_keyframe: true,
                },
                timeline: TimelinePolicy::default(),
            })
        }
        StageKind::Preview { preset, .. } => {
            let preview_input = StageInputSpec {
                codec_hint: input_codec,
                video_meta,
                audio_tracks: Vec::new(),
            };
            Some(FfmpegStagePlan {
                stage_key: key.clone(),
                pipeline_id: key.pipeline.to_string(),
                input: preview_input,
                video: VideoStageOp::Preview {
                    preset: preset.clone(),
                },
                audio: AudioStageOp::Drop,
                output_codec: VideoCodecKind::H264,
                output_profile: None,
                include_audio: false,
                startup: StageStartupPolicy {
                    keyframe_preroll_packets: 128,
                    require_video_parameter_sets: true,
                    wait_for_first_keyframe: true,
                },
                timeline: TimelinePolicy::default(),
            })
        }
        StageKind::AudioRoute { operation, .. } => {
            let routing = parse_audio_operation(operation);
            let audio_op = audio_stage_op_from_routing(&routing)?;
            Some(FfmpegStagePlan {
                stage_key: key.clone(),
                pipeline_id: key.pipeline.to_string(),
                input,
                video: VideoStageOp::Passthrough,
                audio: audio_op,
                output_codec: input_codec,
                output_profile: None,
                include_audio,
                startup: StageStartupPolicy {
                    keyframe_preroll_packets: 0,
                    require_video_parameter_sets: false,
                    wait_for_first_keyframe: false,
                },
                timeline: TimelinePolicy::default(),
            })
        }
        _ => None,
    }
}

fn audio_stage_op_from_routing(routing: &AudioRouting) -> Option<AudioStageOp> {
    match routing {
        AudioRouting::Passthrough => Some(AudioStageOp::Passthrough),
        AudioRouting::SelectTracks { tracks } => Some(AudioStageOp::SelectTracks(tracks.clone())),
        AudioRouting::Downmix { track } => Some(AudioStageOp::Downmix { track: *track }),
        AudioRouting::Remap { track, left, right } => Some(AudioStageOp::Remap {
            track: *track,
            channels: vec![*left, *right],
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::stage::StageKind;
    use std::sync::Arc;

    #[tokio::test]
    async fn ensure_stage_creates_ring_and_returns_existing_on_reuse() {
        let engine = Arc::new(MediaEngine::new());
        let manager = StageRuntimeManager::new(engine.clone());
        let source = Arc::new(RingBuffer::new(16));
        let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));

        let (handle1, created1) = manager
            .ensure_stage(key.clone(), source.clone(), None)
            .await;
        assert!(created1);
        assert_eq!(handle1.ring.codec_hint_str(), "h264");

        let (handle2, created2) = manager
            .ensure_stage(key.clone(), source.clone(), None)
            .await;
        assert!(!created2);
        assert!(Arc::ptr_eq(&handle1.ring, &handle2.ring));
        let runtimes = engine.stages.runtimes.read().await;
        let runtime = runtimes.get(&key).expect("stage runtime registered");
        let runtime_ring = runtime.ring.as_ref().expect("ring-backed runtime");
        assert!(Arc::ptr_eq(runtime_ring, &handle1.ring));
        handle1.cancel.cancel();
        assert!(
            runtime.cancel.is_cancelled(),
            "runtime and handle must share one cancellation token"
        );
        assert!(Arc::ptr_eq(&runtime.lifecycle, &handle1.lifecycle));
        assert!(Arc::ptr_eq(&runtime.metrics, &handle1.metrics));
    }

    #[tokio::test]
    async fn ensure_stage_replaces_cancelled_runtime() {
        let engine = Arc::new(MediaEngine::new());
        let manager = StageRuntimeManager::new(engine.clone());
        let source = Arc::new(RingBuffer::new(16));
        let key = StageKey::new("pipe-replace", StageKind::video_preset("720p"));

        let (handle1, created1) = manager
            .ensure_stage(key.clone(), source.clone(), None)
            .await;
        assert!(created1);
        handle1.cancel.cancel();

        let (handle2, created2) = manager
            .ensure_stage(key.clone(), source.clone(), None)
            .await;

        assert!(created2, "cancelled runtime should be replaced");
        assert!(!Arc::ptr_eq(&handle1.ring, &handle2.ring));
        assert!(!handle2.cancel.is_cancelled());
        let runtimes = engine.stages.runtimes.read().await;
        let runtime = runtimes.get(&key).expect("replacement runtime registered");
        let runtime_ring = runtime.ring.as_ref().expect("ring-backed runtime");
        assert!(Arc::ptr_eq(runtime_ring, &handle2.ring));
        assert!(!runtime.cancel.is_cancelled());
    }

    #[tokio::test]
    async fn ensure_stage_uses_engine_typed_transcoder_ring_capacity() {
        let config = Arc::new(crate::AppConfig {
            transcoder_ring_capacity: 768,
            ..Default::default()
        });
        let engine = Arc::new(MediaEngine::new_with_config(config));
        let manager = StageRuntimeManager::new(engine);
        let source = Arc::new(RingBuffer::new(16));
        let key = StageKey::new("pipe-typed", StageKind::video_preset("720p"));

        let (handle, created) = manager.ensure_stage(key, source, None).await;
        assert!(created);
        assert_eq!(handle.ring.capacity(), 768);
    }

    #[test]
    fn codec_edge_plan_can_passthrough_audio_tracks() {
        let key = StageKey::new(
            "pipe-codec-edge",
            StageKind::codec_edge("hevc_to_h264", StageKind::source()),
        );
        let video = VideoMeta {
            codec: "hevc".to_string(),
            width: 1920,
            height: 1080,
            fps: 30.0,
            bw: None,
            pid: None,
            language: None,
            title: None,
            profile: None,
            level: None,
            pixel_format: None,
        };
        let audio = AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: None,
            language: None,
            title: None,
            profile: None,
        };

        let plan = build_ffmpeg_stage_plan(&key, Some(video), vec![audio], None, true)
            .expect("codec edge plan");

        assert!(plan.include_audio);
        assert_eq!(plan.input.audio_tracks.len(), 1);
        assert!(matches!(
            plan.video,
            VideoStageOp::CodecEdge {
                op: CodecEdgeOp::HevcToH264
            }
        ));
        assert!(matches!(plan.audio, AudioStageOp::Passthrough));
    }

    #[test]
    fn external_and_internal_stage_plan_share_operation() {
        let key = StageKey::new("pipe-shared-plan", StageKind::video_preset("720p"));
        let video = VideoMeta {
            codec: "h264".to_string(),
            width: 1920,
            height: 1080,
            fps: 30.0,
            bw: None,
            pid: None,
            language: None,
            title: None,
            profile: None,
            level: None,
            pixel_format: None,
        };
        let audio = AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: None,
            track_index: 0,
            pid: None,
            language: None,
            title: None,
            profile: None,
        };

        let plan = build_ffmpeg_stage_plan(&key, Some(video), vec![audio], None, true)
            .expect("video preset plan");

        assert!(matches!(
            plan.video,
            VideoStageOp::ScalePreset { ref preset } if preset == "720p"
        ));
        assert!(matches!(plan.audio, AudioStageOp::Passthrough));
        assert_eq!(plan.output_codec, VideoCodecKind::H264);
        assert!(
            plan.startup.wait_for_first_keyframe,
            "shared FFmpeg plan should carry startup policy for both backends"
        );
    }

    #[tokio::test]
    async fn snapshot_reflects_lifecycle_and_metrics() {
        let engine = Arc::new(MediaEngine::new());
        let manager = StageRuntimeManager::new(engine.clone());
        let source = Arc::new(RingBuffer::new(16));
        let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));

        let (handle, _) = manager.ensure_stage(key.clone(), source, None).await;
        handle.metrics.record_in_batch(2, 1024);
        handle.metrics.record_out(512);
        handle.lifecycle.transition(StagePhase::WaitingForCapacity {
            backend: StageBackendKind::ExternalFfmpeg,
        });

        let snap = manager.snapshot(&key).await.expect("snapshot exists");
        assert_eq!(snap.key, key);
        assert_eq!(snap.bytes_in, 1024);
        assert_eq!(snap.bytes_out, 512);
        assert_eq!(snap.packets_in, 2);
        assert_eq!(snap.packets_out, 1);
        assert!(matches!(
            snap.phase,
            StagePhase::WaitingForCapacity {
                backend: StageBackendKind::ExternalFfmpeg,
            }
        ));
    }
}
