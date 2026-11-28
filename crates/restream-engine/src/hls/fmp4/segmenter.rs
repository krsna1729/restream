use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::super::{HlsSegmenterContext, HlsSegmenterStart};
use super::rendition::{AudioRenditionState, VideoRenditionState};
use super::store::Fmp4HlsStore;
use crate::ingest_registry::IngestRegistry;
use crate::stage_lifecycle::StageLifecycle;
use crate::stage_metrics::StageMetrics;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_core::{MediaPacket, MediaType};
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_sync::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer};

pub(crate) async fn start_hls_fmp4_segmenter(
    pipeline_id: String,
    store: Arc<Fmp4HlsStore>,
    ring_buffer: Arc<RingBuffer>,
    context: HlsSegmenterContext,
    cancel_token: CancellationToken,
    start: HlsSegmenterStart,
) {
    let hls_stage_key = start
        .planned_stage_key
        .clone()
        .unwrap_or_else(|| StageKey::new(pipeline_id.as_str(), StageKind::hls()));
    let stage = hls_stage_key.to_string();
    let (lifecycle, metrics) = context
        .stages
        .get_or_create_non_ring_stage_runtime(
            hls_stage_key,
            crate::stage_lifecycle::StagePhase::Registered,
            crate::stage_lifecycle::StageBackendKind::HlsSegmenter,
            cancel_token.clone(),
        )
        .await;
    let result = crate::executor::run_with_class(
        crate::executor::MediaService {
            class: crate::executor::MediaServiceClass::HlsFmp4,
            pipeline_id: pipeline_id.clone(),
            stage,
        },
        cancel_token.clone(),
        run_hls_fmp4_segmenter(
            pipeline_id,
            store,
            ring_buffer,
            context,
            cancel_token.clone(),
            start,
            (lifecycle.clone(), metrics),
        ),
    )
    .await;
    if let Err(error) = result {
        // Report only to the submitted generation, never a replacement.
        lifecycle.record_error(error);
    }
}

async fn run_hls_fmp4_segmenter(
    pipeline_id: String,
    store: Arc<Fmp4HlsStore>,
    ring_buffer: Arc<RingBuffer>,
    context: HlsSegmenterContext,
    cancel_token: CancellationToken,
    start: HlsSegmenterStart,
    stage: (Arc<StageLifecycle>, Arc<StageMetrics>),
) {
    let HlsSegmenterContext {
        ingests,
        stages,
        events,
    } = context;
    let hls_stage_key = start
        .planned_stage_key
        .unwrap_or_else(|| StageKey::new(pipeline_id.as_str(), StageKind::hls()));
    let (lifecycle, metrics) = stage;
    let _lifecycle_guard = crate::stage_lifecycle::StageLifecycleGuard::new(lifecycle.clone());
    lifecycle.transition(crate::stage_lifecycle::StagePhase::BackendSpawned {
        backend: crate::stage_lifecycle::StageBackendKind::HlsSegmenter,
        pid: None,
    });
    events.emit(crate::events::EventKind::StageRegistered {
        pipeline_id: pipeline_id.clone(),
        encoding: "hls".to_string(),
    });

    let mut reader = Reader::new(format!("hls-fmp4:{pipeline_id}"), ring_buffer.clone());
    let mut packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
    let (video_sequence_header, audio_sequence_header) =
        resolve_hls_sequence_headers(&ingests, &pipeline_id).await;
    let min_segment_ms = (store.config().min_segment_secs * 1000.0).round() as i64;
    let mut renditions = None;

    'segmenter: loop {
        tokio::select! {
            _ = cancel_token.cancelled() => break,
            _ = reader.wait_for_data() => {
                loop {
                    if cancel_token.is_cancelled() {
                        break 'segmenter;
                    }
                    packets.clear();
                    if matches!(reader.pull_burst(&mut packets, MEDIA_PULL_BURST_PACKETS), Ok(0) | Err(_)) {
                        break;
                    }
                    for packet in &packets {
                        metrics.record_in(packet.payload.len() as u64);
                        if renditions.is_none() {
                            let Some((video, audio)) = resolve_hls_preview_metadata(
                                &ingests,
                                &ring_buffer,
                                &cancel_token,
                                &pipeline_id,
                                start.video_meta_override.clone(),
                            ).await else {
                                break 'segmenter;
                            };
                            let (audio, advertised) =
                                bind_preview_audio_tracks(audio, audio_sequence_header.as_deref(), &pipeline_id);
                            store.set_stream_metadata(Some(video.clone()), advertised);
                            renditions = Some(Renditions {
                                video: VideoRenditionState::new(&video, video_sequence_header.as_deref()),
                                audio,
                                segment_index: 0,
                                clock: None,
                            });
                        }
                        let t0 = Instant::now();
                        if let Some(state) = renditions.as_mut()
                            && state.push(packet, &store, &pipeline_id, min_segment_ms)
                        {
                            metrics.record_processing(t0.elapsed().as_micros() as u64);
                        }
                    }
                    tokio::task::yield_now().await;
                }
            }
        }
    }

    // Cancellation during the metadata wait leaves no renditions to flush.
    if let Some(state) = renditions.as_mut() {
        state.flush_final(&store, &pipeline_id);
    }

    stages
        .remove_stage_runtime_if_current(&hls_stage_key, &lifecycle)
        .await;
    events.emit(crate::events::EventKind::StageStopped {
        pipeline_id,
        encoding: "hls".to_string(),
    });
}

