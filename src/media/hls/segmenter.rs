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
use crate::media::stage_lifecycle::StageLifecycle;
use crate::media::stage_metrics::StageMetrics;

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
        .clone()
        .unwrap_or_else(|| StageKey::new(pipeline_id.as_str(), StageKind::hls()));
    let (lifecycle, metrics) = engine
        .get_or_create_non_ring_stage_runtime(
            hls_stage_key,
            crate::media::stage_lifecycle::StagePhase::Registered,
            crate::media::stage_lifecycle::StageBackendKind::HlsSegmenter,
            cancel_token.clone(),
        )
        .await;
    let result = crate::media::executor::run_with_class(
        crate::media::executor::MediaServiceClass::HlsSegmenter,
        cancel_token.clone(),
        run_hls_segmenter(
            pipeline_id,
            store,
            ring_buffer,
            audio_ring_buffer,
            engine.clone(),
            cancel_token.clone(),
            start,
            (lifecycle.clone(), metrics),
        ),
    )
    .await;
    if let Err(error) = result {
        // Retain the submitted generation: cancellation may already have
        // allowed a replacement to register the same key.
        lifecycle.record_error(error);
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_hls_segmenter(
    pipeline_id: String,
    store: Arc<HlsStore>,
    ring_buffer: Arc<RingBuffer>,
    audio_ring_buffer: Option<Arc<RingBuffer>>,
    engine: Arc<MediaEngine>,
    cancel_token: CancellationToken,
    start: HlsSegmenterStart,
    stage: (Arc<StageLifecycle>, Arc<StageMetrics>),
) {
    let hls_stage_key = start
        .planned_stage_key
        .unwrap_or_else(|| StageKey::new(pipeline_id.as_str(), StageKind::hls()));
    let (lifecycle, metrics) = stage;
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

    'segmenter: loop {
        tokio::select! {
            _ = cancel_token.cancelled() => break,
            _ = reader.wait_for_data() => {
                loop {
                    if cancel_token.is_cancelled() {
                        break 'segmenter;
                    }
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
                                    engine.remove_stage_runtime_if_current(&hls_stage_key, &lifecycle).await;
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
                                        let lock = crate::sync::lock(&i.audio_tracks);
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
                    tokio::task::yield_now().await;
                }
            }
        }
    }

    engine
        .remove_stage_runtime_if_current(&hls_stage_key, &lifecycle)
        .await;
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;

    use super::*;
    use crate::media::hls::HlsConfig;
    use crate::media::hls::fmp4::{
        Fmp4HlsStore, parse_fmp4_segment_name, start_hls_fmp4_segmenter,
    };
    use crate::media::packet::MediaPacket;

    async fn package_with_blocked_control<F, P>(
        ring: Arc<RingBuffer>,
        cancel: CancellationToken,
        packets: Vec<MediaPacket>,
        service: F,
        published_segment: P,
    ) -> Bytes
    where
        F: Future<Output = ()>,
        P: Fn() -> Option<Bytes> + Send + 'static,
    {
        let (tx, rx) = std::sync::mpsc::channel();
        let observer = std::thread::spawn(move || {
            let result = (|| {
                let deadline = Instant::now() + Duration::from_secs(5);
                while ring.reader_snapshots().is_empty() {
                    if Instant::now() >= deadline {
                        return Err("segmenter reader did not attach");
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }

                let min_dts = packets.iter().map(|packet| packet.dts).min().unwrap();
                let max_dts = packets.iter().map(|packet| packet.dts).max().unwrap();
                let cycle_ms = max_dts - min_dts + 1000;
                let mut offset_ms = 0;
                let mut publication = None;
                let mut cancelled_at = None;
                loop {
                    // Keep publishing across cancellation: shutdown cannot rely
                    // on reaching an empty ring or on a CONTROL timer firing.
                    ring.push_batch(packets.iter().cloned().map(|mut packet| {
                        packet.dts += offset_ms;
                        packet.pts += offset_ms;
                        packet
                    }));
                    offset_ms += cycle_ms;

                    let readers = ring.reader_snapshots();
                    if let Some(cancelled_at) = cancelled_at {
                        if readers.is_empty() {
                            return Ok(publication.expect("published before cancellation"));
                        }
                        if Instant::now().duration_since(cancelled_at) >= Duration::from_secs(2) {
                            return Err("segmenter failed to cancel with continuing publication");
                        }
                    } else if let Some(segment) = published_segment()
                        && readers
                            .iter()
                            .any(|reader| reader.lag_slots > MEDIA_PULL_BURST_PACKETS)
                    {
                        publication = Some(segment);
                        cancelled_at = Some(Instant::now());
                        cancel.cancel();
                    }

                    if Instant::now() >= deadline {
                        return Err("segmenter did not publish while CONTROL was blocked");
                    }
                    std::thread::yield_now();
                }
            })();
            cancel.cancel();
            tx.send(result).expect("CONTROL observer remains alive");
        });

        // Poll the exported service directly, before deliberately blocking the
        // current-thread CONTROL runtime. Only the media executor can progress.
        let (_, result) = tokio::join!(
            biased;
            service,
            async { rx.recv_timeout(Duration::from_secs(8)) }
        );
        observer.join().expect("publication observer exits");
        result
            .expect("publication observer must finish")
            .expect("media publication and graceful cancellation must be CONTROL-independent")
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hls_ts_packages_and_cancels_while_control_thread_is_blocked() {
        let pipeline_id = "hls-ts-media-control";
        let engine = Arc::new(MediaEngine::new());
        let ring = Arc::new(RingBuffer::new(8192));
        let cancel = CancellationToken::new();
        let store = Arc::new(HlsStore::with_config(HlsConfig {
            min_segment_secs: 0.0,
            max_segments: 64,
            ..HlsConfig::default()
        }));
        let (video, audio, packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h264").expect("fixture packets");
        ring.set_audio_tracks(audio);
        let service = start_hls_segmenter(
            pipeline_id.to_string(),
            store.clone(),
            ring.clone(),
            None,
            engine.clone(),
            cancel.clone(),
            HlsSegmenterStart {
                video_meta_override: Some(video),
                planned_stage_key: None,
            },
        );
        let segment = package_with_blocked_control(ring, cancel, packets, service, move || {
            store
                .snapshot()
                .and_then(|snapshot| snapshot.segments.into_iter().last())
                .map(|segment| segment.data)
        })
        .await;

        let mut demuxer = crate::media::mpegts::TsDemuxer::new();
        demuxer.feed(&segment);
        demuxer.flush();
        assert!(
            demuxer
                .drain()
                .iter()
                .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
            "published TS must carry a demuxable video keyframe"
        );
        assert!(
            engine
                .stage_runtime_snapshot(&StageKey::new(pipeline_id, StageKind::hls()))
                .await
                .is_none(),
            "graceful cancellation removes the stage runtime"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hls_fmp4_packages_and_cancels_while_control_thread_is_blocked() {
        let pipeline_id = "hls-fmp4-media-control";
        let engine = Arc::new(MediaEngine::new());
        let ring = Arc::new(RingBuffer::new(8192));
        let cancel = CancellationToken::new();
        let store = Arc::new(Fmp4HlsStore::with_config(HlsConfig {
            min_segment_secs: 0.0,
            max_segments: 64,
            ..HlsConfig::default()
        }));
        let (video, audio, packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h264").expect("fixture packets");
        ring.set_audio_tracks(audio);
        let service = start_hls_fmp4_segmenter(
            pipeline_id.to_string(),
            store.clone(),
            ring.clone(),
            engine.clone(),
            cancel.clone(),
            HlsSegmenterStart {
                video_meta_override: Some(video),
                planned_stage_key: None,
            },
        );
        let published_store = store.clone();
        let segment = package_with_blocked_control(ring, cancel, packets, service, move || {
            let playlist = published_store.get_video_playlist()?;
            let index = playlist.lines().rev().find_map(parse_fmp4_segment_name)?;
            published_store.get_video_segment(index)
        })
        .await;

        let init = store
            .get_video_init_segment()
            .expect("published video init");
        assert!(init.windows(4).any(|kind| kind == b"avc1"));
        let mut boxes = segment.as_ref();
        let mut found_fragment = false;
        let mut found_payload = false;
        while !boxes.is_empty() {
            assert!(boxes.len() >= 8, "complete MP4 box header");
            let len = u32::from_be_bytes(boxes[..4].try_into().unwrap()) as usize;
            assert!((8..=boxes.len()).contains(&len), "complete MP4 box");
            found_fragment |= &boxes[4..8] == b"moof";
            found_payload |= &boxes[4..8] == b"mdat" && len > 8;
            boxes = &boxes[len..];
        }
        assert!(
            found_fragment && found_payload,
            "fMP4 fragment carries media"
        );
        assert!(
            engine
                .stage_runtime_snapshot(&StageKey::new(pipeline_id, StageKind::hls()))
                .await
                .is_none(),
            "graceful cancellation removes the stage runtime"
        );
    }

    async fn abort_control_owner_after_input<F, P>(
        ring: Arc<RingBuffer>,
        engine: Arc<MediaEngine>,
        key: StageKey,
        cancel: CancellationToken,
        packets: Vec<MediaPacket>,
        service: F,
        has_publication: P,
    ) where
        F: Future<Output = ()> + Send + 'static,
        P: Fn() -> bool,
    {
        let _cancel_guard = cancel.clone().drop_guard();
        let task = tokio::spawn(service);
        tokio::time::timeout(Duration::from_secs(5), async {
            while ring.reader_snapshots().is_empty() {
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("segmenter reader attaches");

        let expected_packets = packets.len() as u64;
        ring.push_batch(packets);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if engine
                    .stage_runtime_snapshot(&key)
                    .await
                    .is_some_and(|snapshot| snapshot.packets_in == expected_packets)
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("segmenter processes the fixture before its owner is aborted");
        assert!(!has_publication(), "no segment boundary before owner abort");
        assert!(!cancel.is_cancelled(), "owner is still active");

        task.abort();
        assert!(
            task.await
                .expect_err("CONTROL owner is aborted")
                .is_cancelled()
        );
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if cancel.is_cancelled()
                    && ring.reader_snapshots().is_empty()
                    && engine.stage_runtime_snapshot(&key).await.is_none()
                {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("aborting CONTROL owner cancels and removes the media segmenter");
        assert!(
            has_publication(),
            "owner abort must flush the final segment"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hls_ts_control_owner_abort_flushes_final_segment_and_removes_stage() {
        let pipeline_id = "hls-ts-control-abort";
        let engine = Arc::new(MediaEngine::new());
        let ring = Arc::new(RingBuffer::new(8192));
        let cancel = CancellationToken::new();
        let store = Arc::new(HlsStore::with_config(HlsConfig {
            min_segment_secs: 3600.0,
            ..HlsConfig::default()
        }));
        let (video, audio, packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h264").expect("fixture packets");
        let packets = packets
            .into_iter()
            .skip_while(|packet| packet.media_type != MediaType::Video || !packet.is_keyframe)
            .collect::<Vec<_>>();
        ring.set_audio_tracks(audio);
        let service = start_hls_segmenter(
            pipeline_id.to_string(),
            store.clone(),
            ring.clone(),
            None,
            engine.clone(),
            cancel.clone(),
            HlsSegmenterStart {
                video_meta_override: Some(video),
                planned_stage_key: None,
            },
        );
        abort_control_owner_after_input(
            ring,
            engine,
            StageKey::new(pipeline_id, StageKind::hls()),
            cancel,
            packets,
            service,
            || store.get_segment(0).is_some(),
        )
        .await;
        let segment = store.get_segment(0).expect("final TS segment");
        let mut demuxer = crate::media::mpegts::TsDemuxer::new();
        demuxer.feed(&segment);
        demuxer.flush();
        assert!(
            demuxer
                .drain()
                .iter()
                .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
            "final flush retains the fixture's video keyframe"
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hls_fmp4_control_owner_abort_flushes_final_segment_and_removes_stage() {
        let pipeline_id = "hls-fmp4-control-abort";
        let engine = Arc::new(MediaEngine::new());
        let ring = Arc::new(RingBuffer::new(8192));
        let cancel = CancellationToken::new();
        let store = Arc::new(Fmp4HlsStore::with_config(HlsConfig {
            min_segment_secs: 3600.0,
            ..HlsConfig::default()
        }));
        let (video, audio, packets) =
            crate::test_fixtures::primary_av_packets_for_codec("h264").expect("fixture packets");
        let packets = packets
            .into_iter()
            .skip_while(|packet| packet.media_type != MediaType::Video || !packet.is_keyframe)
            .collect::<Vec<_>>();
        ring.set_audio_tracks(audio);
        let service = start_hls_fmp4_segmenter(
            pipeline_id.to_string(),
            store.clone(),
            ring.clone(),
            engine.clone(),
            cancel.clone(),
            HlsSegmenterStart {
                video_meta_override: Some(video),
                planned_stage_key: None,
            },
        );
        abort_control_owner_after_input(
            ring,
            engine,
            StageKey::new(pipeline_id, StageKind::hls()),
            cancel,
            packets,
            service,
            || store.has_video_playlist(),
        )
        .await;
        let final_video = store.get_video_segment(0).expect("final fMP4 video");
        assert!(final_video.windows(4).any(|kind| kind == b"moof"));
        assert!(final_video.windows(4).any(|kind| kind == b"mdat"));
        assert!(
            store.get_audio_segment(0, 0).is_some(),
            "owner abort also flushes the alternate audio rendition"
        );
    }

    async fn assert_detached_teardown_preserves_replacement(fmp4: bool) {
        let pipeline_id = if fmp4 {
            "fmp4-generation"
        } else {
            "ts-generation"
        };
        let engine = Arc::new(MediaEngine::new());
        let key = StageKey::new(pipeline_id, StageKind::hls());
        let ring = Arc::new(RingBuffer::new(8));
        // Both production segmenters attach their reader before awaiting
        // sequence headers. Hold that await so replacement precedes teardown.
        let headers = engine.ingests.active.write().await;
        let start_owner = |cancel: CancellationToken| {
            let engine = engine.clone();
            let ring = ring.clone();
            tokio::spawn(async move {
                if fmp4 {
                    start_hls_fmp4_segmenter(
                        pipeline_id.into(),
                        Arc::new(Fmp4HlsStore::new()),
                        ring,
                        engine,
                        cancel,
                        HlsSegmenterStart::default(),
                    )
                    .await;
                } else {
                    start_hls_segmenter(
                        pipeline_id.into(),
                        Arc::new(HlsStore::new()),
                        ring,
                        None,
                        engine,
                        cancel,
                        HlsSegmenterStart::default(),
                    )
                    .await;
                }
            })
        };
        let old_cancel = CancellationToken::new();
        let _old_guard = old_cancel.clone().drop_guard();
        let old_owner = start_owner(old_cancel.clone());
        tokio::time::timeout(Duration::from_secs(5), async {
            while ring.reader_snapshots().is_empty() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("old reader attaches");
        let old_lifecycle = engine
            .stages
            .runtimes
            .read()
            .await
            .get(&key)
            .unwrap()
            .lifecycle
            .clone();
        old_owner.abort();
        assert!(old_owner.await.unwrap_err().is_cancelled());
        assert!(old_cancel.is_cancelled());

        let new_cancel = CancellationToken::new();
        let _new_guard = new_cancel.clone().drop_guard();
        let new_owner = start_owner(new_cancel.clone());
        tokio::time::timeout(Duration::from_secs(5), async {
            while ring.reader_snapshots().len() != 2 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("replacement reader attaches before old teardown");
        let replacement = engine
            .stages
            .runtimes
            .read()
            .await
            .get(&key)
            .unwrap()
            .lifecycle
            .clone();
        assert!(!Arc::ptr_eq(&replacement, &old_lifecycle));
        drop(headers);
        tokio::time::timeout(Duration::from_secs(5), async {
            while ring.reader_snapshots().len() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("old media task finishes detached teardown");
        let current = engine
            .stages
            .runtimes
            .read()
            .await
            .get(&key)
            .expect("replacement runtime survives old teardown")
            .lifecycle
            .clone();
        assert!(Arc::ptr_eq(&current, &replacement));
        assert!(matches!(
            current.snapshot().phase,
            crate::media::stage_lifecycle::StagePhase::BackendSpawned { .. }
        ));
        assert!(!new_cancel.is_cancelled());
        new_cancel.cancel();
        tokio::time::timeout(Duration::from_secs(5), new_owner)
            .await
            .expect("replacement stops")
            .expect("replacement owner joins");
        assert!(engine.stage_runtime_snapshot(&key).await.is_none());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hls_ts_detached_teardown_preserves_replacement() {
        assert_detached_teardown_preserves_replacement(false).await;
    }

    #[tokio::test(flavor = "current_thread")]
    async fn hls_fmp4_detached_teardown_preserves_replacement() {
        assert_detached_teardown_preserves_replacement(true).await;
    }
}
