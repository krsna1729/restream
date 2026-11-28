//! First-class stage runtime scheduler.
//!
//! Centralizes stage creation, lifecycle registration, metrics, and backend
//! selection so that outputs, HLS preview, recording, and diagnostics all use
//! the same admission path. This is the runtime scheduler layer described in
//! `docs/architecture.md`.

use std::sync::Arc;
use std::time::Duration;

use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use tracing::info;

use crate::events::EventLog;
use crate::ffmpeg::backend::StageRunContext;
use crate::ffmpeg::stage_input::StageInputPump;
use crate::ffmpeg::stage_output::StageOutputNormalizer;
use crate::ffmpeg::stage_plan::{
    AudioStageOp, CodecEdgeOp, FfmpegStagePlan, StageInputSpec, StageStartupPolicy, TimelinePolicy,
    VideoCodecKind, VideoStageOp,
};
use crate::ingest_registry::IngestRegistry;
use crate::stage_lifecycle::StageBackendKind;
use crate::stage_registry::{StageHandle, StageLease, StageRegistry};
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_domain::domain::audio_routing::{AudioRouting, parse_audio_operation};
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_sync::RingBuffer;

/// Runtime scheduler for media graph stages. The engine builds it
/// (`stage_runtime()`) over its registries, event log and FFmpeg capacity.
#[derive(Clone)]
pub struct StageRuntimeManager {
    ingests: Arc<IngestRegistry>,
    stages: Arc<StageRegistry>,
    events: Arc<EventLog>,
    /// The engine-wide external FFmpeg slots.
    ffmpeg_permits: Arc<Semaphore>,
    transcoder_ring_capacity: usize,
}

impl StageRuntimeManager {
    pub(crate) fn new(
        ingests: Arc<IngestRegistry>,
        stages: Arc<StageRegistry>,
        events: Arc<EventLog>,
        ffmpeg_permits: Arc<Semaphore>,
        transcoder_ring_capacity: usize,
    ) -> Self {
        Self {
            ingests,
            stages,
            events,
            ffmpeg_permits,
            transcoder_ring_capacity,
        }
    }

    /// Ensure a stage exists for the given key, creating its output ring,
    /// lifecycle, and metrics if absent. Returns the handle, the caller's lease
    /// (the stage stops when its last lease drops), and whether this call
    /// created the stage (the caller then spawns it).
    ///
    /// `input_codec_override` is the codec hint of the *source* ring when it
    /// differs from the ingest codec (e.g. an HEVC source ring feeding a
    /// downstream H.264 stage).
    pub async fn ensure_stage(
        &self,
        key: StageKey,
        source_ring: Arc<RingBuffer>,
        input_codec_override: Option<&str>,
    ) -> (StageHandle, StageLease, bool) {
        let backend = restream_domain::planner::select_backend(&key.kind);
        let (handle, lease, created) = self
            .stages
            .admit_ring_stage(key, backend, self.transcoder_ring_capacity)
            .await;
        if created {
            let key = &handle.key;
            self.initialize_stage_metadata(key, &source_ring, input_codec_override, &handle.ring)
                .await;
            info!(
                pipeline_id = %key.pipeline,
                stage = %key,
                "stage registered"
            );
            self.events.emit(crate::events::EventKind::StageRegistered {
                pipeline_id: key.pipeline.to_string(),
                encoding: key.kind.to_string(),
            });
        }
        (handle, lease, created)
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

    fn spawn_ffmpeg(
        &self,
        handle: StageHandle,
        source_ring: Arc<RingBuffer>,
        input_codec_override: Option<&str>,
        include_audio: bool,
    ) {
        let key = handle.key.clone();
        let backend = restream_domain::planner::select_backend(&key.kind);

        if let Some(audio_op) = key.kind.audio_operation()
            && backend == StageBackendKind::AudioRouter
        {
            let pipeline_id = key.pipeline.to_string();
            let output_ring = handle.ring.clone();
            let cancel = handle.cancel.clone();
            let routing = parse_audio_operation(audio_op);
            info!(
                pipeline_id = %pipeline_id,
                stage = %key,
                "spawning audio-router stage"
            );
            let stage = crate::audio_router::AudioRouterStage {
                key: key.clone(),
                cancel: cancel.clone(),
                lifecycle: handle.lifecycle.clone(),
                metrics: handle.metrics.clone(),
                events: self.events.clone(),
            };
            let _ = crate::executor::spawn_with_class(
                crate::executor::MediaService {
                    class: crate::executor::MediaServiceClass::AudioRouter,
                    pipeline_id,
                    stage: key.to_string(),
                },
                cancel,
                async move {
                    crate::audio_router::start_audio_router(
                        stage,
                        routing,
                        source_ring,
                        output_ring,
                    )
                    .await;
                },
            );
            return;
        }

        let pipeline_id = key.pipeline.to_string();
        let output_ring = handle.ring.clone();
        let ingests = self.ingests.clone();
        let stages = self.stages.clone();
        let ffmpeg_permits = self.ffmpeg_permits.clone();
        let events = self.events.clone();
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
                &ingests,
                &pipeline_id,
                &source_ring,
                include_audio,
                eager_raw_parameter_sets,
                input_codec_override.as_deref(),
                &cancel,
            )
            .await
            else {
                // The stage ends here. Its entry (and metrics) go when its
                // consumers' leases drop; a cancelled entry is never reused.
                cancel.cancel();
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
                // The stage ends here, like a failed metadata wait above:
                // its consumers see it end and retry.
                cancel.cancel();
                return;
            };

            // Fetch the video sequence header from the ingest state.
            // For RTMP/FLV sources this contains the AVCC SPS/PPS needed by
            // TsPacketFeeder; for TS/SRT sources it is typically None (the
            // feeder obtains raw annex-B parameter sets from the ring or
            // per-packet payload instead).
            let (video_seq_header, _) =
                match crate::preview_ids::input_id_from_hls_preview_resource_id(&pipeline_id) {
                    Some(input_id) => ingests.get_input_sequence_headers(input_id).await,
                    None => ingests.get_sequence_headers(&pipeline_id).await,
                };

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
            .with_ingests(ingests, pipeline_id.clone())
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
                stages,
                ffmpeg_permits,
                events,
            };

