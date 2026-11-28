use std::sync::Arc;
use std::time::Instant;

use bytes::BytesMut;
use tokio_util::sync::CancellationToken;

use super::{HlsSegmenterStart, HlsStore};
use crate::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::engine::MediaEngine;
use crate::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::stage_lifecycle::StageLifecycle;
use crate::stage_metrics::StageMetrics;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_core::{MediaPacket, MediaType};
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_sync::MEDIA_PULL_BURST_PACKETS;
use restream_sync::{Reader, RingBuffer};

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
    let stage = hls_stage_key.to_string();
    let (lifecycle, metrics) = engine
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
            class: crate::executor::MediaServiceClass::HlsSegmenter,
            pipeline_id: pipeline_id.clone(),
            stage,
        },
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
    let _lifecycle_guard = crate::stage_lifecycle::StageLifecycleGuard::new(lifecycle.clone());
    lifecycle.transition(crate::stage_lifecycle::StagePhase::BackendSpawned {
        backend: crate::stage_lifecycle::StageBackendKind::HlsSegmenter,
        pid: None,
    });
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageRegistered {
            pipeline_id: pipeline_id.clone(),
            encoding: "hls".to_string(),
        });
    let reader = Reader::new(format!("hls:{}", pipeline_id), ring_buffer.clone());
    let audio_reader = audio_ring_buffer
        .clone()
        .map(|ring| Reader::new(format!("hls-audio:{}", pipeline_id), ring));
    // Pre-populate SPS/PPS cache from the engine's stored FLV sequence header.
    // This handles the case where the HLS task starts after the seq header has
    // already passed through the ring buffer (e.g. late-joining consumers).
    let (video_sequence_header, _) = engine.ingests.get_sequence_headers(&pipeline_id).await;
    let config = store.config();
    let mut segmenter = HlsSegmenter {
        accumulator: BytesMut::with_capacity(config.segment_capacity),
        min_segment_secs: config.min_segment_secs,
        segment_capacity: config.segment_capacity,
        segment_start: Instant::now(),
        got_first_keyframe: false,
        ts_packet_buf: Vec::with_capacity(MEDIA_TS_BATCH_TARGET_BYTES),
        feeder: None,
        video_sequence_header,
        preview_video_meta: start.video_meta_override,
        reader,
        audio_reader,
        packets: Vec::with_capacity(MEDIA_PULL_BURST_PACKETS),
        audio_packets: Vec::with_capacity(MEDIA_PULL_BURST_PACKETS),
        pipeline_id,
        store,
        ring_buffer,
        audio_ring_buffer,
        engine,
        cancel_token,
        metrics,
    };
    let ended = segmenter.run().await;

    segmenter
        .engine
        .stages
        .remove_stage_runtime_if_current(&hls_stage_key, &lifecycle)
        .await;
    segmenter
        .engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id: segmenter.pipeline_id.clone(),
            encoding: "hls".to_string(),
        });
    if ended == SegmenterEnd::Cancelled {
        segmenter.flush();
    }
}

/// How the segmenter loop ended.
#[derive(Clone, Copy, PartialEq, Eq)]
enum SegmenterEnd {
    /// Cancelled while segmenting: the partial segment is flushed.
    Cancelled,
    /// Cancelled while waiting for stream metadata: nothing is flushed.
    CancelledAwaitingMetadata,
}

struct HlsSegmenter {
    accumulator: BytesMut,
    min_segment_secs: f64,
    segment_capacity: usize,
    segment_start: Instant,
    got_first_keyframe: bool,
    ts_packet_buf: Vec<u8>,
    /// Created once ingest metadata is known.
    feeder: Option<TsPacketFeeder>,
    video_sequence_header: Option<bytes::Bytes>,
    preview_video_meta: Option<VideoMeta>,
    reader: Reader,
    audio_reader: Option<Reader>,
    packets: Vec<Arc<MediaPacket>>,
    audio_packets: Vec<Arc<MediaPacket>>,
    pipeline_id: String,
    store: Arc<HlsStore>,
    ring_buffer: Arc<RingBuffer>,
    audio_ring_buffer: Option<Arc<RingBuffer>>,
    engine: Arc<MediaEngine>,
    cancel_token: CancellationToken,
    metrics: Arc<StageMetrics>,
}

impl HlsSegmenter {
    async fn run(&mut self) -> SegmenterEnd {
        loop {
            tokio::select! {
                _ = self.cancel_token.cancelled() => return SegmenterEnd::Cancelled,
                _ = self.reader.wait_for_data() => {
                    if let Some(end) = self.drain().await {
                        return end;
                    }
                }
            }
        }
    }

    /// Pulls and segments bursts until the ring is empty; `Some` on
    /// cancellation.
    async fn drain(&mut self) -> Option<SegmenterEnd> {
        loop {
            if self.cancel_token.is_cancelled() {
                return Some(SegmenterEnd::Cancelled);
            }
            self.packets.clear();
            match self
                .reader
                .pull_burst(&mut self.packets, MEDIA_PULL_BURST_PACKETS)
            {
                Ok(0) | Err(_) => return None,
                Ok(_) => {}
            }
            if let Some(audio_reader) = self.audio_reader.as_mut() {
                self.audio_packets.clear();
                let _ = audio_reader.pull_burst(&mut self.audio_packets, MEDIA_PULL_BURST_PACKETS);
            }
            // Taken for the burst so packets can be borrowed while `self`
            // changes; the buffers go back with their capacity.
            let packets = std::mem::take(&mut self.packets);
            let audio_packets = std::mem::take(&mut self.audio_packets);
            let end = self.segment_burst(&packets, &audio_packets).await;
            self.packets = packets;
            self.audio_packets = audio_packets;
            if end.is_some() {
                return end;
            }
            tokio::task::yield_now().await;
        }
    }

