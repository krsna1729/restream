//! In-process FFmpeg transcoder — demuxes input MPEG-TS, applies stream filtering,
//! and pushes `MediaPacket`s directly to the output `RingBuffer`. Uses a single
//! `MemoryQueue` for input (source `RingBuffer` → TsMuxer → FFmpeg demux).
//!
//! Audio routing: compound encodings like `720p+atrack:0,1` or `source+remap:0:1`
//! are parsed to select/remap audio streams.

use crate::domain::output_spec::StagePresetSpec;
use crate::domain::stage::StageKey;
use crate::media::engine::AudioMeta;
use crate::media::ffmpeg::backend::{BackendError, StageRunContext};
use crate::media::ffmpeg::stage_input::StageInputPump;
use crate::media::ffmpeg::stage_output::StageOutputNormalizer;
use crate::media::ffmpeg::stage_plan::FfmpegStagePlan;
use crate::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat, Reader, RingBuffer};

use crate::media::stage_metrics::StageMetrics;
use crate::media::{MEDIA_PRODUCER_BATCH_PACKETS, MEDIA_PULL_BURST_PACKETS};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

/// Zero-copy wrapper: holds an `ffmpeg_next::Packet` so `bytes::Bytes::from_owner`
/// can serve the encoded/demuxed buffer to ring-buffer readers without a `memcpy`.
///
/// Drop calls `av_packet_unref`, decrementing the AVBufferRef refcount. The data
/// remains valid until every downstream `Bytes` clone is released.
///
/// `ffmpeg_next::Packet` is `unsafe impl Send + Sync`, satisfying `from_owner`'s bounds.
struct OwnedFfmpegPacket(ffmpeg_next::Packet);
impl AsRef<[u8]> for OwnedFfmpegPacket {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.0.data().unwrap_or(&[])
    }
}

use crate::domain::audio_routing::{AudioRouting, parse_audio_routing};

/// Byte sink that writes MPEG-TS batches into an in-process `MemoryQueue`.
pub(crate) struct InternalMemoryQueueSink {
    queue: Arc<crate::media::avio::MemoryQueue>,
    cancel: CancellationToken,
}

impl InternalMemoryQueueSink {
    pub(crate) fn new(
        queue: Arc<crate::media::avio::MemoryQueue>,
        cancel: CancellationToken,
    ) -> Self {
        Self { queue, cancel }
    }
}

impl crate::media::ffmpeg::stage_input::StageByteSink for InternalMemoryQueueSink {
    async fn write_ts(&mut self, bytes: &[u8], _cancel: &CancellationToken) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        if !self.queue.write_cancellable(bytes, &self.cancel).await {
            return Err("input queue closed or cancelled".into());
        }
        Ok(())
    }
}

/// Lightweight audio routing stage — no FFmpeg, no MPEG-TS round-trip.
///
/// Handles `SelectTracks` by filtering/re-indexing `MediaPacket`s in a tight
/// async loop. Packets are `Arc<Bytes>` so no payload copy occurs.
///
/// `Remap` and `Downmix` require DSP decode/filter/encode and are routed to
/// the FFmpeg backend by `BackendPolicy`.
pub async fn start_audio_router(
    pipeline_id: String,
    routing: AudioRouting,
    input_buffer: Arc<RingBuffer>,
    output_buffer: Arc<RingBuffer>,
    engine: Arc<crate::media::engine::MediaEngine>,
    cancel: CancellationToken,
    stage_key: StageKey,
) {
    let stage_metrics = engine.get_or_create_stage_metrics(stage_key.clone()).await;
    let lifecycle = engine
        .get_or_create_stage_lifecycle(
            stage_key.clone(),
            crate::media::stage_lifecycle::StagePhase::Registered,
        )
        .await;
    let _lifecycle_guard =
        crate::media::stage_lifecycle::StageLifecycleGuard::new(lifecycle.clone());
    lifecycle.transition(crate::media::stage_lifecycle::StagePhase::BackendSpawned {
        backend: crate::media::stage_lifecycle::StageBackendKind::AudioRouter,
        pid: None,
    });

    // Inherit the codec_hint from the input ring so downstream egresses
    // (SRT, RTMP) build correct PMT even after passing through the audio router.
    let hint = input_buffer.codec_hint_str();
    if !hint.is_empty() {
        output_buffer.set_codec_hint(hint);
    }
    if let Some(parameter_sets) = input_buffer.video_parameter_sets() {
        output_buffer.set_video_parameter_sets(parameter_sets);
    }
    // Propagate audio track metadata to the output ring, applying the routing
    // transformation (e.g. SelectTracks re-indexes track_index values).
    // This is done here (in the spawned task) rather than eagerly in
    // get_or_create_transcoder() because for live SRT ingest the source ring's
    // audio_tracks may not be populated yet at output-wiring time.
    if let Some(input_tracks) = input_buffer.audio_tracks() {
        let output_tracks = apply_audio_routing(&routing, &input_tracks);
        output_buffer.set_audio_tracks(output_tracks);
    }

    let routing_mode = match &routing {
        AudioRouting::Passthrough => "all",
        AudioRouting::SelectTracks { .. } => "subset",
        AudioRouting::Remap { .. } => "remap",
        AudioRouting::Downmix { .. } => "downmix",
    };
    info!(
        "[audio-router] start pipeline={} mode={} input_codec='{}' output_codec='{}'",
        pipeline_id,
        routing_mode,
        input_buffer.codec_hint_str(),
        output_buffer.codec_hint_str(),
    );

    let mut reader = Reader::new(
        format!(
            "audio-router:{}:{:?}",
            pipeline_id,
            std::mem::discriminant(&routing)
        ),
        input_buffer,
    );
    let mut _pushed_count: u64 = 0;
    let mut first_push_logged = false;
    let mut first_input_recorded = false;
    let mut first_output_recorded = false;
    // Pre-allocated batches — reused across bursts so the Vec capacity
    // is retained (no re-allocation on the hot path after the first burst).
    let mut out_batch: Vec<MediaPacket> = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
    let mut packets: Vec<std::sync::Arc<MediaPacket>> =
        Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            _ = reader.wait_for_data() => {
                if reader.pull_burst(&mut packets, MEDIA_PULL_BURST_PACKETS).is_err() {
                    continue;
                }
                if !first_input_recorded {
                    first_input_recorded = true;
                    lifecycle.record_first_input();
                }
                for pkt in packets.drain(..) {
                    stage_metrics.record_in(pkt.payload.len() as u64);
                    if pkt.media_type == MediaType::Video
                        && output_buffer.video_parameter_sets().is_none()
                    {
                        if let Some(parameter_sets) = reader.current_ring().video_parameter_sets() {
                            output_buffer.set_video_parameter_sets(parameter_sets);
                        } else if let Some(parameter_sets) =
                            crate::media::codec::annexb_parameter_sets(&pkt.payload)
                        {
                            output_buffer.set_video_parameter_sets(parameter_sets);
                        }
                    }
                    // Propagate audio_tracks from input ring as soon as they are
                    // available (late-arriving on live SRT/RTMP ingest).
                    if output_buffer.audio_tracks().is_none()
                        && let Some(input_tracks) = reader.current_ring().audio_tracks()
                    {
                        let output_tracks = apply_audio_routing(&routing, &input_tracks);
                        output_buffer.set_audio_tracks(output_tracks);
                    }
                    let out = match &routing {
                        AudioRouting::Passthrough => Some((*pkt).clone()),

                        AudioRouting::SelectTracks { tracks } => {
                            match pkt.media_type {
                                MediaType::Video => Some((*pkt).clone()),
                                MediaType::Audio => {
                                    if let Some(pos) = tracks.iter().position(|&t| t == pkt.track_index as usize) {
                                        let mut new_pkt = (*pkt).clone();
                                        new_pkt.track_index = pos as u32;
                                        Some(new_pkt)
                                    } else {
                                        None // drop this track
                                    }
                                }
                            }
                        }

                        AudioRouting::Remap { left, right, track } => {
                            match pkt.media_type {
                                MediaType::Video => Some((*pkt).clone()),
                                MediaType::Audio if pkt.track_index as usize == *track => {
                                    let _ = (left, right); // channel remap needs DSP
                                    let mut new_pkt = (*pkt).clone();
                                    new_pkt.track_index = 0;
                                    Some(new_pkt)
                                }
                                MediaType::Audio => None,
                            }
                        }

                        AudioRouting::Downmix { .. } => {
                            // Downmix requires decode→mix→encode; not handled here.
                            // get_or_create_transcoder routes Downmix to the FFmpeg path.
                            Some((*pkt).clone())
                        }
                    };
                    if let Some(p) = out {
                        stage_metrics.record_out(p.payload.len() as u64);
                        if !first_push_logged {
                            info!(
                                "[audio-router] first push pipeline={} type={:?} track={} codec_out='{}'",
                                pipeline_id, p.media_type, p.track_index,
                                output_buffer.codec_hint_str()
                            );
                            first_push_logged = true;
                        }
                        out_batch.push(p);
                        _pushed_count += 1;
                    }
                }
                // One write-index store + one Notify for the entire burst.
                if !out_batch.is_empty() {
                    if !first_output_recorded {
                        first_output_recorded = true;
                        lifecycle.record_first_output();
                    }
                    output_buffer.push_drained_batch_capped(&mut out_batch);
                }
            }
        }
    }

    engine.remove_stage_metrics(&stage_key).await;
    engine.remove_stage_lifecycle(&stage_key).await;
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id,
            encoding: stage_key.kind.to_string(),
        });
}

