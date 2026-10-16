//! One source's media bytes to a pipeline ring, shared by every ingest whose
//! source is a byte stream: file ingest's FFmpeg pipe (MPEG-TS) and HLS push
//! (MPEG-TS or fragmented MP4).
//!
//! The container lives behind [`SegmentDemux`]; continuity, preview, the input
//! gate, promotion, parameter sets, ring push and the counters are one core
//! for both.
//!
//! Runs on the MEDIA executor. Each `publish` call takes one bounded chunk
//! (at most 64 KiB), forwards what it completes, and yields, so a source that
//! is always ready cannot monopolize a shared media worker.

mod continuity;
pub mod fmp4;
#[cfg(test)]
#[path = "ingest_publisher/tests.rs"]
mod tests;

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use bytes::Bytes;
use continuity::ContinuousTimestampState;

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::input_gate::{InputPacketBoundary, InputTimestampMapper};
use crate::media::mpegts::{DemuxProbe, TsDemuxer};
use crate::media::packet::{MediaPacket, MediaType};
use crate::media::ring_buffer::{MEDIA_PRODUCER_BATCH_PACKETS, RingBuffer};
use crate::media::stage_metrics::StageMetrics;

/// Largest chunk a caller should hand to [`SegmentPublisher::publish`] at once.
pub const PUBLISH_CHUNK_BYTES: usize = 64 * 1024;

/// The container one ingest publishes, decided by its first object.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    /// MPEG-TS (file ingest's pipe, HLS `.ts` segments).
    Ts,
    /// Fragmented MP4 objects (HLS `.mp4`/`.m4s`).
    Fmp4,
}

impl Container {
    /// A fresh demux for this container.
    pub fn demux(self) -> Box<dyn SegmentDemux> {
        match self {
            Self::Ts => Box::new(TsDemux::default()),
            Self::Fmp4 => Box::new(fmp4::Fmp4Demux::default()),
        }
    }
}

/// Why one step of a demux cannot be used. A session maps each variant to the
/// response the object gets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MediaRejection {
    /// The codec or feature is not implemented (`415`).
    Unsupported(String),
    /// The bytes are not a usable object (`400`).
    Malformed(String),
    /// The object is over its kind's bound (`413`).
    TooLarge(String),
}

impl MediaRejection {
    /// The reason, as it is reported to the encoder and the log.
    pub fn message(&self) -> &str {
        match self {
            Self::Unsupported(message) | Self::Malformed(message) | Self::TooLarge(message) => {
                message
            }
        }
    }
}

impl std::fmt::Display for MediaRejection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// One segment container's demux. What differs between containers (init
/// objects, object ends, discontinuities) is a method here, so the publisher
/// core never inspects the bytes it carries.
pub trait SegmentDemux: Send {
    /// A new object starts; `object` is all of it. HLS objects arrive whole,
    /// so a container that needs a whole object (fMP4) demuxes from this and
    /// lets the chunks that follow pace the release. MPEG-TS ignores it.
    fn begin_object(&mut self, _object: &Bytes) -> Result<(), MediaRejection> {
        Ok(())
    }

    /// The next chunk of the current object (or of the stream) arrived:
    /// demux what it completes.
    fn feed(&mut self, chunk: &[u8]) -> Result<(), MediaRejection>;

    /// Move demuxed packets into `out`; returns how many moved.
    fn drain_into(&mut self, out: &mut Vec<MediaPacket>) -> usize;

    /// The current object ended. A container that needs whole objects rejects
    /// one that ends mid-box here; MPEG-TS is a continuous stream and does not.
    fn end_object(&mut self) -> Result<(), MediaRejection>;

    /// End of the stream: forward whatever the demux still holds.
    fn flush(&mut self);

    /// Drop what the current object demuxed but has not released, so a
    /// failed segment does not publish the rest of itself. What the container
    /// keeps across objects (an fMP4 init, a TS stream's continuity) stays.
    fn discard_object(&mut self) {}

