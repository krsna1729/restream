//! One source's media bytes to a pipeline ring, shared by every ingest whose
//! source is a byte stream: file ingest's FFmpeg pipe (MPEG-TS) and HLS push
//! (MPEG-TS or fragmented MP4).
//!
//! The container lives behind [`SegmentDemux`]; continuity, preview, the input
//! gate, promotion, parameter sets, ring push and the counters are one core
//! for both.
//!
//! An HLS session that follows a variant and its separate audio rendition
//! publishes both through one publisher: each rendition has its own demux
//! and probe, and their packets share one continuity state and are merged in
//! DTS order before they reach the ring (see [`SegmentPublisher::interleave`]).
//!
//! Runs on the MEDIA executor. Each `publish` call takes one bounded chunk
//! (at most 64 KiB), forwards what it completes, and yields, so a source that
//! is always ready cannot monopolize a shared media worker.

mod continuity;
pub mod fmp4;
#[cfg(test)]
#[path = "ingest_publisher/rendition_tests.rs"]
mod rendition_tests;
#[cfg(test)]
#[path = "ingest_publisher/tests.rs"]
mod tests;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use continuity::ContinuousTimestampState;

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::input_gate::{InputPacketBoundary, InputTimestampMapper};
use crate::media::stage_metrics::StageMetrics;
use restream_core::{MediaPacket, MediaType};
use restream_mpegts::{DemuxProbe, TsDemuxer};
use restream_sync::{MEDIA_PRODUCER_BATCH_PACKETS, RingBuffer};

/// Largest chunk a caller should hand to [`SegmentPublisher::publish`] at once.
pub const PUBLISH_CHUNK_BYTES: usize = 64 * 1024;

/// What a discontinuity mark means for a rendition's timeline.
///
/// The ordering pass classifies every mark before it releases the segment:
/// a timeline break (`EXT-X-DISCONTINUITY`, a new epoch) is shared by every
/// rendition and counted as a generation; a clock-keeping mark (a variant
/// switch, a changed init) starts unrelated bytes on the same source clock.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiscontinuityKind {
    /// The source clock continues: another rendition whose raw time is nearby
    /// lends its offset, so the pair stays aligned.
    Clock,
    /// The timeline broke at `generation`, which counts the breaks every
    /// rendition crosses. Only a rendition that crossed that same generation
    /// lends its offset, whatever the raw times.
    Timeline { generation: u64 },
}

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

/// One rendition's demux, its last probe, and (when renditions are merged)
/// its timeline and its packets that wait for the others.
struct Rendition {
    demux: Box<dyn SegmentDemux>,
    /// This demux's probe was taken; an applied init clears it.
    probe_taken: bool,
    probe: Option<DemuxProbe>,
    /// Continuity-adjusted packets not yet forwarded, in DTS order.
    queue: VecDeque<MediaPacket>,
    /// The timeline of a rendition after the first. Rendition 0's is
    /// `IngestTimestamps::continuous`, which outlives the publisher.
    continuous: ContinuousTimestampState,
    /// It had nothing queued when the others' wait for it ran out: they no
    /// longer wait for it until it delivers again.
    absent: bool,
}

impl Rendition {
    fn new(demux: Box<dyn SegmentDemux>) -> Self {
        Self {
            demux,
            probe_taken: false,
            probe: None,
            queue: VecDeque::new(),
            continuous: ContinuousTimestampState::joining(),
            absent: false,
        }
    }
}

/// Demux and forwarding state for one ingest attempt.
pub struct SegmentPublisher {
    /// Never empty; rendition 0 is the only one outside HLS.
    renditions: Vec<Rendition>,
    /// The rendition the current object belongs to.
    current: usize,
    /// Renditions whose packets the merge waits for (see `interleave`).
    expected: usize,
    /// How far ahead of a missing rendition the others may queue before they
    /// are forwarded without it.
    window_ms: i64,
    packets: Vec<MediaPacket>,
    /// A rendition probed since the merged probe was last recorded.
    probe_pending: bool,
    /// Packets were forwarded without every expected rendition: record the
    /// probe of those present instead of waiting for the rest.
    probe_forced: bool,
    /// An expected rendition not even created yet when a wait ran out: as
    /// `Rendition::absent`, until it is.
    unborn_absent: bool,
    timestamps: IngestTimestamps,
    /// Highest continuity-adjusted DTS forwarded so far (milliseconds).
    media_time_ms: Option<i64>,
}

