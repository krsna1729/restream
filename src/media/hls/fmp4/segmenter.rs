use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use bytes::Bytes;
use tokio_util::sync::CancellationToken;
use tracing::warn;

use super::super::HlsSegmenterStart;
use super::rendition::{AudioRenditionState, VideoRenditionState};
use super::store::Fmp4HlsStore;
use crate::domain::stage::{StageKey, StageKind};
use crate::media::engine::MediaEngine;
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::MediaType;
use crate::media::ring_buffer::{Reader, RingBuffer};

pub async fn start_hls_fmp4_segmenter(
    pipeline_id: String,
    store: Arc<Fmp4HlsStore>,
    ring_buffer: Arc<RingBuffer>,
    audio_ring_buffer: Option<Arc<RingBuffer>>,
    engine: Arc<MediaEngine>,
    cancel_token: CancellationToken,
    start: HlsSegmenterStart,
) {
    let hls_stage_key = start
        .planned_stage_key
        .unwrap_or_else(|| StageKey::new(pipeline_id.as_str(), StageKind::hls()));
    let (lifecycle, metrics) = engine
        .get_or_create_non_ring_stage_runtime(
            hls_stage_key.clone(),
            crate::media::stage_lifecycle::StagePhase::Registered,
            crate::media::stage_lifecycle::StageBackendKind::HlsSegmenter,
            cancel_token.clone(),
        )
        .await;
    let _lifecycle_guard =
        crate::media::stage_lifecycle::StageLifecycleGuard::new(lifecycle.clone());
    lifecycle.transition(crate::media::stage_lifecycle::StagePhase::BackendSpawned {
        backend: crate::media::stage_lifecycle::StageBackendKind::HlsSegmenter,
        pid: None,
    });
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageRegistered {
            pipeline_id: pipeline_id.clone(),
            encoding: "hls".to_string(),
        });

    let mut reader = Reader::new(format!("hls-fmp4:{pipeline_id}"), ring_buffer.clone());
    let mut audio_reader = audio_ring_buffer
        .clone()
        .map(|ring| Reader::new(format!("hls-fmp4-audio:{pipeline_id}"), ring));
    let mut packets = Vec::with_capacity(32);
    let mut audio_packets = Vec::with_capacity(32);
    let (video_sequence_header, audio_sequence_header) =
        resolve_hls_sequence_headers(&engine, &pipeline_id).await;
    let config = store.config();
    let min_segment_ms = (config.min_segment_secs * 1000.0).round() as i64;
    let preview_video_meta = start.video_meta_override.clone();

    let mut video_state: Option<VideoRenditionState> = None;
    let mut audio_states: HashMap<u32, AudioRenditionState> = HashMap::new();
    let mut next_segment_index = 0u64;
    let mut got_first_keyframe = false;
    let mut global_zero_ms = 0i64;
    let mut segment_start_pts_ms = 0i64;

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => break,
            _ = reader.wait_for_data() => {
                loop {
                    packets.clear();
                    match reader.pull_burst(&mut packets, 32) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }

                    if let Some(audio_reader) = audio_reader.as_mut() {
                        audio_packets.clear();
                        let _ = audio_reader.pull_burst(&mut audio_packets, 32);
                    }

                    for packet in packets.iter().chain(
                        audio_packets
                            .iter()
                            .filter(|packet| packet.media_type == MediaType::Audio),
                    ) {
                        metrics.record_in(packet.payload.len() as u64);

                        if video_state.is_none() {
                            let Some((video, audio_tracks)) = resolve_hls_preview_metadata(
                                &engine,
                                &ring_buffer,
                                audio_ring_buffer.as_ref(),
                                &cancel_token,
                                &pipeline_id,
                                preview_video_meta.clone(),
                            )
                            .await else {
                                engine.remove_stage_runtime(&hls_stage_key).await;
                                engine.runtime.event_log.emit(crate::events::EventKind::StageStopped {
                                    pipeline_id: pipeline_id.clone(),
                                    encoding: "hls".to_string(),
                                });
                                return;
                            };

                            let supported_audio_tracks: Vec<AudioMeta> = audio_tracks
                                .into_iter()
                                .filter(|track| track.codec.eq_ignore_ascii_case("aac"))
                                .collect();
                            store.set_stream_metadata(Some(video.clone()), supported_audio_tracks.clone());
                            video_state = Some(VideoRenditionState::new(
                                &video,
                                video_sequence_header.as_deref(),
                            ));
                            for track in supported_audio_tracks {
                                audio_states.insert(
                                    track.track_index,
                                    AudioRenditionState::new(
                                        &track,
                                        audio_sequence_header.as_deref(),
                                    ),
                                );
                            }
                        }

                        let t0 = Instant::now();
                        match packet.media_type {
                            MediaType::Video => {
                                let Some(state) = video_state.as_mut() else {
                                    continue;
                                };
                                if packet.is_keyframe {
                                    if !got_first_keyframe {
                                        got_first_keyframe = true;
                                        global_zero_ms = packet.dts;
                                        segment_start_pts_ms = packet.pts;
                                    } else if packet.pts - segment_start_pts_ms >= min_segment_ms {
                                        if let Err(err) = state.flush_segment(
                                            &store,
                                            next_segment_index,
                                            segment_duration_secs(segment_start_pts_ms, packet.pts),
                                            Some(relative_to_hls_zero_ms(
                                                packet.dts,
                                                global_zero_ms,
                                            )),
                                        ) {
                                            warn!(pipeline_id = %pipeline_id, err = %err, "failed to flush video fmp4 segment");
                                        }
                                        for audio_state in audio_states.values_mut() {
                                            if let Err(err) = audio_state.flush_segment(
                                                &store,
                                                next_segment_index,
                                                segment_duration_secs(segment_start_pts_ms, packet.pts),
                                            ) {
                                                warn!(pipeline_id = %pipeline_id, err = %err, "failed to flush audio fmp4 segment");
                                            }
                                        }
                                        next_segment_index += 1;
                                        segment_start_pts_ms = packet.pts;
                                    }
                                }

                                if !got_first_keyframe || packet.dts < global_zero_ms {
                                    continue;
                                }
                                if let Err(err) = state.push_packet(packet, global_zero_ms) {
                                    warn!(pipeline_id = %pipeline_id, err = %err, "dropping video packet from fmp4 preview");
                                }
                            }
                            MediaType::Audio => {
                                if !got_first_keyframe || packet.dts < global_zero_ms {
                                    continue;
                                }
                                let Some(state) = audio_states.get_mut(&packet.track_index) else {
                                    continue;
                                };
                                if let Err(err) = state.push_packet(packet, global_zero_ms) {
                                    warn!(
                                        pipeline_id = %pipeline_id,
                                        track_index = packet.track_index,
                                        err = %err,
                                        "dropping audio packet from fmp4 preview"
                                    );
                                }
                            }
                        }
                        metrics.record_processing(t0.elapsed().as_micros() as u64);
                    }
                }
            }
        }
    }

    if let Some(state) = video_state.as_mut()
        && let Err(err) = state.flush_segment(
            &store,
            next_segment_index,
            state.current_segment_duration_secs(),
            None,
        )
    {
        warn!(pipeline_id = %pipeline_id, err = %err, "failed to flush final video fmp4 segment");
    }
    for audio_state in audio_states.values_mut() {
        if let Err(err) = audio_state.flush_segment(
            &store,
            next_segment_index,
            audio_state.current_segment_duration_secs(),
        ) {
            warn!(pipeline_id = %pipeline_id, err = %err, "failed to flush final audio fmp4 segment");
        }
    }

    engine.remove_stage_runtime(&hls_stage_key).await;
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id,
            encoding: "hls".to_string(),
        });
}