    /// The next bytes start an unrelated stream.
    fn discontinuity(&mut self);

    /// Apply the playlist's `EXT-X-MAP` init object. MPEG-TS has none.
    fn apply_init(&mut self, _bytes: &[u8], _max_bytes: usize) -> Result<(), MediaRejection> {
        Err(MediaRejection::Unsupported(
            "this ingest container has no init segment".to_string(),
        ))
    }

    /// The one-time probe: codecs, tracks and parameter sets.
    fn take_probe(&mut self) -> Option<DemuxProbe>;
}

/// MPEG-TS: the streaming demuxer every TS ingest already used.
#[derive(Default)]
pub struct TsDemux(TsDemuxer);

impl SegmentDemux for TsDemux {
    fn feed(&mut self, chunk: &[u8]) -> Result<(), MediaRejection> {
        self.0.feed(chunk);
        Ok(())
    }

    fn drain_into(&mut self, out: &mut Vec<MediaPacket>) -> usize {
        self.0.drain_into(out)
    }

    fn end_object(&mut self) -> Result<(), MediaRejection> {
        Ok(())
    }

    fn flush(&mut self) {
        self.0.flush();
    }

    fn discontinuity(&mut self) {
        self.0 = TsDemuxer::new();
    }

    fn take_probe(&mut self) -> Option<DemuxProbe> {
        self.0.take_probe()
    }
}

/// Where one registered ingest attempt publishes. Resolved once per attempt.
pub struct PublishTarget {
    engine: Arc<MediaEngine>,
    pipeline_id: String,
    registration: IngestRegistration,
    ring: Arc<RingBuffer>,
    bytes_received: Arc<AtomicU64>,
    /// The session's first-media wall clock, shared with the ingest state
    /// (see `ActiveIngest::first_media_ms`).
    first_media_ms: Arc<AtomicU64>,
    metrics: Arc<StageMetrics>,
    last_progress_ms: Arc<AtomicU64>,
    keyframe_times: Arc<std::sync::Mutex<Vec<i64>>>,
}