impl SegmentPublisher {
    pub fn new(demux: Box<dyn SegmentDemux>, timestamps: IngestTimestamps) -> Self {
        Self {
            renditions: vec![Rendition::new(demux)],
            current: 0,
            expected: 1,
            window_ms: 0,
            packets: Vec::with_capacity(MEDIA_PRODUCER_BATCH_PACKETS),
            probe_pending: false,
            probe_forced: false,
            unborn_absent: false,
            timestamps,
            media_time_ms: None,
        }
    }

    /// The objects that follow belong to `rendition`, demuxed by its own
    /// demux (`demux` builds it the first time).
    pub fn select_rendition(
        &mut self,
        rendition: usize,
        demux: impl FnOnce() -> Box<dyn SegmentDemux>,
    ) {
        if rendition >= self.renditions.len() {
            self.renditions.push(Rendition::new(demux()));
            self.unborn_absent = false;
        }
        self.current = rendition.min(self.renditions.len().saturating_sub(1));
    }

    /// Merge `renditions` streams (a variant and its separate audio) in DTS
    /// order: a packet is forwarded once every one of them has queued media
    /// at or after it, or once the queued media runs `window` past the
    /// oldest packet (a rendition that stalls or never comes). The variant
    /// contributes its video and a separate rendition its audio: what else
    /// each carries would alias the other's track 0. One rendition forwards
    /// as it demuxes.
    /// When fewer renditions are expected than before (the master dropped
    /// one, or a pull stopped following its audio), what waited only for
    /// them leaves now rather than with the next segment, which may never
    /// come.
    pub fn interleave(&mut self, renditions: usize, window: Duration, target: &PublishTarget) {
        let expected = renditions.max(1);
        let shrank = expected < self.expected;
        self.expected = expected;
        self.window_ms = i64::try_from(window.as_millis()).unwrap_or(i64::MAX);
        if shrank {
            self.release_interleaved(false);
            self.forward_batch(target);
        }
    }

    pub fn into_timestamps(self) -> IngestTimestamps {
        self.timestamps
    }