            match backend {
                StageBackendKind::ExternalFfmpeg => {
                    info!(
                        pipeline_id = %pipeline_id,
                        stage = %key,
                        "spawning external ffmpeg stage"
                    );
                    if let Err(e) = crate::external_transcoder::run_external_ffmpeg_backend(
                        plan,
                        input_pump,
                        output_normalizer,
                        ctx,
                    )
                    .await
                    {
                        tracing::error!(stage = %key, error = %e, "external ffmpeg stage failed");
                    }
                }
                StageBackendKind::AudioRouter
                | StageBackendKind::HlsSegmenter
                | StageBackendKind::Recording => {}
            }
        });
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
            let ingests = self.ingests.active.read().await;
            ingests
                .get(key.pipeline.as_str())
                .map(|i| {
                    let metadata = i.metadata();
                    let lock = restream_sync::lock::lock(&i.audio_tracks);
                    if lock.is_empty()
                        && let Some(audio) = metadata.audio
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
                crate::audio_router::apply_audio_routing(&routing, &input_tracks)
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

/// How long a codec-edge/video stage waits for audio after video is ready
/// before starting as video-only. Incomplete tracks (present, zero channels)
/// keep waiting; only a truly empty track list takes this grace path.
pub(crate) const AUDIO_ABSENCE_GRACE: Duration = Duration::from_secs(1);

pub(crate) async fn wait_for_stage_metadata(
    ingests: &IngestRegistry,
    pipeline_id: &str,
    source_buffer: &Arc<RingBuffer>,
    include_audio: bool,
    eager_raw_parameter_sets: bool,
    input_codec_override: Option<&str>,
    cancel: &CancellationToken,
) -> Option<(VideoMeta, std::sync::Arc<Vec<AudioMeta>>)> {
    let mut audio_absent_deadline: Option<tokio::time::Instant> = None;
    loop {
        if cancel.is_cancelled() {
            return None;
        }

        let ingest_result = {
            let ingest = ingests.stage_source_ingest(pipeline_id).await;
            ingest.and_then(|ingest| {
                let metadata = ingest.metadata();
                let mut video = metadata.video?;
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
                    let engine_has_seq_header =
                        restream_sync::lock::lock(&ingest.video_sequence_header).is_some();
                    let ring_has_packets = source_buffer.get_write_idx() > 0;
                    if !ring_has_params && !engine_has_seq_header && !ring_has_packets {
                        return None;
                    }
                }

                let audio_tracks = if include_audio {
                    let ingest_audio_tracks = {
                        let lock = restream_sync::lock::lock(&ingest.audio_tracks);
                        if lock.is_empty() {
                            metadata
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

        if let Some((video, audio_tracks)) = ingest_result {
            if !include_audio || stage_audio_tracks_ready(&audio_tracks) {
                return Some((video, audio_tracks));
            }
            if audio_tracks.is_empty() {
                let deadline = *audio_absent_deadline
                    .get_or_insert_with(|| tokio::time::Instant::now() + AUDIO_ABSENCE_GRACE);
                if tokio::time::Instant::now() >= deadline {
                    info!(
                        pipeline_id = %pipeline_id,
                        "starting video-only after audio-absence grace"
                    );
                    return Some((video, audio_tracks));
                }
            } else {
                audio_absent_deadline = None;
            }
        } else {
            audio_absent_deadline = None;
        }

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
}

/// Build the FFmpeg stage plan from a stage key and source ring.
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
        StageKind::VideoPreset {
            preset,
            output_codec,
        } => {
            let output_codec = output_codec
                .as_deref()
                .map(VideoCodecKind::from_codec_name)
                .unwrap_or_else(|| input_codec.clone());
            Some(FfmpegStagePlan {
                stage_key: key.clone(),
                pipeline_id: key.pipeline.to_string(),
                input,
                video: VideoStageOp::ScalePreset {
                    preset: preset.clone(),
                },
                audio: AudioStageOp::Passthrough,
                output_codec,
                output_profile: None,
                include_audio,
                startup: StageStartupPolicy {
                    keyframe_preroll_packets: 64,
                    require_video_parameter_sets: true,
                    wait_for_first_keyframe: true,
                },
                timeline: TimelinePolicy::default(),
            })
        }
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
mod tests;