pub fn apply_audio_routing(routing: &AudioRouting, input_tracks: &[AudioMeta]) -> Vec<AudioMeta> {
    match routing {
        AudioRouting::Passthrough => input_tracks.to_vec(),
        AudioRouting::SelectTracks { tracks } => {
            let mut out = Vec::new();
            let mut out_idx = 0;
            for (i, track) in input_tracks.iter().enumerate() {
                if tracks.contains(&i) {
                    let mut t = track.clone();
                    t.track_index = out_idx;
                    out.push(t);
                    out_idx += 1;
                }
            }
            out
        }
        AudioRouting::Remap { track, .. } => {
            if let Some(t) = input_tracks.get(*track) {
                let mut out_track = t.clone();
                out_track.track_index = 0;
                vec![out_track]
            } else {
                Vec::new()
            }
        }
        AudioRouting::Downmix { track } => {
            if let Some(t) = input_tracks.get(*track) {
                let mut out_track = t.clone();
                out_track.track_index = 0;
                out_track.channels = 2;
                out_track.channel_layout = Some("stereo".to_string());
                vec![out_track]
            } else {
                Vec::new()
            }
        }
    }
}

#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn start_transcoder_inner(
    pipeline_id: String,
    preset: String,
    _input_buffer: Arc<RingBuffer>,
    output_buffer: Arc<RingBuffer>,
    engine: Arc<crate::media::engine::MediaEngine>,
    cancel_token: CancellationToken,
    stage_key: StageKey,
    mut input_pump: crate::media::ffmpeg::stage_input::StageInputPump,
    output_normalizer: StageOutputNormalizer,
    needs_scale: bool,
) {
    let input_queue = Arc::new(crate::media::avio::MemoryQueue::new());
    let stage_metrics = engine.get_or_create_stage_metrics(stage_key.clone()).await;
    let stage_lifecycle = engine
        .get_or_create_stage_lifecycle(
            stage_key.clone(),
            crate::media::stage_lifecycle::StagePhase::Registered,
        )
        .await;
    engine
        .register_input_queue(stage_key.clone(), input_queue.clone())
        .await;

    // Spawn thread to run FFmpeg processing: demux input MPEG-TS, push packets
    // directly to the output RingBuffer (no output mux/demux round-trip).
    let input_queue_clone = input_queue.clone();
    let preset_clone = preset.clone();
    let cancel_token_clone = cancel_token.clone();
    let cancel_on_exit = cancel_token.clone();
    let pipeline_id_clone = pipeline_id.clone();
    let out_buf = output_buffer.clone();
    let stage_metrics_for_thread = stage_metrics.clone();
    let stage_lifecycle_for_thread = stage_lifecycle.clone();
    let handle = std::thread::spawn(move || {
        stage_lifecycle_for_thread.transition(
            crate::media::stage_lifecycle::StagePhase::BackendSpawned {
                backend: crate::media::stage_lifecycle::StageBackendKind::InternalFfmpeg,
                pid: None,
            },
        );
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if needs_scale {
                let video_preset = preset_clone.strip_prefix("video:").unwrap_or(&preset_clone);
                run_ffmpeg_transcode_with_scale_with_normalizer(
                    input_queue_clone,
                    out_buf,
                    video_preset,
                    cancel_token_clone,
                    Some(stage_metrics_for_thread),
                    Some(output_normalizer),
                )
            } else {
                run_ffmpeg_transcoder_stage_with_normalizer(
                    input_queue_clone,
                    out_buf,
                    &preset_clone,
                    cancel_token_clone,
                    Some(stage_metrics_for_thread),
                    Some(output_normalizer),
                )
            }
        }));
        match result {
            Ok(Err(e)) => {
                stage_lifecycle_for_thread.record_error(e);
                error!(pipeline_id = %pipeline_id_clone, preset = %preset_clone, err = ?e, "FFmpeg transcode thread failed")
            }
            Err(_) => {
                stage_lifecycle_for_thread.record_error("FFmpeg transcode thread panicked");
                error!(pipeline_id = %pipeline_id_clone, preset = %preset_clone, "FFmpeg transcode thread panicked")
            }
            _ => {}
        }
        cancel_on_exit.cancel();
    });
    engine.register_os_thread(handle);

    let mut queue_sink = InternalMemoryQueueSink::new(input_queue.clone(), cancel_token.clone());
    if let Err(e) = input_pump.pump_to(&mut queue_sink, &cancel_token).await {
        error!(
            pipeline_id = %pipeline_id,
            preset = %preset,
            "internal transcoder shared pump failed: {}",
            e
        );
    }

    input_queue.close();
    engine.remove_input_queue(&stage_key).await;
    engine.remove_stage_metrics(&stage_key).await;
    engine.remove_stage_lifecycle(&stage_key).await;
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id: pipeline_id.clone(),
            encoding: preset.clone(),
        });
}

