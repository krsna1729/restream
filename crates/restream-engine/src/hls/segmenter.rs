use std::sync::Arc;
use std::time::Instant;

use bytes::BytesMut;
use tokio_util::sync::CancellationToken;

use super::{HlsSegmenterContext, HlsSegmenterStart, HlsStore};
use crate::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::ingest_registry::IngestRegistry;
use crate::stage_lifecycle::StageLifecycle;
use crate::stage_metrics::StageMetrics;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_core::{MediaPacket, MediaType};
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_sync::MEDIA_PULL_BURST_PACKETS;
use restream_sync::{Reader, RingBuffer};

pub(crate) async fn start_hls_segmenter(
    pipeline_id: String,
    store: Arc<HlsStore>,
    ring_buffer: Arc<RingBuffer>,
    audio_ring_buffer: Option<Arc<RingBuffer>>,
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
            context,
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
    let reader = Reader::new(format!("hls:{}", pipeline_id), ring_buffer.clone());
    let audio_reader = audio_ring_buffer
        .clone()
        .map(|ring| Reader::new(format!("hls-audio:{}", pipeline_id), ring));
    // Pre-populate SPS/PPS cache from the engine's stored FLV sequence header.
    // This handles the case where the HLS task starts after the seq header has
    // already passed through the ring buffer (e.g. late-joining consumers).
    let (video_sequence_header, _) = ingests.get_sequence_headers(&pipeline_id).await;
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
        ingests,
        cancel_token,
        metrics,
    };
    let ended = segmenter.run().await;

    stages
        .remove_stage_runtime_if_current(&hls_stage_key, &lifecycle)
        .await;
    events.emit(crate::events::EventKind::StageStopped {
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
    ingests: Arc<IngestRegistry>,
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
        let ingests = self.ingests.active.read().await;
        ingests
            .get(&self.pipeline_id)
            .and_then(|ingest| ingest.metadata().video)
    }

    /// Video metadata and audio tracks from the live ingest, falling back to
    /// its single probed audio track.
    async fn ingest_stream_metadata(&self) -> Option<(Option<VideoMeta>, Arc<Vec<AudioMeta>>)> {
        let ingests = self.ingests.active.read().await;
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
mod tests;