impl PublishTarget {
    /// Resolve the attempt's counters; fails once the attempt is no longer
    /// the input's current session.
    pub async fn open(
        engine: Arc<MediaEngine>,
        pipeline_id: String,
        registration: IngestRegistration,
        ring: Arc<RingBuffer>,
    ) -> Result<Self, String> {
        let (bytes_received, first_media_ms, metrics, last_progress_ms, keyframe_times) = engine
            .with_ingest_session(&registration, |ingest| {
                (
                    ingest.bytes_received.clone(),
                    ingest.first_media_ms.clone(),
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
            first_media_ms,
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
pub struct IngestTimestamps {
    continuous: ContinuousTimestampState,
    promotion: InputTimestampMapper,
}

/// Demux and forwarding state for one ingest attempt.
pub struct SegmentPublisher {
    demux: Box<dyn SegmentDemux>,
    packets: Vec<MediaPacket>,
    probe_sent: bool,
    timestamps: IngestTimestamps,
    /// Highest continuity-adjusted DTS demuxed so far (milliseconds).
    media_time_ms: Option<i64>,
}

impl SegmentPublisher {
    pub fn new(demux: Box<dyn SegmentDemux>, timestamps: IngestTimestamps) -> Self {
        Self {
            demux,
            packets: Vec::with_capacity(MEDIA_PRODUCER_BATCH_PACKETS),
            probe_sent: false,
            timestamps,
            media_time_ms: None,
        }
    }

    pub fn into_timestamps(self) -> IngestTimestamps {
        self.timestamps
    }

    /// The media time this publisher has reached: the highest DTS it has
    /// demuxed, after continuity. Callers pace against it; the publisher
    /// itself never waits on the clock.
    pub fn media_time_ms(&self) -> Option<i64> {
        self.media_time_ms
    }

    /// Demux one chunk, forward the packets it completes, count it, yield.
    pub async fn publish(
        &mut self,
        target: &PublishTarget,
        chunk: &[u8],
    ) -> Result<(), MediaRejection> {
        self.demux.feed(chunk)?;
        self.forward(target);
        self.take_probe(target).await;

        let read = chunk.len() as u64;
        let now_ms = MediaEngine::now_epoch_ms();
        crate::media::engine::note_media_bytes(
            &target.bytes_received,
            &target.first_media_ms,
            read,
            now_ms,
        );
        target.metrics.record_in(read);
        target.last_progress_ms.store(now_ms, Ordering::Relaxed);
        tokio::task::yield_now().await;
        Ok(())
    }

    /// A new object starts (an HLS segment); its chunks follow through
    /// [`Self::publish`].
    pub fn begin_object(&mut self, object: &Bytes) -> Result<(), MediaRejection> {
        self.demux.begin_object(object)
    }

    /// The current object ended: forward what its last bytes completed.
    pub fn end_object(&mut self, target: &PublishTarget) -> Result<(), MediaRejection> {
        self.demux.end_object()?;
        self.forward(target);
        Ok(())
    }

    /// Apply the playlist's `EXT-X-MAP` init object: the tracks, codecs and
    /// parameter sets every segment of that map is demuxed with. A different
    /// init rebuilds the demuxer; the session flags the discontinuity.
    pub async fn apply_init(
        &mut self,
        target: &PublishTarget,
        bytes: &[u8],
        max_bytes: usize,
    ) -> Result<(), MediaRejection> {
        self.demux.apply_init(bytes, max_bytes)?;
        // A new init may change resolution, profile or the audio track: its
        // probe replaces the recorded metadata, so the one-shot gate opens.
        self.probe_sent = false;
        self.take_probe(target).await;
        Ok(())
    }

    /// End of the stream: forward the PES each stream still holds.
    pub fn flush(&mut self, target: &PublishTarget) {
        self.demux.flush();
        self.forward(target);
    }

    /// The current object could not be demuxed. The segment is dropped:
    /// what it had demuxed but not yet published is discarded, and the
    /// timeline is re-anchored for the next segment, like a discontinuity.
    pub fn discard_failed_object(&mut self) {
        self.demux.discard_object();
        self.packets.clear();
        self.timestamps.continuous.mark_discontinuity();
    }

    /// The next bytes start an unrelated stream (HLS discontinuity or an
    /// encoder restart): finish the current one and re-anchor its timestamps
    /// right after the last forwarded ones. A container whose fragments are
    /// anchored by their own `tfdt` keeps its init and needs no other reset.
    pub fn discontinuity(&mut self, target: &PublishTarget) {
        self.flush(target);
        self.demux.discontinuity();
        self.timestamps.continuous.mark_discontinuity();
    }

    /// The stream's one-time probe becomes the ingest's metadata. Parameter
    /// sets reach the ring from the keyframes `forward` publishes (an fMP4
    /// demux puts its init's `avcC` SPS/PPS ahead of each keyframe).
    async fn take_probe(&mut self, target: &PublishTarget) {
        if self.probe_sent {
            return;
        }
        let Some(probe) = self.demux.take_probe() else {
            return;
        };
        self.probe_sent = true;
        record_probe(target, probe).await;
    }

    fn forward(&mut self, target: &PublishTarget) {
        if self.demux.drain_into(&mut self.packets) == 0 {
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
///
/// The video/audio metadata is written last: it is what opens an output's
/// start gate (`MediaEngine::ingest_tracks_ready`), so the audio tracks, the
/// track selection and the sequence header must already be in place.
async fn record_probe(target: &PublishTarget, probe: DemuxProbe) {
    let engine = &target.engine;
    let first_audio = probe.audio_tracks.first().cloned();
    let selected_video_track_index = probe.video.as_ref().map(|_| 0);
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
    engine
        .update_ingest_session_meta(
            &target.pipeline_id,
            &target.registration,
            probe.video,
            first_audio,
            None,
        )
        .await;
    engine
        .note_ingest_session_probed(&target.registration)
        .await;
}
