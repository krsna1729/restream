//! MPEG-TS bytes to a pipeline ring, shared by every ingest whose source is
//! a TS byte stream (file ingest's FFmpeg pipe, HLS push segments).
//!
//! Runs on the MEDIA executor. Each `publish` call takes one bounded chunk
//! (at most 64 KiB), forwards what it completes, and yields, so a source that
//! is always ready cannot monopolize a shared media worker.

mod continuity;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use continuity::ContinuousTimestampState;

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::input_gate::{InputPacketBoundary, InputTimestampMapper};
use crate::media::mpegts::{DemuxProbe, TsDemuxer};
use crate::media::packet::{MediaPacket, MediaType};
use crate::media::ring_buffer::{MEDIA_PRODUCER_BATCH_PACKETS, RingBuffer};
use crate::media::stage_metrics::StageMetrics;

/// Largest chunk a caller should hand to [`TsPublisher::publish`] at once.
pub const PUBLISH_CHUNK_BYTES: usize = 64 * 1024;

/// Where one registered ingest attempt publishes. Resolved once per attempt.
pub struct TsPublishTarget {
    engine: Arc<MediaEngine>,
    pipeline_id: String,
    registration: IngestRegistration,
    ring: Arc<RingBuffer>,
    bytes_received: Arc<AtomicU64>,
    metrics: Arc<StageMetrics>,
    last_progress_ms: Arc<AtomicU64>,
    keyframe_times: Arc<std::sync::Mutex<Vec<i64>>>,
}

impl TsPublishTarget {
    /// Resolve the attempt's counters; fails once the attempt is no longer
    /// the input's current session.
    pub async fn open(
        engine: Arc<MediaEngine>,
        pipeline_id: String,
        registration: IngestRegistration,
        ring: Arc<RingBuffer>,
    ) -> Result<Self, String> {
        let (bytes_received, metrics, last_progress_ms, keyframe_times) = engine
            .with_ingest_session(&registration, |ingest| {
                (
                    ingest.bytes_received.clone(),
                    ingest.metrics.clone(),
                    ingest.last_progress_ms.clone(),
                    ingest.keyframe_times.clone(),
                )
            })
            .await
            .ok_or_else(|| format!("Active ingest missing for pipeline {pipeline_id}"))?;
        Ok(Self {
            engine,
            pipeline_id,
            registration,
            ring,
            bytes_received,
            metrics,
            last_progress_ms,
            keyframe_times,
        })
    }

    pub fn registration(&self) -> &IngestRegistration {
        &self.registration
    }
}

/// Timestamp state that outlives one source pass (file-ingest loops keep it
/// across FFmpeg restarts so every stream keeps increasing).
#[derive(Default)]
pub struct TsTimestamps {
    continuous: ContinuousTimestampState,
    promotion: InputTimestampMapper,
}

/// Demux and forwarding state for one ingest attempt.
pub struct TsPublisher {
    demuxer: TsDemuxer,
    packets: Vec<MediaPacket>,
    probe_sent: bool,
    timestamps: TsTimestamps,
    /// Highest continuity-adjusted DTS demuxed so far (milliseconds).
    media_time_ms: Option<i64>,
}

impl TsPublisher {
    pub fn new(timestamps: TsTimestamps) -> Self {
        Self {
            demuxer: TsDemuxer::new(),
            packets: Vec::with_capacity(MEDIA_PRODUCER_BATCH_PACKETS),
            probe_sent: false,
            timestamps,
            media_time_ms: None,
        }
    }

    pub fn into_timestamps(self) -> TsTimestamps {
        self.timestamps
    }

    /// The media time this publisher has reached: the highest DTS it has
    /// demuxed, after continuity. Callers pace against it; the publisher
    /// itself never waits on the clock.
    pub fn media_time_ms(&self) -> Option<i64> {
        self.media_time_ms
    }