    async fn segment_burst(
        &mut self,
        packets: &[Arc<MediaPacket>],
        audio_packets: &[Arc<MediaPacket>],
    ) -> Option<SegmenterEnd> {
        for packet in packets.iter().chain(
            audio_packets
                .iter()
                .filter(|packet| packet.media_type == MediaType::Audio),
        ) {
            if !self.observe_keyframe(packet) {
                continue;
            }
            self.metrics.record_in(packet.payload.len() as u64);
            if self.feeder.is_none() && !self.create_feeder().await {
                return Some(SegmenterEnd::CancelledAwaitingMetadata);
            }
            self.feed(packet);
        }
        None
    }

    /// Cuts a segment at a video keyframe once the minimum duration has
    /// passed; false until the first keyframe.
    fn observe_keyframe(&mut self, packet: &MediaPacket) -> bool {
        if packet.media_type == MediaType::Video && packet.is_keyframe {
            if self.got_first_keyframe {
                let elapsed = self.segment_start.elapsed().as_secs_f64();
                if elapsed >= self.min_segment_secs && !self.accumulator.is_empty() {
                    let ts_segment = self.accumulator.split().freeze();
                    self.store.push_segment(elapsed, ts_segment);
                    self.accumulator.reserve(self.segment_capacity);
                    self.segment_start = Instant::now();
                }
            }
            self.got_first_keyframe = true;
        }
        self.got_first_keyframe
    }

    fn feed(&mut self, packet: &MediaPacket) {
        let Some(feeder) = self.feeder.as_mut() else {
            return;
        };
        let t0 = Instant::now();
        self.ts_packet_buf.clear();
        let wrote = feeder.extend_ts_for_packet(packet, &mut self.ts_packet_buf);
        self.metrics
            .record_processing(t0.elapsed().as_micros() as u64);
        if wrote {
            self.metrics.record_out(self.ts_packet_buf.len() as u64);
            self.accumulator.extend_from_slice(&self.ts_packet_buf);
        }
    }

    /// Creates the TS feeder once video metadata and audio tracks are known.
    /// Waiting for video metadata avoids a muxer with zero audio streams when
    /// the probe has not completed yet. False when cancelled while waiting.
    async fn create_feeder(&mut self) -> bool {
        let Some((video, audio_tracks)) = self.await_stream_metadata().await else {
            return false;
        };
        let audio_tracks_vec = audio_tracks.as_ref().clone();
        self.feeder = Some(TsPacketFeeder::new(
            video.as_ref(),
            audio_tracks,
            PacketFeedConfig {
                video_sequence_header: self.video_sequence_header.as_ref().map(|v| v.to_vec()),
                raw_video_parameter_sets: self.reader.video_parameter_sets().map(|v| v.to_vec()),
                ..PacketFeedConfig::default()
            },
        ));
        self.store.set_stream_metadata(video, audio_tracks_vec);
        true
    }

    /// Polls every 100 ms for video metadata with audio tracks: from the
    /// video ring, the audio ring, then the live ingest. `None` on
    /// cancellation.
    async fn await_stream_metadata(&self) -> Option<(Option<VideoMeta>, Arc<Vec<AudioMeta>>)> {
        loop {
            if self.cancel_token.is_cancelled() {
                return None;
            }
            for ring in std::iter::once(&self.ring_buffer).chain(self.audio_ring_buffer.as_ref()) {
                if let Some(tracks) = ring.audio_tracks().filter(|tracks| !tracks.is_empty())
                    && let Some(video) = self.video_meta().await
                {
                    return Some((Some(video), Arc::new(tracks)));
                }
            }
            if let Some(meta) = self.ingest_stream_metadata().await {
                return Some(meta);
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    /// The preview override, else the live ingest's video metadata.
    async fn video_meta(&self) -> Option<VideoMeta> {
        if let Some(video) = self.preview_video_meta.clone() {
            return Some(video);
        }
        let ingests = self.engine.ingests.active.read().await;
        ingests
            .get(&self.pipeline_id)
            .and_then(|ingest| ingest.metadata().video)
    }

    /// Video metadata and audio tracks from the live ingest, falling back to
    /// its single probed audio track.
    async fn ingest_stream_metadata(&self) -> Option<(Option<VideoMeta>, Arc<Vec<AudioMeta>>)> {
        let ingests = self.engine.ingests.active.read().await;
        let ingest = ingests.get(&self.pipeline_id)?;
        let metadata = ingest.metadata();
        let video = self.preview_video_meta.clone().or(metadata.video);
        video.as_ref()?;
        let lock = restream_sync::lock::lock(&ingest.audio_tracks);
        let tracks = if lock.is_empty()
            && let Some(audio) = metadata.audio
        {
            Arc::new(vec![audio])
        } else {
            Arc::clone(&lock)
        };
        Some((video, tracks))
    }

    /// Publishes the remaining data as the final segment.
    fn flush(&mut self) {
        if !self.accumulator.is_empty() {
            let elapsed = self.segment_start.elapsed().as_secs_f64();
            let ts_segment = self.accumulator.split().freeze();
            self.store.push_segment(elapsed, ts_segment);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use bytes::Bytes;

    use super::*;
    use crate::hls::HlsConfig;
    use crate::hls::fmp4::{Fmp4HlsStore, parse_fmp4_segment_name, start_hls_fmp4_segmenter};
    use restream_core::MediaPacket;

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
                .and_then(|snapshot| snapshot.segments.last().map(|segment| segment.data.clone()))
        })
        .await;

        let mut demuxer = restream_mpegts::TsDemuxer::new();
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
        let mut demuxer = restream_mpegts::TsDemuxer::new();
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
            crate::stage_lifecycle::StagePhase::BackendSpawned { .. }
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