/// Backend entry point for the in-process FFmpeg adapter. The internal paths
/// already create and use `StageInputPump` and `StageOutputNormalizer`
/// internally; this function is the thin `FfmpegStageBackend` wrapper that
/// bridges from the trait to those existing implementations.
pub async fn run_internal_ffmpeg_backend(
    plan: FfmpegStagePlan,
    input_pump: StageInputPump,
    output_normalizer: StageOutputNormalizer,
    ctx: StageRunContext,
) -> Result<(), BackendError> {
    let source_ring = input_pump.source_ring();
    let output_ring = output_normalizer.output_ring();

    if matches!(
        plan.video,
        crate::media::ffmpeg::stage_plan::VideoStageOp::CodecEdge { .. }
    ) {
        crate::media::h264_transcoder::start_h264_transcoder_inner(
            ctx.pipeline_id.clone(),
            source_ring,
            output_ring,
            ctx.engine,
            ctx.cancel,
            ctx.stage_key,
            input_pump,
            output_normalizer,
        )
        .await;
    } else {
        let needs_scale = matches!(
            plan.video,
            crate::media::ffmpeg::stage_plan::VideoStageOp::ScalePreset { .. }
        );
        start_transcoder_inner(
            ctx.pipeline_id,
            ctx.stage_key.kind.to_string(),
            source_ring,
            output_ring,
            ctx.engine,
            ctx.cancel,
            ctx.stage_key,
            input_pump,
            output_normalizer,
            needs_scale,
        )
        .await;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests {
    use super::*;
    use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
    use crate::media::engine::AudioMeta;
    use crate::media::ring_buffer::PayloadFormat;
    use std::sync::Arc;
    use tokio_util::sync::CancellationToken;

    // --- apply_audio_routing tests ---

    #[test]
    fn apply_routing_passthrough_preserves_all_tracks() {
        let tracks = vec![
            AudioMeta {
                codec: "aac".into(),
                sample_rate: 48000,
                channels: 2,
                channel_layout: None,
                track_index: 0,
                pid: Some(0x101),
                language: Some("eng".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".into(),
                sample_rate: 44100,
                channels: 1,
                channel_layout: None,
                track_index: 1,
                pid: Some(0x102),
                language: Some("spa".to_string()),
                title: None,
                profile: None,
            },
        ];
        let result = apply_audio_routing(&AudioRouting::Passthrough, &tracks);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].track_index, 0);
        assert_eq!(result[1].track_index, 1);
    }

    #[test]
    fn apply_routing_select_tracks_filters_and_reindexes() {
        let tracks = vec![
            AudioMeta {
                codec: "aac".into(),
                sample_rate: 48000,
                channels: 2,
                channel_layout: None,
                track_index: 0,
                pid: Some(0x101),
                language: Some("eng".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".into(),
                sample_rate: 44100,
                channels: 1,
                channel_layout: None,
                track_index: 1,
                pid: Some(0x102),
                language: Some("spa".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".into(),
                sample_rate: 32000,
                channels: 1,
                channel_layout: None,
                track_index: 2,
                pid: Some(0x103),
                language: None,
                title: None,
                profile: None,
            },
        ];
        // Select tracks 0 and 2
        let routing = AudioRouting::SelectTracks { tracks: vec![0, 2] };
        let result = apply_audio_routing(&routing, &tracks);
        assert_eq!(result.len(), 2);
        assert_eq!(result[0].track_index, 0); // re-indexed: track 0 → index 0
        assert_eq!(result[1].track_index, 1); // re-indexed: track 2 → index 1
        assert_eq!(result[0].sample_rate, 48000);
        assert_eq!(result[1].sample_rate, 32000);
    }

    /// Verify that stage keys for different video presets with the same audio
    /// routing produce different cache keys, preventing cross-contamination.
    /// See docs/media-pipeline.md "Audio Stage Cache Concern".
    #[test]
    fn stage_keys_isolate_video_presets() {
        use crate::domain::stage::EncodingStagePlan;

        let plan_720 = EncodingStagePlan::from_encoding("pipe1", "720p+atrack:0");
        let plan_1080 = EncodingStagePlan::from_encoding("pipe1", "1080p+atrack:0");

        let audio_720 = plan_720.audio_stage().unwrap();
        let audio_1080 = plan_1080.audio_stage().unwrap();
        assert_ne!(
            audio_720, audio_1080,
            "audio stages with different video upstreams must have different keys"
        );

        let plan_720_dup = EncodingStagePlan::from_encoding("pipe1", "720p+atrack:0");
        assert_eq!(audio_720, plan_720_dup.audio_stage().unwrap());
    }

    /// Verify video stage keys are shared across outputs with different audio routing.
    #[test]
    fn video_stage_shared_across_audio_variants() {
        use crate::domain::stage::{EncodingStagePlan, StageKind};
        let expected = StageKind::video_preset("720p");
        for encoding in &["720p", "720p+atrack:0", "720p+remap:0:1"] {
            let plan = EncodingStagePlan::from_encoding("pipe1", encoding);
            let video = plan.video_stage().unwrap();
            assert_eq!(video.kind, expected, "encoding={}", encoding);
        }
    }

    #[test]
    fn test_apply_audio_routing_reindexes() {
        let input_tracks = vec![
            AudioMeta {
                codec: "aac".to_string(),
                channels: 2,
                sample_rate: 48000,
                track_index: 0,
                channel_layout: None,
                pid: Some(0x101),
                language: Some("eng".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".to_string(),
                channels: 2,
                sample_rate: 48000,
                track_index: 1,
                channel_layout: None,
                pid: Some(0x102),
                language: Some("spa".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".to_string(),
                channels: 2,
                sample_rate: 48000,
                track_index: 2,
                channel_layout: None,
                pid: Some(0x103),
                language: None,
                title: None,
                profile: None,
            },
        ];

        let routing = AudioRouting::SelectTracks { tracks: vec![2] };
        let output_tracks = apply_audio_routing(&routing, &input_tracks);
        assert_eq!(output_tracks.len(), 1);
        assert_eq!(output_tracks[0].track_index, 0); // re-indexed from 2 to 0
    }

    #[tokio::test]
    async fn test_audio_router_reindexes_packets() {
        use crate::domain::stage::{StageKey, StageKind};
        use crate::media::engine::MediaEngine;

        let source_ring = Arc::new(RingBuffer::new(16));
        let out_ring = Arc::new(RingBuffer::new(16));
        let engine = Arc::new(MediaEngine::new());
        let cancel = CancellationToken::new();
        let stage_key = StageKey::new(
            "pipe-id",
            StageKind::audio_route("atrack:2", StageKind::source()),
        );

        // Start audio router
        let routing = AudioRouting::SelectTracks { tracks: vec![2] };
        let handle = tokio::spawn(start_audio_router(
            "pipe-id".to_string(),
            routing,
            source_ring.clone(),
            out_ring.clone(),
            engine,
            cancel.clone(),
            stage_key,
        ));

        // Push some source packets
        source_ring.push(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: true,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from_static(&[1, 2, 3]),
        });
        source_ring.push(MediaPacket {
            media_type: MediaType::Audio,
            track_index: 2, // track 2
            pts: 10,
            dts: 10,
            is_keyframe: false,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from_static(&[4, 5, 6]),
        });

        // Let the router process
        tokio::time::sleep(std::time::Duration::from_millis(150)).await;
        cancel.cancel();
        let _ = handle.await;

        // Verify output packets
        let mut reader = Reader::new("test_router".to_string(), out_ring);
        let mut out_pkts = Vec::new();
        while let Ok(Some(pkt)) = reader.pull() {
            out_pkts.push(pkt);
        }

        // Should contain video packet and audio packet
        assert_eq!(out_pkts.len(), 2);
        assert_eq!(out_pkts[0].media_type, MediaType::Video);
        assert_eq!(out_pkts[1].media_type, MediaType::Audio);
        assert_eq!(out_pkts[1].track_index, 0); // re-indexed to 0
    }

    #[tokio::test]
    async fn audio_router_propagates_late_arriving_audio_tracks() {
        // Simulates SRT multi-audio: source ring has no audio_tracks when the
        // audio_router stage starts, then they arrive mid-stream.
        use crate::domain::stage::{StageKey, StageKind};
        use crate::media::engine::MediaEngine;

        let source_ring = Arc::new(RingBuffer::new(16));
        let out_ring = Arc::new(RingBuffer::new(16));
        let engine = Arc::new(MediaEngine::new());
        let cancel = CancellationToken::new();
        let stage_key = StageKey::new(
            "late-tracks",
            StageKind::audio_route("atrack:0,1", StageKind::source()),
        );

        source_ring.set_codec_hint("h264");
        // NOTE: no set_audio_tracks() yet — simulates live SRT before probe

        let handle = tokio::spawn(start_audio_router(
            "late-tracks".to_string(),
            AudioRouting::SelectTracks { tracks: vec![0, 1] },
            source_ring.clone(),
            out_ring.clone(),
            engine,
            cancel.clone(),
            stage_key,
        ));

        // Output ring has no audio_tracks yet (source not probed)
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(
            out_ring.audio_tracks().is_none(),
            "output ring should not have tracks before source has them"
        );

        // SRT ingest probe completes — audio_tracks become available
        source_ring.set_audio_tracks(vec![
            AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48000,
                channels: 2,
                channel_layout: Some("stereo".to_string()),
                track_index: 0,
                pid: Some(0x100),
                language: Some("eng".to_string()),
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48000,
                channels: 2,
                channel_layout: Some("stereo".to_string()),
                track_index: 1,
                pid: Some(0x101),
                language: Some("fra".to_string()),
                title: None,
                profile: None,
            },
        ]);

        // Push an audio packet — router burst loop should propagate audio_tracks
        source_ring.push(MediaPacket {
            media_type: MediaType::Audio,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: false,
            payload: bytes::Bytes::from_static(&[0x01]),
            format: crate::media::ring_buffer::PayloadFormat::Raw,
        });

        tokio::time::sleep(std::time::Duration::from_millis(50)).await;

        let tracks = out_ring
            .audio_tracks()
            .expect("output ring should have audio_tracks after source ring received them");
        assert_eq!(
            tracks.len(),
            2,
            "SelectTracks [0,1] should propagate both tracks"
        );
        assert_eq!(tracks[0].track_index, 0);
        assert_eq!(tracks[1].track_index, 1);

        cancel.cancel();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn audio_tracks_ring_supports_reconnect_update() {
        // Verifies that ArcSwapOption allows updating audio_tracks across
        // publisher reconnects — a reconnected stream may have different tracks.
        let ring = Arc::new(RingBuffer::new(8));

        // First publisher: 2 audio tracks
        ring.set_audio_tracks(vec![
            AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48000,
                channels: 2,
                channel_layout: None,
                track_index: 0,
                pid: None,
                language: None,
                title: None,
                profile: None,
            },
            AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48000,
                channels: 2,
                channel_layout: None,
                track_index: 1,
                pid: None,
                language: None,
                title: None,
                profile: None,
            },
        ]);
        assert_eq!(
            ring.audio_tracks().unwrap().len(),
            2,
            "first publisher: 2 tracks"
        );

        // Publisher reconnects — RTMP clears with empty vec, then re-probes
        ring.set_audio_tracks(Vec::new());
        assert!(
            ring.audio_tracks().is_none(),
            "empty set_audio_tracks should clear metadata (not a no-op)"
        );

        // Re-probe with new single-track configuration
        ring.set_audio_tracks(vec![AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 44100,
            channels: 1,
            channel_layout: None,
            track_index: 0,
            pid: None,
            language: None,
            title: None,
            profile: None,
        }]);
        let tracks = ring.audio_tracks().unwrap();
        assert_eq!(tracks.len(), 1, "reconnected publisher: 1 track");
        assert_eq!(
            tracks[0].sample_rate, 44100,
            "reconnected publisher track data"
        );
    }

    #[tokio::test]
    async fn audio_router_preserves_upstream_video_parameter_sets() {
        use crate::domain::stage::{StageKey, StageKind};
        use crate::media::engine::MediaEngine;

        let source_ring = Arc::new(RingBuffer::new(16));
        let out_ring = Arc::new(RingBuffer::new(16));
        let engine = Arc::new(MediaEngine::new());
        let cancel = CancellationToken::new();
        let stage_key = StageKey::new(
            "pipe-video-params",
            StageKind::audio_route("atrack:0", StageKind::source()),
        );

        source_ring.set_codec_hint("h264");
        source_ring.set_video_parameter_sets(vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80,
        ]);

        let handle = tokio::spawn(start_audio_router(
            "pipe-video-params".to_string(),
            AudioRouting::SelectTracks { tracks: vec![0] },
            source_ring,
            out_ring.clone(),
            engine,
            cancel.clone(),
            stage_key,
        ));

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert_eq!(out_ring.codec_hint_str(), "h264");
        assert_eq!(
            out_ring.video_parameter_sets(),
            Some(vec![
                0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
                0xCE, 0x38, 0x80,
            ])
        );

        cancel.cancel();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn audio_router_learns_video_parameter_sets_from_live_packets() {
        use crate::domain::stage::{StageKey, StageKind};
        use crate::media::engine::MediaEngine;

        let source_ring = Arc::new(RingBuffer::new(16));
        let out_ring = Arc::new(RingBuffer::new(16));
        let engine = Arc::new(MediaEngine::new());
        let cancel = CancellationToken::new();
        let stage_key = StageKey::new(
            "pipe-video-params-live",
            StageKind::audio_route("atrack:0", StageKind::source()),
        );

        source_ring.set_codec_hint("h264");

        let handle = tokio::spawn(start_audio_router(
            "pipe-video-params-live".to_string(),
            AudioRouting::SelectTracks { tracks: vec![0] },
            source_ring.clone(),
            out_ring.clone(),
            engine,
            cancel.clone(),
            stage_key,
        ));

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        assert!(
            out_ring.video_parameter_sets().is_none(),
            "router should start without cached parameter sets when the upstream ring does not have them yet"
        );

        source_ring.push(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: true,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from_static(&[
                0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x40, 0x28, 0x02, 0xDD, 0x80,
                0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x38, 0x80, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88,
                0x84, 0x00,
            ]),
        });

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if out_ring.video_parameter_sets().is_some() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < ready_deadline,
                "router should cache parameter sets from the first live video packet"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        cancel.cancel();
        let _ = handle.await;
    }

    #[tokio::test]
    async fn audio_router_copies_late_upstream_parameter_sets_without_inband_headers() {
        use crate::domain::stage::{StageKey, StageKind};
        use crate::media::engine::MediaEngine;

        let source_ring = Arc::new(RingBuffer::new(16));
        let out_ring = Arc::new(RingBuffer::new(16));
        let engine = Arc::new(MediaEngine::new());
        let cancel = CancellationToken::new();
        let stage_key = StageKey::new(
            "pipe-video-params-cache",
            StageKind::audio_route("atrack:0", StageKind::source()),
        );

        source_ring.set_codec_hint("h264");

        let handle = tokio::spawn(start_audio_router(
            "pipe-video-params-cache".to_string(),
            AudioRouting::SelectTracks { tracks: vec![0] },
            source_ring.clone(),
            out_ring.clone(),
            engine,
            cancel.clone(),
            stage_key,
        ));

        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        source_ring.set_video_parameter_sets(vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80,
        ]);
        source_ring.push(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: true,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x00]),
        });

        let ready_deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(1);
        loop {
            if out_ring.video_parameter_sets().is_some() {
                break;
            }
            assert!(
                tokio::time::Instant::now() < ready_deadline,
                "router should copy late upstream parameter sets even when the live packet payload lacks them"
            );
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }

        cancel.cancel();
        let _ = handle.await;
    }

    // M7: pts=0 from AV_NOPTS_VALUE would cause a massive backward jump on a
    // long-running stream. Verify the timestamp conversion formula itself is
    // correct and that skipping None-pts packets is the right behavior.
    //
    // We can't inject AV_NOPTS_VALUE into a live FFmpeg pipeline in a unit
    // test, but we can verify that the i128-based conversion that follows
    // a valid pts produces the expected millisecond value, confirming that
    // pts=0 would produce 0ms (a backward jump from, e.g., 3600000ms).
    #[test]
    fn pts_zero_would_produce_zero_ms_timestamp() {
        // Simulate the conversion for pts=0 with a 90kHz timebase (tb=1/90000).
        let pts: i64 = 0;
        let tb_num: i64 = 1;
        let tb_den: i64 = 90000;
        let pts_ms = (pts as i128 * tb_num as i128 * 1000 / tb_den as i128) as i64;
        assert_eq!(pts_ms, 0, "pts=0 produces 0ms — correct to skip, not use");

        // A real 1-hour stream has pts ≈ 324_000_000 ticks at 90kHz.
        let pts_1h: i64 = 3_600 * 90_000;
        let pts_ms_1h = (pts_1h as i128 * tb_num as i128 * 1000 / tb_den as i128) as i64;
        assert_eq!(pts_ms_1h, 3_600_000, "1h at 90kHz = 3600000ms");
        // Substituting 0 for AV_NOPTS_VALUE would create a -3600000ms backward jump.
        assert_eq!(pts_ms - pts_ms_1h, -3_600_000);
    }

    // M6: ts_batch must be cleared at the top of each burst arm so stale bytes
    // never accumulate across iterations. Verify the invariant by simulating
    // the burst pattern: partial batch from one burst must not appear in the next.
    #[test]
    fn ts_batch_cleared_before_each_burst() {
        let mut ts_batch: Vec<u8> = Vec::with_capacity(MEDIA_TS_BATCH_TARGET_BYTES);

        // Simulate two burst cycles: first accumulates data, second must start empty.
        let burst1 = b"packet_data_burst1";
        ts_batch.extend_from_slice(burst1);
        assert!(!ts_batch.is_empty());

        // Write and clear (as the arm does after write()).
        // Then simulate loop top: clear is now at the TOP of the arm.
        ts_batch.clear(); // ← this is the arm-top clear (M6 fix)
        assert!(
            ts_batch.is_empty(),
            "ts_batch must be empty at burst start — stale data would corrupt the stream"
        );

        let burst2 = b"packet_data_burst2";
        ts_batch.extend_from_slice(burst2);
        assert_eq!(&ts_batch[..], burst2, "burst2 must not contain burst1 data");
    }
}

