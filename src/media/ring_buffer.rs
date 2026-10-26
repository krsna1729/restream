//! Lock-free single-producer multi-consumer ring buffer for media packet fan-out.
//!
//! # Memory Layout
//!
//! Packet slots are densely packed because readers only load them; cache-line
//! isolation is reserved for the producer-owned indexes that are actively
//! modified. This keeps the slot working set small enough for cache.
//!
//! # Packet Walk
//!
//! ```text
//! Ingest (RTMP/SRT demuxer)
//!   → push(MediaPacket)
//!     → ArcSwapOption::store() on slot[write_idx % capacity]
//!     → AtomicUsize::store(write_idx + 1, Release)
//!     → Notify::notify_waiters()
//!
//! Reader (egress / HLS / recording)
//!   → wait_for_data()  (Notify::notified().await)
//!   → pull()
//!     → ArcSwapOption::load_full() on slot[read_idx % capacity]
//!     → returns Arc<MediaPacket> (zero-copy, ref-counted)
//! ```
//!
//! # Capacity
//!
//! Default: 1024 slots, configurable with `RESTREAM_RING_CAPACITY`. Slots hold
//! demuxed media packets rather than fixed 188-byte TS packets, so retained
//! payload memory scales with compressed frame size. The default gives tens of
//! seconds of burst tolerance for common live inputs while bounding memory
//! earlier than the old 4096-slot default.
//!
//! # Overflow & Recovery
//!
//! When a reader falls behind by ≥ capacity slots, `pull()` detects the gap
//! and calls `fast_forward()`, which jumps to the most recent keyframe via
//! an O(1) atomic read of `last_keyframe_idx`. This avoids decoding artifacts
//! by always resuming from an IDR frame.
//!
//! # Why ArcSwap
//!
//! Multiple readers call `load_full()` concurrently without locking. That
//! eliminates per-slot `RwLock` contention that would otherwise dominate at
//! hundreds of concurrent egress readers.
//!
//! # Single writer
//!
//! `push()` is not exclusive in the type system: a second caller compiles. The
//! ingest contract is runtime discipline. [`super::input_gate::InputPacketGate`]
//! plus selection serialized by [`crate::media::engine::MediaEngine`]
//! (`select_pipeline_input`, `selection_lock`) ensure only the selected input
//! forwards into a pipeline ring. HTTP promotion persists the choice and calls
//! the engine directly; it does not wait for the reconciler. Monotonic
//! `write_idx` orders slots for readers; it does not prove a unique producer.
//!
//! # Frame units
//!
//! Every slot also records a [`FrameUnit`]: the configuration generation of
//! the packet's track at publication (see [`frame_units`]).
//! [`RingBuffer::frame_unit`] and [`RingBuffer::track_config_at`] answer by
//! sequence in O(1) without a lock; the unit word is tagged with its
//! sequence, so a sequence the producer has overwritten (even mid-batch,
//! before the write index moves) reads as not retained. Publication never
//! parses payloads: each producer
//! records configurations it already has, right before the packet that
//! carries them.

use arc_swap::ArcSwapOption;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use tracing::debug;

use super::packet::{MediaPacket, MediaType, PayloadFormat};

mod frame_units;
mod reader;
mod subscribers;

use frame_units::TrackConfigs;
pub use frame_units::{
    CONFIG_HISTORY, CONFIG_TRACKS_PER_TYPE, FrameUnit, NotRetained, TrackConfig,
    VideoConfigFollower,
};
pub use reader::{Reader, ReaderInfo, ReaderSnapshot};
pub use subscribers::{PublishSubscribers, PublishWake};

pub const DEFAULT_RING_CAPACITY: usize = 1024;

/// Max media packets a runtime reader processes per hot-loop burst.
pub const MEDIA_PULL_BURST_PACKETS: usize = 32;

/// Soft cap for producer-side publications into a packet ring.
pub const MEDIA_PRODUCER_BATCH_PACKETS: usize = MEDIA_PULL_BURST_PACKETS;

// Transcoder output rings hold demuxed frames from the FFmpeg child process.
// At 720p30 with one audio track, the packet rate is ~80 pkt/s; 512 slots
// ≈ 6.4 s of jitter headroom (above the 5 s requirement). I-frames from the
// CRF23 encoder are large (~30–50 KB each), so the per-slot payload size is
// much larger than the source ring — 512 slots already dominate memory at high
// bitrates. Scale-test evidence: no transcoder ring overflows across 15 cases.
pub const DEFAULT_TRANSCODER_RING_CAPACITY: usize = 512;

pub struct RingSlot {
    data: ArcSwapOption<MediaPacket>,
    published_at_us: AtomicU64,
    /// Packed [`FrameUnit`] of the packet in `data`; 0 before the first write.
    unit: AtomicU64,
}