    /// The media time this publisher has reached: the highest DTS it has
    /// forwarded, after continuity. Callers pace against it; the publisher
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
        self.demux().feed(chunk)?;
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
        self.demux().begin_object(object)
    }

    /// The current object ended: forward what its last bytes completed.
    pub fn end_object(&mut self, target: &PublishTarget) -> Result<(), MediaRejection> {
        self.demux().end_object()?;
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
        self.demux().apply_init(bytes, max_bytes)?;
        // A new init may change resolution, profile or the audio track: its
        // probe replaces the recorded metadata, so the one-shot gate opens.
        if let Some(rendition) = self.renditions.get_mut(self.current) {
            rendition.probe_taken = false;
        }
        self.take_probe(target).await;
        Ok(())
    }

    /// End of the stream: forward the PES each stream still holds, and
    /// every packet that waited for another rendition.
    pub fn flush(&mut self, target: &PublishTarget) {
        let current = self.current;
        for rendition in 0..self.renditions.len() {
            self.current = rendition;
            self.flush_current(target);
        }
        self.current = current;
        if self.renditions.len() > 1 {
            self.release_interleaved(true);
            self.forward_batch(target);
        }
    }

    fn flush_current(&mut self, target: &PublishTarget) {
        self.demux().flush();
        self.forward(target);
    }

    /// The current object could not be demuxed. The segment is dropped:
    /// what it had demuxed but not yet published is discarded, and the
    /// timeline is re-anchored for the next segment, like a discontinuity.
    pub fn discard_failed_object(&mut self) {
        self.demux().discard_object();
        self.packets.clear();
        // The next segment continues the clock this one meant to: a timeline
        // break that was already marked stays pending (`Clock` never
        // downgrades it), and otherwise a nearby rendition lends its offset.
        self.continuity(self.current).mark_clock_discontinuity();
    }

    /// The next bytes start an unrelated stream. `kind` says whether the
    /// break starts a timeline generation shared by every rendition or keeps
    /// the source clock. Finish the current stream and re-anchor its
    /// timestamps right after the last forwarded ones. A container whose
    /// fragments are anchored by their own `tfdt` keeps its init and needs no
    /// other reset.
    pub fn reanchor(&mut self, target: &PublishTarget, kind: DiscontinuityKind) {
        self.flush_current(target);
        self.demux().discontinuity();
        match kind {
            DiscontinuityKind::Clock => self.continuity(self.current).mark_clock_discontinuity(),
            DiscontinuityKind::Timeline { generation } => self
                .continuity(self.current)
                .mark_timeline_break(generation),
        }
    }

    /// A rendition's timeline: each is re-anchored on its own discontinuities,
    /// so one rendition's re-anchor never moves the other's packets.
    fn continuity(&mut self, rendition: usize) -> &mut ContinuousTimestampState {
        match self.renditions.get_mut(rendition) {
            Some(other) if rendition > 0 => &mut other.continuous,
            _ => &mut self.timestamps.continuous,
        }
    }

    fn continuity_of(&self, rendition: usize) -> &ContinuousTimestampState {
        match self.renditions.get(rendition) {
            Some(other) if rendition > 0 => &other.continuous,
            _ => &self.timestamps.continuous,
        }
    }

    /// Settle a pending re-anchor of `rendition` before its next packet. A
    /// clock-keeping mark may take a same-generation rendition's offset
    /// when its last raw time is within the window (a variant switch, an init
    /// change). A timeline break may take it only from a rendition that
    /// crossed the same generation, whatever the raw times: the old offset
    /// would leave the break in the output as a gap. Otherwise the packet is
    /// anchored right after the latest timestamp forwarded by any rendition.
    fn align_rebase(&mut self, rendition: usize) {
        let state = self.continuity_of(rendition);
        if !state.rebase_pending() {
            return;
        }
        let pending_generation = state.pending_generation();
        let generation = pending_generation.unwrap_or_else(|| state.generation());
        let Some(first) = self.packets.first() else {
            return;
        };
        let raw = ContinuousTimestampState::continuity_timestamp_ms(first);
        let window = u64::try_from(self.window_ms).unwrap_or(0);
        let mut aligned = None;
        let mut latest = None;
        for index in 0..self.renditions.len() {
            let other = self.continuity_of(index);
            latest = latest.max(other.latest_ms());
            if index == rendition || aligned.is_some() || other.generation() != generation {
                continue;
            }
            let close = other
                .last_raw_ms()
                .is_some_and(|last| last.abs_diff(raw) <= window);
            if pending_generation.is_some() || close {
                aligned = Some(other.offset_ms());
            }
        }
        let offset = match (aligned, latest) {
            (Some(offset), _) => offset,
            (None, Some(latest)) => latest.saturating_add(1).saturating_sub(raw),
            (None, None) => 0,
        };
        self.continuity(rendition).rebase_to(offset);
    }

    fn demux(&mut self) -> &mut dyn SegmentDemux {
        // `current` is clamped into `renditions` by `select_rendition`, and
        // rendition 0 is made by `new` and never removed.
        let index = if self.current < self.renditions.len() {
            self.current
        } else {
            0
        };
        self.renditions[index].demux.as_mut()
    }

    /// The stream's one-time probe becomes the ingest's metadata. Parameter
    /// sets reach the ring from the keyframes `forward` publishes (an fMP4
    /// demux puts its init's `avcC` SPS/PPS ahead of each keyframe).
    ///
    /// With several renditions, the probe recorded is their merge (video from
    /// the variant, audio from the separate rendition), once every expected
    /// rendition has probed or packets had to be forwarded without one.
    async fn take_probe(&mut self, target: &PublishTarget) {
        if let Some(rendition) = self.renditions.get_mut(self.current)
            && !rendition.probe_taken
            && let Some(probe) = rendition.demux.take_probe()
        {
            rendition.probe_taken = true;
            rendition.probe = Some(probe);
            self.probe_pending = true;
        }
        if !self.probe_pending {
            return;
        }
        let probed = self
            .renditions
            .iter()
            .filter(|rendition| rendition.probe.is_some())
            .count();
        if probed < self.expected && !self.probe_forced {
            return;
        }
        self.probe_pending = false;
        self.probe_forced = false;
        let Some(probe) = merged_probe(&mut self.renditions, self.expected) else {
            return;
        };
        record_probe(target, probe).await;
    }

    fn forward(&mut self, target: &PublishTarget) {
        let current = self.current;
        let Some(rendition) = self.renditions.get_mut(current) else {
            return;
        };
        if rendition.demux.drain_into(&mut self.packets) == 0 {
            return;
        }
        let merged = self.renditions.len() > 1 || self.expected > 1;
        if !merged {
            for packet in &mut self.packets {
                self.timestamps.continuous.apply(packet);
            }
            self.forward_batch(target);
            return;
        }
        // The variant carries the video, a separate rendition the audio; what
        // each carries besides would alias the other's track 0. Left out:
        // rendition 0 once the master no longer names an audio rendition
        // (`expected` back to 1), which keeps its own audio. Before
        // continuity: a dropped packet must not become its stream's last
        // timestamp.
        let keep = if current == 0 {
            MediaType::Video
        } else {
            MediaType::Audio
        };
        if current > 0 || self.expected > 1 {
            self.packets.retain(|packet| packet.media_type == keep);
        }
        self.align_rebase(current);
        let state = match self.renditions.get_mut(current) {
            Some(other) if current > 0 => &mut other.continuous,
            _ => &mut self.timestamps.continuous,
        };
        for packet in &mut self.packets {
            state.apply(packet);
        }
        if let Some(rendition) = self.renditions.get_mut(current) {
            if !self.packets.is_empty() {
                rendition.absent = false;
            }
            rendition.queue.extend(self.packets.drain(..));
        }
        self.release_interleaved(false);
        self.forward_batch(target);
    }

    /// Move queued packets to `packets` in DTS order while every expected
    /// rendition has media queued, or (`all`, or the queued media spanning
    /// more than the window) regardless.
    fn release_interleaved(&mut self, all: bool) {
        loop {
            let mut oldest: Option<(usize, i64)> = None;
            let mut newest = i64::MIN;
            let mut waiting = false;
            for (index, rendition) in self.renditions.iter().enumerate() {
                match (rendition.queue.front(), rendition.queue.back()) {
                    (Some(front), Some(back)) => {
                        if oldest.is_none_or(|(_, dts)| front.dts < dts) {
                            oldest = Some((index, front.dts));
                        }
                        newest = newest.max(back.dts);
                    }
                    _ => waiting |= index < self.expected && !rendition.absent,
                }
            }
            if self.renditions.len() < self.expected && !self.unborn_absent {
                waiting = true;
            }
            let Some((index, dts)) = oldest else {
                return;
            };
            if waiting && !all {
                if newest.saturating_sub(dts) <= self.window_ms {
                    return;
                }
                // The wait ran out: what has nothing queued is absent, and
                // the others stop waiting for it until it delivers again, so
                // a rendition that stalls or never comes costs one window,
                // not one per packet.
                self.probe_forced = true;
                let expected = self.expected;
                for rendition in self.renditions.iter_mut().take(expected) {
                    if rendition.queue.is_empty() {
                        rendition.absent = true;
                    }
                }
                self.unborn_absent = true;
            }
            let Some(packet) = self
                .renditions
                .get_mut(index)
                .and_then(|rendition| rendition.queue.pop_front())
            else {
                return;
            };
            self.packets.push(packet);
        }
    }

    fn forward_batch(&mut self, target: &PublishTarget) {
        if self.packets.is_empty() {
            return;
        }
        let registration = &target.registration;
        // One comparison per batch: DTS increases per stream, and the last
        // packet of a demuxed batch is within one frame of its maximum.
        if let Some(last) = self.packets.last() {
            self.media_time_ms = self.media_time_ms.max(Some(last.dts));
        }
        if let Some(preview_ring) = registration.preview_ring.load_full() {
            preview_ring.push_batch_recording_config(self.packets.iter().cloned());
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
            if packet.media_type == MediaType::Video && packet.is_keyframe {
                let mut times = restream_sync::lock::lock(&target.keyframe_times);
                times.push(packet.pts);
                if times.len() > 30 {
                    times.remove(0);
                }
            }
        }
        if let Some(last) = self.packets.iter().max_by_key(|packet| packet.dts) {
            InputTimestampMapper::record_forwarded(last, &registration.last_forwarded_dts);
        }
        target
            .ring
            .push_drained_batch_recording_config(&mut self.packets);
    }
}

/// The probe to record: rendition 0's alone, or with several renditions the
/// video of the first that has video and the audio of the last that has
/// audio (a separate audio rendition outranks the variant's own).
fn merged_probe(renditions: &mut [Rendition], expected: usize) -> Option<DemuxProbe> {
    if expected <= 1 && renditions.len() <= 1 {
        return renditions
            .first_mut()
            .and_then(|rendition| rendition.probe.take());
    }
    let probes = || {
        renditions
            .iter()
            .filter_map(|rendition| rendition.probe.as_ref())
    };
    let video = probes().find(|probe| probe.video.is_some());
    let audio = probes()
        .filter(|probe| !probe.audio_tracks.is_empty())
        .last();
    let mut merged = video.or(audio)?.clone();
    if let Some(audio) = audio {
        merged.audio_tracks = audio.audio_tracks.clone();
    }
    Some(merged)
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