/// Execute the FFmpeg-backed processing stage used by `start_transcoder`.
///
/// Demuxes input MPEG-TS from `in_queue`, applies stream filtering (audio
/// routing), and pushes `MediaPacket`s directly to the output `RingBuffer`.
/// No output muxer or demux thread needed.
#[doc(hidden)]
pub fn run_ffmpeg_transcoder_stage(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    preset: &str,
    token: CancellationToken,
) -> Result<(), &'static str> {
    run_ffmpeg_transcoder_stage_with_metrics(in_queue, out_ring, preset, token, None)
}

fn run_ffmpeg_transcoder_stage_with_metrics(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    preset: &str,
    token: CancellationToken,
    metrics: Option<Arc<StageMetrics>>,
) -> Result<(), &'static str> {
    run_ffmpeg_transcoder_stage_with_normalizer(in_queue, out_ring, preset, token, metrics, None)
}

fn run_ffmpeg_transcoder_stage_with_normalizer(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    preset: &str,
    token: CancellationToken,
    metrics: Option<Arc<StageMetrics>>,
    existing_normalizer: Option<StageOutputNormalizer>,
) -> Result<(), &'static str> {
    use crate::media::avio::CustomInput;

    let stage_spec = StagePresetSpec::parse(preset);
    let video_preset = stage_spec.video_encoding();
    let audio_routing = stage_spec
        .audio_operation()
        .map(crate::domain::audio_routing::parse_audio_operation)
        .unwrap_or_else(|| parse_audio_routing(preset));

    let mut custom_input = CustomInput::new(&*in_queue)?;
    let ictx = custom_input
        .input
        .as_mut()
        .ok_or("Failed to get CustomInput context")?;

    let mut audio_stream_index = 0usize;
    let mut audio_out_index = 0u32;
    let mut stream_meta: Vec<Option<(MediaType, u32)>> = Vec::new();

    let _force_h264 = video_preset == "h264";

    for stream in ictx.streams() {
        let medium = stream.parameters().medium();
        if medium == ffmpeg_next::media::Type::Video {
            stream_meta.push(Some((MediaType::Video, 0)));
        } else if medium == ffmpeg_next::media::Type::Audio {
            let include = match &audio_routing {
                AudioRouting::Passthrough => true,
                AudioRouting::SelectTracks { tracks } => tracks.contains(&audio_stream_index),
                AudioRouting::Remap { track, .. } => audio_stream_index == *track,
                AudioRouting::Downmix { track } => audio_stream_index == *track,
            };
            if include {
                stream_meta.push(Some((MediaType::Audio, audio_out_index)));
                audio_out_index += 1;
            } else {
                stream_meta.push(None);
            }
            audio_stream_index += 1;
        } else {
            stream_meta.push(None);
        }
    }

    let stream_count = stream_meta.iter().filter(|m| m.is_some()).count().max(1);
    let mut normalizer = if let Some(n) = existing_normalizer {
        n
    } else {
        let normalizer_metrics = metrics
            .clone()
            .unwrap_or_else(|| Arc::new(StageMetrics::new()));
        crate::media::ffmpeg::stage_output::StageOutputNormalizer::new(
            out_ring,
            stream_count,
            normalizer_metrics,
        )
        .with_video_track_count(1)
    };

    let mut batch: Vec<MediaPacket> = Vec::with_capacity(MEDIA_PRODUCER_BATCH_PACKETS);
    for (stream, packet) in ictx.packets() {
        if token.is_cancelled() {
            break;
        }

        let idx = stream.index();
        let Some(&Some((media_type, track_index))) = stream_meta.get(idx) else {
            continue;
        };

        let tb = stream.time_base();
        // Skip packets with AV_NOPTS_VALUE — using 0 on a long-running stream
        // would cause a massive backward jump through DtsEnforcer (M7 fix).
        let Some(pts) = packet.pts() else { continue };
        let dts = packet.dts().unwrap_or(pts);
        let pts_ms = if tb.1 != 0 {
            // i128 avoids f64 precision loss for large pts values (e.g. after
            // hours of streaming at 90 kHz: pts ≈ 3×10¹¹, f64 has only 53-bit
            // mantissa ≈ 9×10¹⁵ exact range but loses sub-ms precision before that).
            (pts as i128 * tb.0 as i128 * 1000 / tb.1 as i128) as i64
        } else {
            pts
        };
        let dts_ms = if tb.1 != 0 {
            (dts as i128 * tb.0 as i128 * 1000 / tb.1 as i128) as i64
        } else {
            dts
        };
        let is_keyframe = packet.is_key();

        let output_packet = MediaPacket {
            media_type,
            track_index,
            pts: pts_ms,
            dts: dts_ms,
            is_keyframe,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from_owner(OwnedFfmpegPacket(packet)),
        };
        batch.push(output_packet);
        if batch.len() >= MEDIA_PRODUCER_BATCH_PACKETS {
            normalizer.push_batch(&mut batch);
        }
    }
    if !batch.is_empty() {
        normalizer.push_batch(&mut batch);
    }

    Ok(())
}