#[repr(align(64))]
pub struct AlignedAtomicUsize {
    val: AtomicUsize,
}

/// Compact histogram for `pull_burst` yield sizes.
///
/// Buckets cover [1], [2], [3-4], [5-8], [9-16], [17-32].
/// Burst size 0 (nothing available) is not counted — callers skip stat
/// recording when `available == 0`.
pub const BURST_HIST_BUCKETS: usize = 6;
const fn burst_bucket(n: usize) -> usize {
    match n {
        1 => 0,
        2 => 1,
        3..=4 => 2,
        5..=8 => 3,
        9..=16 => 4,
        _ => 5,
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PayloadStats {
    pub slots: usize,
    pub payload_bytes: usize,
    pub video_bytes: usize,
    pub audio_bytes: usize,
    pub min_payload_bytes: usize,
    pub max_payload_bytes: usize,
}

pub struct RingBuffer {
    slots: Vec<RingSlot>,
    write_idx: AlignedAtomicUsize,
    last_keyframe_idx: AlignedAtomicUsize,
    /// Cumulative payload bytes published: the offered load for delivery
    /// telemetry. Single producer, so a relaxed load/store suffices.
    published_bytes: AtomicU64,
    capacity: usize,
    created_at: Instant,
    notify: Arc<tokio::sync::Notify>,
    pub readers: std::sync::Mutex<Vec<std::sync::Weak<ReaderInfo>>>,
    /// Current hint: 0 unknown/absent, 1 initial hint, 2 H.264, 3 HEVC.
    /// The immutable initial string preserves arbitrary producer codec names
    /// and the borrowed getter; later supported PMT changes use static names.
    codec_hint: AtomicU8,
    initial_codec_hint: std::sync::OnceLock<String>,
    /// Per-track codec configurations with generations (video: raw Annex B
    /// parameter sets, so late-joining stages can seed decoders before the
    /// next keyframe). Written by the producer only.
    track_configs: TrackConfigs,
    /// Audio tracks metadata of packets in this ring.
    /// Stored behind ArcSwapOption so it can be updated when a publisher reconnects
    /// with a different track configuration (OnceLock would silently ignore updates).
    pub audio_tracks: ArcSwapOption<Vec<crate::media::metadata::AudioMeta>>,
    /// Estimated packet rate (pkt/s) set once after stream probe.
    /// Used by telemetry to compute buffer depth in seconds.
    pub estimated_pkt_rate: std::sync::atomic::AtomicU32,
    end_of_stream: AtomicBool,
    /// Forwarding pointer set when this ring is superseded by a larger one.
    ///
    /// When `adapt_pipeline_ring` grows the source ring, it stores the new ring
    /// here and fires `self.notify.notify_waiters()`.  Readers on the old ring
    /// wake up, drain any remaining unread slots, then follow `next` to the new
    /// ring.  External egress connections never disconnect — they just see a
    /// sub-millisecond hiccup as readers move to the new ring.
    pub next: ArcSwapOption<RingBuffer>,
    /// Woken by the producer after each publication (see [`PublishWake`]).
    /// Created on first subscription, so a ring nobody subscribes to pays
    /// one atomic load per publication.
    pub publish_subscribers: std::sync::OnceLock<Arc<PublishSubscribers>>,
    /// Woken by the producer only when track metadata (codec hint, track
    /// configuration, audio tracks) changes. Created on first subscription,
    /// so a ring nobody watches pays no wake work for metadata changes.
    metadata_subscribers: std::sync::OnceLock<Arc<PublishSubscribers>>,
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            slots.push(RingSlot {
                data: ArcSwapOption::empty(),
                published_at_us: AtomicU64::new(0),
                unit: AtomicU64::new(0),
            });
        }
        debug!(capacity, "ring buffer created");
        Self {
            slots,
            write_idx: AlignedAtomicUsize {
                val: AtomicUsize::new(0),
            },
            last_keyframe_idx: AlignedAtomicUsize {
                // usize::MAX is the sentinel meaning "no keyframe seen yet".
                // This disambiguates from a real keyframe at slot 0 (which
                // would also produce index 0 if we started from AtomicUsize::new(0)).
                val: AtomicUsize::new(usize::MAX),
            },
            published_bytes: AtomicU64::new(0),
            capacity,
            created_at: Instant::now(),
            notify: Arc::new(tokio::sync::Notify::new()),
            readers: std::sync::Mutex::new(Vec::new()),
            codec_hint: AtomicU8::new(0),
            initial_codec_hint: std::sync::OnceLock::new(),
            track_configs: TrackConfigs::new(),
            audio_tracks: ArcSwapOption::empty(),
            estimated_pkt_rate: std::sync::atomic::AtomicU32::new(0),
            end_of_stream: AtomicBool::new(false),
            next: ArcSwapOption::empty(),
            publish_subscribers: std::sync::OnceLock::new(),
            metadata_subscribers: std::sync::OnceLock::new(),
        }
    }

    /// The publication subscribers of this ring and of every ring that
    /// replaces it. Subscribers hold this set rather than the ring, so a
    /// subscription never pins a superseded ring's packets.
    pub fn publication_subscribers(&self) -> Arc<PublishSubscribers> {
        Arc::clone(
            self.publish_subscribers
                .get_or_init(|| Arc::new(PublishSubscribers::new())),
        )
    }

    /// The metadata subscribers of this ring and of every ring that replaces
    /// it: woken only when the codec hint, a track configuration or the audio
    /// tracks change, not on every publication.
    pub fn metadata_subscribers(&self) -> Arc<PublishSubscribers> {
        Arc::clone(
            self.metadata_subscribers
                .get_or_init(|| Arc::new(PublishSubscribers::new())),
        )
    }

    /// One atomic load without subscribers; with them, one `ArcSwap` load and
    /// one call per subscriber. No allocation.
    #[inline]
    fn wake_publication_subscribers(&self) {
        if let Some(subscribers) = self.publish_subscribers.get() {
            subscribers.wake_all();
        }
    }

    /// Metadata-only wake: no publication subscriber runs, so an origin
    /// watcher sees codec/track changes without polling every packet.
    #[inline]
    fn wake_metadata_subscribers(&self) {
        if let Some(subscribers) = self.metadata_subscribers.get() {
            subscribers.wake_all();
        }
    }

    /// Create a ring whose write cursor starts at `start_write_idx` rather than 0.
    ///
    /// Used when growing a pipeline ring: the new ring continues the old ring's
    /// index sequence so that existing `Reader` instances (which remember their
    /// `read_idx`) can transparently migrate and resume without a gap.
    pub fn new_continuing(capacity: usize, start_write_idx: usize) -> Self {
        let ring = Self::new(capacity);
        ring.write_idx.val.store(start_write_idx, Ordering::Relaxed);
        // The last_keyframe sentinel is irrelevant to new readers migrating from
        // the old ring; they will establish a new baseline from the first keyframe.
        ring
    }

    /// Seed this continuing ring with the readable tail from `old_ring`.
    ///
    /// Adaptive resizing keeps the global write-index timeline intact so active
    /// readers can migrate without gaps. Late readers created after the resize
    /// also need a real preroll window; otherwise `fast_forward()` has no
    /// keyframe to target and starts them at the live edge. This copies only the
    /// still-readable tail (`capacity - 1` packets) into the matching indices of
    /// the grown ring without advancing the write cursor, together with their
    /// frame units and the track configurations, so sequence lookups and
    /// generations continue across the resize.
    pub fn seed_readable_tail_from(&self, old_ring: &RingBuffer) -> usize {
        let old_write_idx = old_ring.get_write_idx();
        let tail_capacity = self
            .capacity
            .saturating_sub(1)
            .min(old_ring.capacity.saturating_sub(1));
        let start_idx = old_write_idx.saturating_sub(tail_capacity);
        let mut copied = 0usize;
        let mut last_keyframe_idx = usize::MAX;

        for idx in start_idx..old_write_idx {
            let Some(packet) = old_ring.read_at(idx) else {
                continue;
            };
            let slot_idx = idx % self.capacity;
            let unit = old_ring.slots[idx % old_ring.capacity]
                .unit
                .load(Ordering::Acquire);
            self.slots[slot_idx]
                .published_at_us
                .store(self.elapsed_us().max(1), Ordering::Release);
            self.slots[slot_idx].data.store(Some(packet.clone()));
            self.slots[slot_idx].unit.store(unit, Ordering::Release);
            if packet.media_type == MediaType::Video && packet.is_keyframe {
                last_keyframe_idx = idx;
            }
            copied += 1;
        }

        if last_keyframe_idx != usize::MAX {
            self.last_keyframe_idx
                .val
                .store(last_keyframe_idx, Ordering::Release);
        }
        self.track_configs.copy_from(&old_ring.track_configs);
        self.write_idx.val.store(old_write_idx, Ordering::Release);
        copied
    }

    /// Forward all waiting readers to `new_ring` and mark this ring as superseded.
    ///
    /// After this call, `self` receives no new data (the caller must redirect the
    /// producer to `new_ring`).  Readers currently blocked in `wait_for_data` are
    /// woken; they drain any remaining slots in `self`, then follow `self.next` to
    /// `new_ring` automatically.
    pub fn seal_and_forward(&self, new_ring: Arc<RingBuffer>) {
        // The replacement inherits the publication subscribers before it is
        // reachable, so no publication on it can miss them.
        // A fresh replacement has no set of its own yet, so `set` succeeds.
        let _ = new_ring
            .publish_subscribers
            .set(self.publication_subscribers());
        if let Some(metadata) = self.metadata_subscribers.get() {
            let _ = new_ring.metadata_subscribers.set(Arc::clone(metadata));
        }
        self.next.store(Some(new_ring));
        // Wake all readers blocked on this ring so they can discover `next`.
        self.notify.notify_waiters();
        self.wake_publication_subscribers();
        self.wake_metadata_subscribers();
    }

    pub fn mark_end_of_stream(&self) {
        self.end_of_stream.store(true, Ordering::Release);
        self.notify.notify_waiters();
        self.wake_publication_subscribers();
        self.wake_metadata_subscribers();
    }

    pub fn is_end_of_stream(&self) -> bool {
        self.end_of_stream.load(Ordering::Acquire)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of readers whose `Arc<ReaderInfo>` is still alive.
    pub fn active_reader_count(&self) -> usize {
        let mut readers = crate::sync::lock(&self.readers);
        readers.retain(|w| w.upgrade().is_some());
        readers.len()
    }

    /// Store the probed packet rate so telemetry can show buffer depth in seconds.
    pub fn set_estimated_pkt_rate(&self, pkt_per_sec: f64) {
        self.estimated_pkt_rate
            .store(pkt_per_sec.round() as u32, Ordering::Relaxed);
    }

    /// Probed packet rate (0 when the probe hasn't run yet). The shared
    /// SRT TS muxer sizes its retention ring from this rate at creation.
    pub fn estimated_pkt_rate(&self) -> f64 {
        self.estimated_pkt_rate.load(Ordering::Relaxed) as f64
    }

    /// Buffer depth in seconds: how long the ring can absorb an ingest interruption.
    /// Returns `None` if the packet rate hasn't been set yet.
    pub fn buffer_depth_secs(&self) -> Option<f64> {
        let rate = self.estimated_pkt_rate.load(Ordering::Relaxed);
        if rate == 0 {
            return None;
        }
        Some(self.capacity as f64 / rate as f64)
    }

    /// Set or replace the supported video codec hint. Called only on metadata
    /// changes, never per packet; an unchanged hint does not wake observers.
    pub fn set_codec_hint(&self, codec: &str) {
        use crate::domain::output_spec::VideoCodecKind;
        let initial = self.initial_codec_hint.get_or_init(|| codec.to_string());
        let hint = if codec.is_empty() {
            0
        } else if codec == initial {
            1
        } else {
            match VideoCodecKind::from_codec_name(codec) {
                VideoCodecKind::H264 => 2,
                VideoCodecKind::Hevc => 3,
                VideoCodecKind::Unknown => 1,
            }
        };
        if self.codec_hint.swap(hint, Ordering::AcqRel) != hint {
            self.wake_metadata_subscribers();
        }
    }

    /// Return the codec hint if set, or empty string.
    pub fn codec_hint_str(&self) -> &str {
        match self.codec_hint.load(Ordering::Acquire) {
            0 => "",
            2 => "h264",
            3 => "hevc",
            _ => self
                .initial_codec_hint
                .get()
                .map(String::as_str)
                .unwrap_or(""),
        }
    }

    /// Remove a producer's video track, including its current decoder
    /// configuration. Historical configurations remain available to readers
    /// draining packets published before this boundary.
    pub(crate) fn remove_video_track(&self) {
        self.track_configs.clear(MediaType::Video, 0);
        self.codec_hint.store(0, Ordering::Release);
        self.wake_metadata_subscribers();
    }

    /// Record `bytes` as the configuration of one track. Same bytes keep the
    /// track's generation, changed bytes start the next one; packets
    /// published afterwards carry it. Producer only. Returns the generation
    /// (0 for a track index outside the table).
    pub fn set_track_config(&self, media_type: MediaType, track_index: u32, bytes: Vec<u8>) -> u32 {
        if bytes.is_empty() {
            return self.track_configs.generation(media_type, track_index);
        }
        let previous = self.track_configs.generation(media_type, track_index);
        let generation = self.track_configs.set(media_type, track_index, bytes);
        if generation != previous {
            self.wake_metadata_subscribers();
        }
        generation
    }

    /// The current configuration of one track, if any was recorded.
    pub fn track_config(
        &self,
        media_type: MediaType,
        track_index: u32,
    ) -> Option<Arc<TrackConfig>> {
        self.track_configs.current(media_type, track_index)
    }

    /// Record raw Annex B parameter sets as the video track's configuration.
    pub fn set_video_parameter_sets(&self, parameter_sets: Vec<u8>) {
        self.set_track_config(MediaType::Video, 0, parameter_sets);
    }

    /// The video track's current configuration with its generation.
    pub fn video_config(&self) -> Option<Arc<TrackConfig>> {
        self.track_config(MediaType::Video, 0)
    }

    /// Returns a snapshot copy of the video track's current parameter sets.
    pub fn video_parameter_sets(&self) -> Option<Vec<u8>> {
        self.video_config().map(|config| config.bytes.to_vec())
    }

    /// The frame unit recorded for the packet published at `sequence`, or
    /// `None` when it is not published yet or no longer retained (the same
    /// window as `Reader::pull`).
    pub fn frame_unit(&self, sequence: usize) -> Option<FrameUnit> {
        let write_idx = self.write_idx.val.load(Ordering::Acquire);
        if sequence >= write_idx || write_idx - sequence >= self.capacity {
            return None;
        }
        // A publication overwrites the slot before it stores its write index
        // (a batch, several slots before), so a re-check of the write index
        // cannot see the overwrite; the word's sequence tag does.
        FrameUnit::unpack(
            self.slots[sequence % self.capacity]
                .unit
                .load(Ordering::Acquire),
            sequence,
        )
    }

    /// The configuration that applies to the packet published at `sequence`,
    /// whose track is `media_type`/`track_index`: `Ok(None)` when its
    /// producer had recorded none, `Err(NotRetained)` when the packet or its
    /// configuration generation is no longer retained.
    pub fn track_config_at(
        &self,
        sequence: usize,
        media_type: MediaType,
        track_index: u32,
    ) -> Result<Option<Arc<TrackConfig>>, NotRetained> {
        let unit = self.frame_unit(sequence).ok_or(NotRetained)?;
        if unit.config_generation == 0 {
            return Ok(None);
        }
        self.track_configs
            .get(media_type, track_index, unit.config_generation)
            .map(Some)
            .ok_or(NotRetained)
    }

    /// The unit word of the packet being published at `sequence`. Producer
    /// only.
    #[inline]
    fn next_frame_unit(&self, packet: &MediaPacket, sequence: usize) -> u64 {
        FrameUnit {
            config_generation: self
                .track_configs
                .generation(packet.media_type, packet.track_index),
        }
        .pack(sequence)
    }

    /// Record the complete in-band parameter sets a Raw video packet carries
    /// as its track's configuration.
    fn record_in_band_config(&self, packet: &MediaPacket) {
        if packet.media_type == MediaType::Video
            && packet.format == PayloadFormat::Raw
            && let Some(parameter_sets) = super::codec::annexb_parameter_sets(&packet.payload)
        {
            self.set_track_config(MediaType::Video, packet.track_index, parameter_sets);
        }
    }

    /// Set (or update) the audio track metadata for this ring.
    /// An empty `tracks` vec clears the metadata, signalling "not yet known" to
    /// downstream stages — this is used by RTMP ingest on publisher reconnect.
    pub fn set_audio_tracks(&self, tracks: Vec<crate::media::metadata::AudioMeta>) {
        for track_index in 0..CONFIG_TRACKS_PER_TYPE as u32 {
            if !tracks.iter().any(|track| track.track_index == track_index) {
                self.track_configs.clear(MediaType::Audio, track_index);
            }
        }
        let count = tracks.len();
        if tracks.is_empty() {
            self.audio_tracks.store(None);
            debug!("ring audio tracks cleared");
        } else {
            self.audio_tracks.store(Some(std::sync::Arc::new(tracks)));
            debug!(count, "ring audio tracks set");
        }
        self.wake_metadata_subscribers();
    }

    /// The audio track metadata handle without copying it, for watchers that
    /// only compare identity (a change installs a new `Arc`).
    pub(crate) fn audio_tracks_snapshot(
        &self,
    ) -> Option<std::sync::Arc<Vec<crate::media::metadata::AudioMeta>>> {
        self.audio_tracks.load_full()
    }

    /// Returns a snapshot clone of the audio track metadata, or `None` if not yet known.
    /// Clones the inner Vec; `audio_tracks` is only read at stage startup, not on the
    /// hot packet path, so this clone is acceptable.
    pub fn audio_tracks(&self) -> Option<Vec<crate::media::metadata::AudioMeta>> {
        self.audio_tracks.load_full().map(|arc| (*arc).clone())
    }

    pub fn push(&self, packet: MediaPacket) {
        let idx = self.write_idx.val.load(Ordering::Relaxed);
        let slot_idx = idx % self.capacity;
        let is_keyframe = packet.media_type == MediaType::Video && packet.is_keyframe;
        let payload_bytes = packet.payload.len() as u64;

        let unit = self.next_frame_unit(&packet, idx);
        self.slots[slot_idx]
            .published_at_us
            .store(self.elapsed_us().max(1), Ordering::Release);
        self.slots[slot_idx].unit.store(unit, Ordering::Release);
        self.slots[slot_idx].data.store(Some(Arc::new(packet)));

        if is_keyframe {
            self.last_keyframe_idx.val.store(idx, Ordering::Release);
        }
        self.add_published_bytes(payload_bytes);

        self.write_idx.val.store(idx + 1, Ordering::Release);
        self.notify.notify_waiters();
        self.wake_publication_subscribers();
    }

    /// Publish a burst with one write-index release and one waiter notification.
    ///
    /// The ring is single-producer, so slots can be populated first and made
    /// visible together by the final release store. Returns the number of
    /// packets published.
    pub fn push_batch<I>(&self, packets: I) -> usize
    where
        I: IntoIterator<Item = MediaPacket>,
    {
        let start_idx = self.write_idx.val.load(Ordering::Relaxed);
        let mut count = 0usize;
        let mut payload_bytes = 0u64;

        for packet in packets {
            let idx = start_idx + count;
            let slot_idx = idx % self.capacity;
            let is_keyframe = packet.media_type == MediaType::Video && packet.is_keyframe;
            payload_bytes += packet.payload.len() as u64;

            let unit = self.next_frame_unit(&packet, idx);
            self.slots[slot_idx]
                .published_at_us
                .store(self.elapsed_us().max(1), Ordering::Release);
            self.slots[slot_idx].unit.store(unit, Ordering::Release);
            self.slots[slot_idx].data.store(Some(Arc::new(packet)));
            if is_keyframe {
                self.last_keyframe_idx.val.store(idx, Ordering::Release);
            }
            count += 1;
        }

        if count > 0 {
            self.add_published_bytes(payload_bytes);
            self.write_idx
                .val
                .store(start_idx + count, Ordering::Release);
            self.notify.notify_waiters();
            self.wake_publication_subscribers();
        }

        count
    }

    /// Drain and publish a reusable producer batch in bounded chunks.
    pub fn push_drained_batch_capped(&self, packets: &mut Vec<MediaPacket>) -> usize {
        let mut total = 0usize;
        let mut drained = packets.drain(..);
        loop {
            let published = self.push_batch(drained.by_ref().take(MEDIA_PRODUCER_BATCH_PACKETS));
            if published == 0 {
                break;
            }
            total += published;
        }
        total
    }

    /// [`Self::push_batch`] for a producer whose Raw video carries its
    /// parameter sets in band and that would scan them anyway (SRT ingest):
    /// each packet's complete sets become its track's configuration just
    /// before that packet is stamped, so a new generation starts exactly at
    /// the packet carrying it and earlier packets keep the previous one.
    pub fn push_batch_recording_config<I>(&self, packets: I) -> usize
    where
        I: IntoIterator<Item = MediaPacket>,
    {
        self.push_batch(
            packets
                .into_iter()
                .inspect(|packet| self.record_in_band_config(packet)),
        )
    }

    /// [`Self::push_drained_batch_capped`] recording in-band configurations
    /// as [`Self::push_batch_recording_config`] does.
    pub fn push_drained_batch_recording_config(&self, packets: &mut Vec<MediaPacket>) -> usize {
        let mut total = 0usize;
        let mut drained = packets.drain(..);
        loop {
            let published = self
                .push_batch_recording_config(drained.by_ref().take(MEDIA_PRODUCER_BATCH_PACKETS));
            if published == 0 {
                break;
            }
            total += published;
        }
        total
    }

    pub fn read_at(&self, idx: usize) -> Option<Arc<MediaPacket>> {
        let slot_idx = idx % self.capacity;
        self.slots[slot_idx].data.load_full()
    }

    /// When the packet at `sequence` was published, in microseconds since
    /// the ring was created. The slot may hold a newer packet by the time
    /// it is read: check [`Self::frame_unit`] after reading the packet.
    pub fn published_at_us(&self, sequence: usize) -> u64 {
        self.slots[sequence % self.capacity]
            .published_at_us
            .load(Ordering::Acquire)
    }

    fn add_published_bytes(&self, bytes: u64) {
        // Single producer: no read-modify-write instruction needed.
        let total = self.published_bytes.load(Ordering::Relaxed);
        self.published_bytes
            .store(total.wrapping_add(bytes), Ordering::Relaxed);
    }

    /// Cumulative payload bytes published so far (offered load).
    pub fn published_bytes(&self) -> u64 {
        self.published_bytes.load(Ordering::Relaxed)
    }

    fn elapsed_us(&self) -> u64 {
        self.created_at.elapsed().as_micros().min(u64::MAX as u128) as u64
    }

    pub fn get_write_idx(&self) -> usize {
        self.write_idx.val.load(Ordering::Acquire)
    }

    pub fn get_notify(&self) -> Arc<tokio::sync::Notify> {
        self.notify.clone()
    }

    pub fn min_read_idx(&self) -> usize {
        let write_idx = self.write_idx.val.load(Ordering::Relaxed);
        let readers = crate::sync::lock(&self.readers);
        let mut min_idx = write_idx;
        let mut has_readers = false;
        for w in readers.iter() {
            if let Some(info) = w.upgrade() {
                let r_idx = info.read_idx.load(Ordering::Relaxed);
                min_idx = min_idx.min(r_idx);
                has_readers = true;
            }
        }
        if has_readers { min_idx } else { write_idx }
    }

    pub fn fill_and_capacity(&self) -> (usize, usize) {
        let write_idx = self.write_idx.val.load(Ordering::Relaxed);
        let readers = crate::sync::lock(&self.readers);
        let mut min_idx = write_idx;
        let mut has_readers = false;
        for w in readers.iter() {
            if let Some(info) = w.upgrade() {
                let r_idx = info.read_idx.load(Ordering::Relaxed);
                min_idx = min_idx.min(r_idx);
                has_readers = true;
            }
        }
        let fill = if has_readers {
            write_idx.saturating_sub(min_idx).min(self.capacity)
        } else {
            write_idx.min(self.capacity)
        };
        (fill, self.capacity)
    }

    pub fn payload_stats(&self) -> PayloadStats {
        let mut stats = PayloadStats::default();
        let mut min_payload = usize::MAX;

        for slot in &self.slots {
            let Some(packet) = slot.data.load_full() else {
                continue;
            };
            let len = packet.payload.len();
            stats.slots += 1;
            stats.payload_bytes = stats.payload_bytes.saturating_add(len);
            min_payload = min_payload.min(len);
            stats.max_payload_bytes = stats.max_payload_bytes.max(len);
            match packet.media_type {
                MediaType::Video => {
                    stats.video_bytes = stats.video_bytes.saturating_add(len);
                }
                MediaType::Audio => {
                    stats.audio_bytes = stats.audio_bytes.saturating_add(len);
                }
            }
        }

        if stats.slots > 0 {
            stats.min_payload_bytes = min_payload;
        }

        stats
    }

    /// Packets retained in the ring as (DTS, payload bytes), one entry per
    /// DTS, in DTS order.
    fn payload_by_dts(&self) -> Vec<(i64, u64)> {
        let mut samples = Vec::with_capacity(self.capacity);
        for slot in &self.slots {
            let Some(packet) = slot.data.load_full() else {
                continue;
            };
            samples.push((packet.dts, packet.payload.len() as u64));
        }
        samples.sort_unstable_by_key(|(dts, _)| *dts);

        let mut grouped: Vec<(i64, u64)> = Vec::with_capacity(samples.len());
        for (dts, bytes) in samples {
            if let Some((last_dts, last_bytes)) = grouped.last_mut()
                && *last_dts == dts
            {
                *last_bytes = last_bytes.saturating_add(bytes);
                continue;
            }
            grouped.push((dts, bytes));
        }
        grouped
    }

    /// Average payload bitrate over the retained window's media time (DTS),
    /// at least 250 ms of it: what the feed runs at now, however long the
    /// ring has existed.
    pub fn retained_payload_bitrate_bps(&self) -> Option<u64> {
        average_payload_bps(&self.payload_by_dts())
    }

    /// Estimate the aggregate encoded payload bitrate from packets currently
    /// retained in the ring.
    ///
    /// This is sampled only during stage startup. It deliberately uses media
    /// DTS rather than wall-clock arrival time: a publisher can deliver in
    /// bursts, while FFmpeg's probe must cover a bounded amount of media time.
    /// Use both the retained-window average and a short media-time peak. Early
    /// live ingest can be VBR-heavy around the first keyframe, and FFmpeg needs
    /// enough bytes to see the audio/video headers in that burst even when the
    /// whole retained window averages lower. The startup policy applies the
    /// final cap, so this estimator should err toward stable startup.
    pub fn observed_payload_bitrate_bps(&self) -> Option<u64> {
        const PEAK_WINDOW_MS: i64 = 250;

        let grouped = self.payload_by_dts();
        let average_bps = average_payload_bps(&grouped)?;
        let mut peak_bps = 0u64;
        let mut window_bytes = 0u64;
        let mut end = 0usize;
        for start in 0..grouped.len() {
            let window_end_dts = grouped[start].0.saturating_add(PEAK_WINDOW_MS);
            while end < grouped.len() && grouped[end].0 < window_end_dts {
                window_bytes = window_bytes.saturating_add(grouped[end].1);
                end += 1;
            }

            if window_bytes > 0 {
                let effective_span_ms = grouped[end.saturating_sub(1)]
                    .0
                    .saturating_sub(grouped[start].0)
                    .max(PEAK_WINDOW_MS);
                let candidate_bps = window_bytes
                    .saturating_mul(8)
                    .saturating_mul(1_000)
                    .div_ceil(effective_span_ms as u64);
                peak_bps = peak_bps.max(candidate_bps);
            }

            window_bytes = window_bytes.saturating_sub(grouped[start].1);
        }

        Some(average_bps.max(peak_bps))
    }

    pub fn reader_snapshots(&self) -> Vec<ReaderSnapshot> {
        let write_idx = self.get_write_idx();
        let now_us = self.elapsed_us();
        let mut snapshots = Vec::new();

        let mut readers = crate::sync::lock(&self.readers);
        readers.retain(|weak_ref| {
            let Some(info) = weak_ref.upgrade() else {
                return false;
            };

            let read_idx = info.read_idx.load(Ordering::Acquire);
            let lag_slots = write_idx.saturating_sub(read_idx);
            let packet_age_ms = if lag_slots == 0 || lag_slots >= self.capacity {
                None
            } else {
                let slot = &self.slots[read_idx % self.capacity];
                if slot.data.load_full().is_some() {
                    let published_at_us = slot.published_at_us.load(Ordering::Acquire);
                    (published_at_us > 0).then(|| now_us.saturating_sub(published_at_us) / 1000)
                } else {
                    None
                }
            };
            let (avg_burst_size, median_burst_size, burst_count) = info.burst_stats();

            snapshots.push(ReaderSnapshot {
                name: info.name.clone(),
                read_idx,
                write_idx,
                lag_slots,
                overflow_count: info.overflow_count.load(Ordering::Relaxed),
                packet_age_ms,
                burst_count,
                avg_burst_size,
                median_burst_size,
            });

            true
        });

        snapshots
    }

    pub fn fast_forward(&self, current_write_idx: usize) -> usize {
        let kf_idx = self.last_keyframe_idx.val.load(Ordering::Acquire);
        // usize::MAX is the sentinel for "no keyframe seen yet".
        let kf_known = kf_idx != usize::MAX;
        if kf_known && current_write_idx.saturating_sub(kf_idx) < self.capacity {
            return kf_idx;
        }
        // No valid keyframe is known yet (stream start) or the last keyframe
        // index is more than `capacity` slots behind the write cursor (overflow
        // without a keyframe in the window).
        // Return the current write position to start at the live edge rather
        // than using saturating_sub(100) which returns 0 when write_idx < 100.
        current_write_idx
    }
}

/// Per-stream DTS monotonicity enforcer for MPEG-TS muxing.
///
/// FFmpeg's `write_interleaved` requires strictly increasing DTS per stream.
/// Audio packets at millisecond granularity can share timestamps (e.g. two AAC
/// frames in the same millisecond). This enforcer bumps colliding DTS by 1 and
/// adjusts PTS to maintain PTS >= DTS.
#[derive(Debug, Clone)]
pub struct DtsEnforcer {
    last_dts: Vec<i64>,
}

impl DtsEnforcer {
    pub fn new(num_streams: usize) -> Self {
        Self {
            last_dts: vec![i64::MIN; num_streams],
        }
    }

    /// Enforce monotonically increasing DTS for a given stream.
    /// Returns the corrected (pts, dts) pair.
    pub fn enforce(&mut self, stream_idx: usize, pts: i64, dts: i64) -> (i64, i64) {
        let mut dts = dts;
        if let Some(prev) = self.last_dts.get(stream_idx)
            && dts <= *prev
        {
            dts = prev.saturating_add(1);
        }
        let pts = if pts < dts { dts } else { pts };
        if let Some(slot) = self.last_dts.get_mut(stream_idx) {
            *slot = dts;
        }
        (pts, dts)
    }
}

/// Payload bitrate over `grouped`'s DTS span (milliseconds), at least 250 ms.
fn average_payload_bps(grouped: &[(i64, u64)]) -> Option<u64> {
    const MIN_OBSERVATION_MS: i64 = 250;

    if grouped.len() < 2 {
        return None;
    }
    let span_ms = grouped.last()?.0.checked_sub(grouped.first()?.0)?;
    let payload_bytes = grouped
        .iter()
        .fold(0u64, |acc, (_, bytes)| acc.saturating_add(*bytes));
    if payload_bytes == 0 || span_ms < MIN_OBSERVATION_MS {
        return None;
    }
    Some(
        payload_bytes
            .saturating_mul(8)
            .saturating_mul(1_000)
            .div_ceil(span_ms as u64),
    )
}

#[cfg(test)]
#[path = "ring_buffer_tests.rs"]
mod tests;