struct SegmentClock {
    zero_dts_ms: i64,
    start_pts_ms: i64,
}

struct Renditions {
    video: VideoRenditionState,
    audio: HashMap<u32, AudioRenditionState>,
    segment_index: u64,
    clock: Option<SegmentClock>,
}

impl Renditions {
    /// Returns whether the packet reached a rendition, including mux errors.
    fn push(
        &mut self,
        packet: &MediaPacket,
        store: &Fmp4HlsStore,
        pipeline_id: &str,
        min_segment_ms: i64,
    ) -> bool {
        if packet.media_type == MediaType::Video && packet.is_keyframe {
            self.cut_at_keyframe(packet, store, pipeline_id, min_segment_ms);
        }
        let Some(clock) = &self.clock else {
            return false;
        };
        if packet.dts < clock.zero_dts_ms {
            return false;
        }
        match packet.media_type {
            MediaType::Video => {
                if let Err(err) = self.video.push_packet(packet, clock.zero_dts_ms) {
                    warn!(pipeline_id, %err, "dropping video packet from fmp4 preview");
                }
            }
            MediaType::Audio => {
                let Some(state) = self.audio.get_mut(&packet.track_index) else {
                    return false;
                };
                if let Err(err) = state.push_packet(packet, clock.zero_dts_ms) {
                    warn!(pipeline_id, track_index = packet.track_index, %err, "dropping audio packet from fmp4 preview");
                }
            }
        }
        true
    }

    fn cut_at_keyframe(
        &mut self,
        packet: &MediaPacket,
        store: &Fmp4HlsStore,
        pipeline_id: &str,
        min_segment_ms: i64,
    ) {
        let Some(clock) = self.clock.as_mut() else {
            self.clock = Some(SegmentClock {
                zero_dts_ms: packet.dts,
                start_pts_ms: packet.pts,
            });
            return;
        };
        if packet.pts - clock.start_pts_ms < min_segment_ms {
            return;
        }
        let duration = segment_duration_secs(clock.start_pts_ms, packet.pts);
        let end = relative_to_hls_zero_ms(packet.dts, clock.zero_dts_ms);
        if let Err(err) = self
            .video
            .flush_segment(store, self.segment_index, duration, Some(end))
        {
            warn!(pipeline_id, %err, "failed to flush video fmp4 segment");
        }
        for state in self.audio.values_mut() {
            if let Err(err) = state.flush_segment(store, self.segment_index, duration) {
                warn!(pipeline_id, %err, "failed to flush audio fmp4 segment");
            }
        }
        self.segment_index += 1;
        clock.start_pts_ms = packet.pts;
    }

    fn flush_final(&mut self, store: &Fmp4HlsStore, pipeline_id: &str) {
        if let Err(err) = self.video.flush_segment(
            store,
            self.segment_index,
            self.video.current_segment_duration_secs(),
            None,
        ) {
            warn!(pipeline_id, %err, "failed to flush final video fmp4 segment");
        }
        for state in self.audio.values_mut() {
            if let Err(err) = state.flush_segment(
                store,
                self.segment_index,
                state.current_segment_duration_secs(),
            ) {
                warn!(pipeline_id, %err, "failed to flush final audio fmp4 segment");
            }
        }
    }
}