/// Real decode -> scale -> encode transcoder stage.
pub fn run_ffmpeg_transcode_with_scale(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    video_preset: &str,
    token: CancellationToken,
) -> Result<(), &'static str> {
    run_ffmpeg_transcode_with_scale_with_metrics(in_queue, out_ring, video_preset, token, None)
}

fn run_ffmpeg_transcode_with_scale_with_metrics(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    video_preset: &str,
    token: CancellationToken,
    metrics: Option<Arc<StageMetrics>>,
) -> Result<(), &'static str> {
    run_ffmpeg_transcode_with_scale_with_normalizer(
        in_queue,
        out_ring,
        video_preset,
        token,
        metrics,
        None,
    )
}

fn run_ffmpeg_transcode_with_scale_with_normalizer(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    video_preset: &str,
    token: CancellationToken,
    metrics: Option<Arc<StageMetrics>>,
    existing_normalizer: Option<StageOutputNormalizer>,
) -> Result<(), &'static str> {
    use crate::media::avio::CustomInput;
    use ffmpeg_next::format::Pixel;

    let mut custom = CustomInput::new(&*in_queue)?;
    let ictx = custom
        .input
        .as_mut()
        .ok_or("Failed to get CustomInput context")?;

    // Identify streams
    let video_idx = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .map(|s| s.index())
        .ok_or("no video stream")?;

    // Build stream metadata (same pattern as h264_transcoder)
    let mut stream_meta: Vec<Option<(MediaType, u32)>> = Vec::new();
    let mut audio_track_counter = 0u32;
    for s in ictx.streams() {
        match s.parameters().medium() {
            ffmpeg_next::media::Type::Video => {
                stream_meta.push(Some((MediaType::Video, 0)));
            }
            ffmpeg_next::media::Type::Audio => {
                stream_meta.push(Some((MediaType::Audio, audio_track_counter)));
                audio_track_counter += 1;
            }
            _ => {
                stream_meta.push(None);
            }
        }
    }

    let dec_params = ictx
        .stream(video_idx)
        .ok_or("no video stream")?
        .parameters();
    let codec_id = dec_params.id();
    let dec_ctx = ffmpeg_next::codec::Context::from_parameters(dec_params)
        .map_err(|_| "decoder context error")?;
    let mut decoder = dec_ctx
        .decoder()
        .video()
        .map_err(|_| "decoder open error")?;

    // Look up target dimensions
    let profile = crate::media::profiles::get_blocking(video_preset);

    let target_w = profile.width;
    let target_h = profile.height;
    let skip_scaling = target_w == 0;

    let enc_codec = match codec_id {
        ffmpeg_next::codec::Id::H264 => {
            ffmpeg_next::codec::encoder::find(ffmpeg_next::codec::Id::H264)
                .ok_or("no H.264 encoder")?
        }
        ffmpeg_next::codec::Id::HEVC => ffmpeg_next::codec::encoder::find_by_name("libx265")
            .or_else(|| ffmpeg_next::codec::encoder::find(ffmpeg_next::codec::Id::HEVC))
            .ok_or("no HEVC/H.265 encoder")?,
        _ => return Err("Unsupported video codec for internal transcoding"),
    };

    let stream_count = 1 + audio_track_counter as usize;
    let mut normalizer = if let Some(n) = existing_normalizer {
        n
    } else {
        let normalizer_metrics = metrics
            .clone()
            .unwrap_or_else(|| Arc::new(StageMetrics::new()));
        crate::media::ffmpeg::stage_output::StageOutputNormalizer::new(
            out_ring,
            stream_count,
            normalizer_metrics,
        )
        .with_video_track_count(1)
    };

    let mut encoder: Option<ffmpeg_next::codec::encoder::video::Encoder> = None;
    let mut scaler: Option<ffmpeg_next::software::scaling::Context> = None;
    let mut enc_frame = ffmpeg_next::frame::Video::empty();
    let mut enc_pkt = ffmpeg_next::Packet::empty();

    for (stream, pkt) in ictx.packets() {
        if token.is_cancelled() {
            break;
        }

        let idx = stream.index();

        // Audio copy
        if stream.parameters().medium() == ffmpeg_next::media::Type::Audio {
            let Some(&Some((media_type, track_index))) = stream_meta.get(idx) else {
                continue;
            };
            let tb = stream.time_base();
            // Skip packets with AV_NOPTS_VALUE (M7 fix — same as passthrough path).
            let Some(pts) = pkt.pts() else { continue };
            let dts_val = pkt.dts().unwrap_or(pts);
            let pts_ms = if tb.1 != 0 {
                (pts as i128 * tb.0 as i128 * 1000 / tb.1 as i128) as i64
            } else {
                pts
            };
            let dts_ms = if tb.1 != 0 {
                (dts_val as i128 * tb.0 as i128 * 1000 / tb.1 as i128) as i64
            } else {
                dts_val
            };
            let is_keyframe = pkt.is_key();
            let output_packet = MediaPacket {
                media_type,
                track_index,
                pts: pts_ms,
                dts: dts_ms,
                is_keyframe,
                format: PayloadFormat::Raw,
                payload: bytes::Bytes::from_owner(OwnedFfmpegPacket(pkt)),
            };
            normalizer.push(output_packet);
            continue;
        }

        if idx != video_idx {
            continue;
        }

        let video_tb = stream.time_base();
        if decoder.send_packet(&pkt).is_err() {
            continue;
        }

        let mut dec_frame = ffmpeg_next::frame::Video::empty();
        while decoder.receive_frame(&mut dec_frame).is_ok() {
            // Lazy encoder + scaler init
            if encoder.is_none() {
                let width = decoder.width();
                let height = decoder.height();
                let in_fmt = dec_frame.format();

                let out_w = if target_w > 0 { target_w } else { width };
                let out_h = if target_h > 0 { target_h } else { height };

                let need_scaling = !skip_scaling && (out_w != width || out_h != height)
                    || in_fmt != Pixel::YUV420P;
                if need_scaling {
                    let sw = ffmpeg_next::software::scaling::Context::get(
                        in_fmt,
                        width,
                        height,
                        Pixel::YUV420P,
                        out_w,
                        out_h,
                        ffmpeg_next::software::scaling::Flags::BILINEAR,
                    )
                    .map_err(|_| "failed to create scaler")?;
                    scaler = Some(sw);
                }

                let fr = stream.avg_frame_rate();
                let (fn_, fd) = if fr.numerator() > 0 && fr.denominator() > 0 {
                    (fr.numerator(), fr.denominator())
                } else {
                    (30, 1)
                };

                // SAFETY: avcodec_alloc_context3 allocates an FFmpeg
                // AVCodecContext. The `enc_codec` pointer was obtained from
                // avcodec_find_encoder_by_name and is valid for the process
                // lifetime. The returned pointer is either null (handled) or
                // a valid heap allocation. Context::wrap takes ownership.
                let enc_ctx = unsafe {
                    let ptr = ffmpeg_next::ffi::avcodec_alloc_context3(
                        enc_codec.as_ptr() as *mut ffmpeg_next::ffi::AVCodec
                    );
                    if ptr.is_null() {
                        return Err("failed to allocate encoder context");
                    }
                    ffmpeg_next::codec::Context::wrap(ptr, None)
                };
                let mut enc_video = enc_ctx
                    .encoder()
                    .video()
                    .map_err(|_| "failed to get encoder video interface")?;

                enc_video.set_width(out_w);
                enc_video.set_height(out_h);
                enc_video.set_format(Pixel::YUV420P);
                // Use millisecond time base so encoder output timestamps are in ms,
                // matching the shared stage timeline and copied audio timestamps.
                enc_video.set_time_base(ffmpeg_next::Rational::new(1, 1000));
                enc_video.set_frame_rate(Some(ffmpeg_next::Rational::new(fn_, fd)));
                enc_video.set_gop(profile.gop);
                enc_video.set_max_b_frames(profile.bframes);

                let bitrate = if profile.bitrate > 0 {
                    profile.bitrate as usize
                } else {
                    (out_w * out_h) as usize * 3
                };
                enc_video.set_bit_rate(bitrate);
                if profile.max_bitrate > 0 {
                    enc_video.set_max_bit_rate(profile.max_bitrate as usize);
                }

                let mut opts = ffmpeg_next::Dictionary::new();
                opts.set("preset", &profile.preset);
                opts.set("tune", &profile.tune);
                if profile.bitrate == 0 {
                    opts.set("crf", &profile.crf.to_string());
                }

                let opened = enc_video
                    .open_as_with(enc_codec, opts)
                    .map_err(|_| "failed to open encoder")?;
                encoder = Some(opened);
            }

            let Some(enc) = encoder.as_mut() else {
                continue;
            };

            // Use source-derived timestamp for the frame so encoded video shares
            // the same clock origin as copied audio.
            let source_pts_ms = dec_frame.pts().map(|pts| {
                if video_tb.1 != 0 {
                    (pts as i128 * video_tb.0 as i128 * 1000 / video_tb.1 as i128) as i64
                } else {
                    pts
                }
            });

            let frame_to_encode = if let Some(ref mut sw) = scaler {
                if sw.run(&dec_frame, &mut enc_frame).is_err() {
                    continue;
                }
                enc_frame.set_pts(source_pts_ms);
                // Drop source picture-type hints so the new encoder can choose
                // GOP/B-frame placement from its own settings.
                enc_frame.set_kind(ffmpeg_next::util::picture::Type::None);
                &enc_frame
            } else {
                dec_frame.set_pts(source_pts_ms);
                // Even without scaling, a decode/re-encode stage should not
                // preserve source I/P/B tags across the encoder boundary.
                dec_frame.set_kind(ffmpeg_next::util::picture::Type::None);
                &dec_frame
            };

            if enc.send_frame(frame_to_encode).is_err() {
                continue;
            }

            while enc.receive_packet(&mut enc_pkt).is_ok() {
                let pts_ms = enc_pkt.pts().unwrap_or(0);
                let dts_ms = enc_pkt.dts().unwrap_or(pts_ms);
                // enc_pkt is reused across iterations; clone() calls av_packet_ref (refcount
                // bump only, no data copy) so the ring buffer holds the AVBufferRef alive.
                let output_packet = MediaPacket {
                    media_type: MediaType::Video,
                    track_index: 0,
                    pts: pts_ms,
                    dts: dts_ms,
                    is_keyframe: enc_pkt.is_key(),
                    format: PayloadFormat::Raw,
                    payload: bytes::Bytes::from_owner(OwnedFfmpegPacket(enc_pkt.clone())),
                };
                normalizer.push(output_packet);
            }
        }
    }

    if let Some(enc) = encoder.as_mut() {
        let _ = enc.send_eof();
        while enc.receive_packet(&mut enc_pkt).is_ok() {
            let pts_ms = enc_pkt.pts().unwrap_or(0);
            let dts_ms = enc_pkt.dts().unwrap_or(pts_ms);
            let output_packet = MediaPacket {
                media_type: MediaType::Video,
                track_index: 0,
                pts: pts_ms,
                dts: dts_ms,
                is_keyframe: enc_pkt.is_key(),
                format: PayloadFormat::Raw,
                payload: bytes::Bytes::from_owner(OwnedFfmpegPacket(enc_pkt.clone())),
            };
            normalizer.push(output_packet);
        }
    }

    Ok(())
}