    /// Demux one chunk, forward the packets it completes, count it, yield.
    pub async fn publish(&mut self, target: &TsPublishTarget, chunk: &[u8]) {
        self.demuxer.feed(chunk);
        self.forward(target);
        if !self.probe_sent
            && let Some(probe) = self.demuxer.take_probe()
        {
            self.probe_sent = true;
            record_probe(target, probe).await;
        }

        let read = chunk.len() as u64;
        target.bytes_received.fetch_add(read, Ordering::Relaxed);
        target.metrics.record_in(read);
        target
            .last_progress_ms
            .store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);
        tokio::task::yield_now().await;
    }

    /// End of the TS stream: forward the PES each stream still holds.
    pub fn flush(&mut self, target: &TsPublishTarget) {
        self.demuxer.flush();
        self.forward(target);
    }

    /// The next bytes start an unrelated TS stream (HLS discontinuity or an
    /// encoder restart): finish the current one, demux the next from scratch,
    /// and re-anchor its timestamps right after the last forwarded ones.
    pub fn discontinuity(&mut self, target: &TsPublishTarget) {
        self.flush(target);
        self.demuxer = TsDemuxer::new();
        self.timestamps.continuous.mark_discontinuity();
    }

    fn forward(&mut self, target: &TsPublishTarget) {
        if self.demuxer.drain_into(&mut self.packets) == 0 {
            return;
        }
        let registration = &target.registration;
        for packet in &mut self.packets {
            self.timestamps.continuous.apply(packet);
        }
        // One comparison per batch: DTS increases per stream, and the last
        // packet of a demuxed batch is within one frame of its maximum.
        if let Some(last) = self.packets.last() {
            self.media_time_ms = self.media_time_ms.max(Some(last.dts));
        }
        if let Some(preview_ring) = registration.preview_ring.load_full() {
            preview_ring.push_batch(self.packets.iter().cloned());
        }

        let first_keyframe = self
            .packets
            .iter()
            .position(|packet| packet.media_type == MediaType::Video && packet.is_keyframe);
        let boundary = if first_keyframe.is_some() {
            InputPacketBoundary::VideoKeyframe
        } else {
            InputPacketBoundary::Other
        };
        let Some(lease) = registration.gate.try_enter(boundary) else {
            self.packets.clear();
            return;
        };
        if lease.activated()
            && let Some(first_keyframe) = first_keyframe
        {
            self.packets.drain(..first_keyframe);
        }
        for packet in &mut self.packets {
            self.timestamps.promotion.map_packet(
                packet,
                lease.activated(),
                &registration.last_forwarded_dts,
            );
        }
        for packet in &self.packets {
            if packet.media_type == MediaType::Video
                && let Some(parameter_sets) =
                    crate::media::codec::annexb_parameter_sets(&packet.payload)
            {
                target.ring.set_video_parameter_sets(parameter_sets);
            }
            if packet.media_type == MediaType::Video && packet.is_keyframe {
                let mut times = crate::sync::lock(&target.keyframe_times);
                times.push(packet.pts);
                if times.len() > 30 {
                    times.remove(0);
                }
            }
        }
        if let Some(last) = self.packets.iter().max_by_key(|packet| packet.dts) {
            InputTimestampMapper::record_forwarded(last, &registration.last_forwarded_dts);
        }
        target.ring.push_drained_batch_capped(&mut self.packets);
    }
}

/// The stream's one-time probe becomes the ingest's metadata.
async fn record_probe(target: &TsPublishTarget, probe: DemuxProbe) {
    let engine = &target.engine;
    let first_audio = probe.audio_tracks.first().cloned();
    let selected_video_track_index = probe.video.as_ref().map(|_| 0);
    engine
        .update_ingest_session_meta(
            &target.pipeline_id,
            &target.registration,
            probe.video,
            first_audio,
            None,
        )
        .await;
    if let Some(sequence_header) = probe.video_sequence_header {
        engine
            .cache_ingest_session_sequence_header(&target.registration, true, sequence_header)
            .await;
    }
    engine
        .update_ingest_session_video_track_selection(
            &target.registration,
            probe.video_track_count,
            selected_video_track_index,
        )
        .await;
    if !probe.audio_tracks.is_empty() {
        engine
            .update_ingest_session_audio_tracks(
                &target.pipeline_id,
                &target.registration,
                probe.audio_tracks,
            )
            .await;
    }
}
