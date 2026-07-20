use std::sync::Arc;
use std::time::Instant;

use bytes::BytesMut;
use tokio_util::sync::CancellationToken;

use super::{HlsSegmenterStart, HlsStore};
use crate::domain::stage::{StageKey, StageKind};
use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::media::engine::MediaEngine;
use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::media::packet::MediaType;
use crate::media::ring_buffer::MEDIA_PULL_BURST_PACKETS;
use crate::media::ring_buffer::{Reader, RingBuffer};

pub async fn start_hls_segmenter(
    pipeline_id: String,
    store: Arc<HlsStore>,
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
    let mut reader = Reader::new(format!("hls:{}", pipeline_id), ring_buffer.clone());
    let mut audio_reader = audio_ring_buffer
        .clone()
        .map(|ring| Reader::new(format!("hls-audio:{}", pipeline_id), ring));
    let mut packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
    let mut audio_packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);
    let mut feeder: Option<TsPacketFeeder> = None;
    // Pre-populate SPS/PPS cache from the engine's stored FLV sequence header.
    // This handles the case where the HLS task starts after the seq header has
    // already passed through the ring buffer (e.g. late-joining consumers).
    let (video_sequence_header, _) = engine.get_sequence_headers(&pipeline_id).await;
    let config = store.config();
    let mut accumulator = BytesMut::with_capacity(config.segment_capacity);
    let mut segment_start = Instant::now();
    let mut got_first_keyframe = false;
    let mut ts_packet_buf = Vec::<u8>::with_capacity(MEDIA_TS_BATCH_TARGET_BYTES);
    let preview_video_meta = start.video_meta_override.clone();

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => break,
            _ = reader.wait_for_data() => {
                loop {
                    packets.clear();
                    match reader.pull_burst(&mut packets, MEDIA_PULL_BURST_PACKETS) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }

                    if let Some(audio_reader) = audio_reader.as_mut() {
                        audio_packets.clear();
                        let _ = audio_reader.pull_burst(&mut audio_packets, MEDIA_PULL_BURST_PACKETS);
                    }

                    for packet in packets.iter().chain(
                        audio_packets
                            .iter()
                            .filter(|packet| packet.media_type == MediaType::Audio),
                    ) {
                        if packet.media_type == MediaType::Video && packet.is_keyframe {
                            if got_first_keyframe {
                                let elapsed = segment_start.elapsed().as_secs_f64();
                                if elapsed >= config.min_segment_secs && !accumulator.is_empty() {
                                    let ts_segment = accumulator.split().freeze();
                                    store.push_segment(elapsed, ts_segment);
                                    accumulator.reserve(config.segment_capacity);
                                    segment_start = Instant::now();
                                }
                            }
                            got_first_keyframe = true;
                        }

                        if !got_first_keyframe {
                            continue;
                        }

                        metrics.record_in(packet.payload.len() as u64);

                        // Lazily create the feeder once we have ingest metadata.
                        // Wait for video metadata to avoid creating a muxer with zero audio
                        // streams when the probe hasn't completed yet.
                        if feeder.is_none() {
                            let (video, audio_tracks) = loop {
                                if cancel_token.is_cancelled() {
                                    engine.remove_stage_runtime(&hls_stage_key).await;
                                    engine.runtime.event_log.emit(crate::events::EventKind::StageStopped {
                                        pipeline_id: pipeline_id.clone(),
                                        encoding: "hls".to_string(),
                                    });
                                    return;
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
                                            .get(&pipeline_id)
                                            .and_then(|ingest| ingest.metadata().video)
                                    };
                                    if video.is_some() {
                                        break (video, std::sync::Arc::new(tracks.to_vec()));
                                    }
                                }
                                if let Some(audio_ring_buffer) = audio_ring_buffer.as_ref()
                                    && let Some(tracks) = audio_ring_buffer
                                        .audio_tracks()
                                        .filter(|tracks| !tracks.is_empty())
                                {
                                    let video = if let Some(video) = preview_video_meta.clone() {
                                        Some(video)
                                    } else {
                                        let ingests = engine.ingests.active.read().await;
                                        ingests
                                            .get(&pipeline_id)
                                            .and_then(|ingest| ingest.metadata().video)
                                    };
                                    if video.is_some() {
                                        break (video, std::sync::Arc::new(tracks.to_vec()));
                                    }
                                }
                                let result = {
                                    let ingests = engine.ingests.active.read().await;
                                    ingests.get(&pipeline_id).and_then(|i| {
                                        let metadata = i.metadata();
                                        let video =
                                            preview_video_meta.clone().or(metadata.video);
                                        video.as_ref()?;
                                        let lock = i.audio_tracks.lock().unwrap_or_else(|e| e.into_inner());
                                        let tracks = if lock.is_empty()
                                            && let Some(audio) = metadata.audio {
                                                std::sync::Arc::new(vec![audio])
                                            } else {
                                                std::sync::Arc::clone(&lock)
                                            };
                                        Some((video, tracks))
                                    })
                                };
                                if let Some(meta) = result {
                                    break meta;
                                }
                                tokio::time::sleep(std::time::Duration::from_millis(100)).await;
                            };
                            let audio_tracks_vec = audio_tracks.as_ref().clone();
                            feeder = Some(TsPacketFeeder::new(
                                video.as_ref(),
                                audio_tracks,
                                PacketFeedConfig {
                                    video_sequence_header: video_sequence_header
                                        .as_ref()
                                        .map(|v| v.to_vec()),
                                    raw_video_parameter_sets: reader
                                        .current_ring()
                                        .video_parameter_sets()
                                        .map(|v| v.to_vec()),
                                    ..PacketFeedConfig::default()
                                },
                            ));
                            store.set_stream_metadata(video.clone(), audio_tracks_vec);
                        }

                        let Some(ref mut feeder) = feeder else {
                            continue;
                        };

                        let t0 = Instant::now();
                        ts_packet_buf.clear();
                        let wrote = feeder.extend_ts_for_packet(packet, &mut ts_packet_buf);
                        metrics.record_processing(t0.elapsed().as_micros() as u64);
                        if wrote {
                            metrics.record_out(ts_packet_buf.len() as u64);
                            accumulator.extend_from_slice(&ts_packet_buf);
                        }
                    }
                }
            }
        }
    }

    engine.remove_stage_runtime(&hls_stage_key).await;
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id: pipeline_id.clone(),
            encoding: "hls".to_string(),
        });

    // Flush remaining data as final segment
    if !accumulator.is_empty() {
        let elapsed = segment_start.elapsed().as_secs_f64();
        let ts_segment = accumulator.freeze();
        store.push_segment(elapsed, ts_segment);
    }
}