async fn resolve_hls_preview_metadata(
    engine: &MediaEngine,
    ring_buffer: &Arc<RingBuffer>,
    audio_ring_buffer: Option<&Arc<RingBuffer>>,
    cancel_token: &CancellationToken,
    pipeline_id: &str,
    preview_video_meta: Option<VideoMeta>,
) -> Option<(VideoMeta, Vec<AudioMeta>)> {
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
                let ingests = engine.ingests.active.read().await;
                ingests
                    .get(pipeline_id)
                    .and_then(|ingest| ingest.metadata().video)
            };
            if let Some(video) = video {
                return Some((video, tracks.to_vec()));
            }
        }
        if let Some(audio_ring_buffer) = audio_ring_buffer
            && let Some(tracks) = audio_ring_buffer
                .audio_tracks()
                .filter(|tracks| !tracks.is_empty())
        {
            let video = if let Some(video) = preview_video_meta.clone() {
                Some(video)
            } else {
                let ingests = engine.ingests.active.read().await;
                ingests
                    .get(pipeline_id)
                    .and_then(|ingest| ingest.metadata().video)
            };
            if let Some(video) = video {
                return Some((video, tracks.to_vec()));
            }
        }
        let result = {
            let ingests = engine.ingests.active.read().await;
            ingests.get(pipeline_id).and_then(|ingest| {
                let metadata = ingest.metadata();
                let video = preview_video_meta.clone().or(metadata.video)?;
                let lock = ingest
                    .audio_tracks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
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
    engine: &MediaEngine,
    pipeline_id: &str,
) -> (Option<Bytes>, Option<Bytes>) {
    if let Some(input_id) =
        crate::media::engine_hls::input_id_from_hls_preview_resource_id(pipeline_id)
    {
        engine.get_input_sequence_headers(input_id).await
    } else {
        engine.get_sequence_headers(pipeline_id).await
    }
}

fn segment_duration_secs(start_pts_ms: i64, end_pts_ms: i64) -> f64 {
    end_pts_ms.saturating_sub(start_pts_ms).max(1) as f64 / 1000.0
}