fn bind_preview_audio_tracks(
    audio_tracks: Vec<AudioMeta>,
    audio_sequence_header: Option<&[u8]>,
    pipeline_id: &str,
) -> (HashMap<u32, AudioRenditionState>, Vec<AudioMeta>) {
    let mut audio_states = HashMap::new();
    let mut advertised = Vec::new();
    for track in audio_tracks {
        if !track.codec.eq_ignore_ascii_case("aac") {
            continue;
        }
        match AudioRenditionState::new(&track, audio_sequence_header) {
            Some(state) => {
                audio_states.insert(track.track_index, state);
                advertised.push(track);
            }
            None => {
                warn!(
                    pipeline_id = %pipeline_id,
                    track_index = track.track_index,
                    "dropping HLS preview audio track; no mp4a sample entry"
                );
            }
        }
    }
    (audio_states, advertised)
}

async fn resolve_hls_preview_metadata(
    ingests: &IngestRegistry,
    ring_buffer: &Arc<RingBuffer>,
    cancel_token: &CancellationToken,
    pipeline_id: &str,
    preview_video_meta: Option<VideoMeta>,
) -> Option<(VideoMeta, Vec<AudioMeta>)> {
    let pipeline_id = crate::preview_ids::preview_pipeline(pipeline_id).0;
    loop {
        if cancel_token.is_cancelled() {
            return None;
        }
        if let Some(tracks) = ring_buffer
            .audio_tracks()
            .filter(|tracks| !tracks.is_empty())
        {
            let video = if let Some(video) = preview_video_meta.clone() {
                Some(video)
            } else {
                let ingests = ingests.active.read().await;
                ingests
                    .get(pipeline_id)
                    .and_then(|ingest| ingest.metadata().video)
            };
            if let Some(video) = video {
                return Some((video, tracks.to_vec()));
            }
        }
        let result = {
            let ingests = ingests.active.read().await;
            ingests.get(pipeline_id).and_then(|ingest| {
                let metadata = ingest.metadata();
                let video = preview_video_meta.clone().or(metadata.video)?;
                let lock = restream_sync::lock::lock(&ingest.audio_tracks);
                let tracks = if lock.is_empty() {
                    metadata
                        .audio
                        .clone()
                        .map(|audio| vec![audio])
                        .unwrap_or_default()
                } else {
                    lock.as_ref().clone()
                };
                Some((video, tracks))
            })
        };
        if let Some(result) = result {
            return Some(result);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

pub(super) const fn relative_to_hls_zero_ms(timestamp_ms: i64, zero_ms: i64) -> i64 {
    timestamp_ms.saturating_sub(zero_ms)
}

pub(super) async fn resolve_hls_sequence_headers(
    ingests: &IngestRegistry,
    pipeline_id: &str,
) -> (Option<Bytes>, Option<Bytes>) {
    if let Some(input_id) = crate::preview_ids::input_id_from_hls_preview_resource_id(pipeline_id) {
        ingests.get_input_sequence_headers(input_id).await
    } else {
        ingests
            .get_sequence_headers(crate::preview_ids::preview_pipeline(pipeline_id).0)
            .await
    }
}

fn segment_duration_secs(start_pts_ms: i64, end_pts_ms: i64) -> f64 {
    end_pts_ms.saturating_sub(start_pts_ms).max(1) as f64 / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use restream_core::metadata::AudioMeta;

    fn aac_track(track_index: u32) -> AudioMeta {
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: None,
            track_index,
            pid: None,
            language: None,
            title: None,
            profile: None,
        }
    }

    fn opus_track(track_index: u32) -> AudioMeta {
        AudioMeta {
            codec: "opus".to_string(),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: None,
            track_index,
            pid: None,
            language: None,
            title: None,
            profile: None,
        }
    }

    #[test]
    fn bind_preview_audio_tracks_advertises_only_bound_aac() {
        let (states, advertised) = bind_preview_audio_tracks(
            vec![opus_track(1), aac_track(2)],
            None,
            "pipe-preview-audio",
        );
        assert_eq!(advertised.len(), 1);
        assert_eq!(advertised[0].track_index, 2);
        assert!(states.contains_key(&2));
        assert!(!states.contains_key(&1));
    }
}
