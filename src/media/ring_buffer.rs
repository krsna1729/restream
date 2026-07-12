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
//! Single-writer is guaranteed by the monotonic `write_idx` — only the ingest
//! thread ever calls `push()`. Multiple readers call `load_full()` concurrently
//! without any locking. This eliminates the per-slot RwLock contention that
//! would otherwise be the bottleneck at 500+ concurrent egress readers.

use arc_swap::ArcSwapOption;
use bytes::Bytes;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use tracing::{debug, info, warn};

use super::MEDIA_PRODUCER_BATCH_PACKETS;

pub const DEFAULT_RING_CAPACITY: usize = 1024;

pub fn default_ring_capacity() -> usize {
    DEFAULT_RING_CAPACITY
}

// Transcoder output rings hold demuxed frames from the FFmpeg child process.
// At 720p30 with one audio track, the packet rate is ~80 pkt/s; 512 slots
// ≈ 6.4 s of jitter headroom (above the 5 s requirement). I-frames from the
// CRF23 encoder are large (~30–50 KB each), so the per-slot payload size is
// much larger than the source ring — 512 slots already dominate memory at high
// bitrates. Scale-test evidence: no transcoder ring overflows across 15 cases.
pub const DEFAULT_TRANSCODER_RING_CAPACITY: usize = 512;

pub fn default_transcoder_ring_capacity() -> usize {
    DEFAULT_TRANSCODER_RING_CAPACITY
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum MediaType {
    Video = 0,
    Audio = 1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum PayloadFormat {
    Flv = 0,
    Raw = 1,
}

/// 56-byte media packet.  `#[repr(C)]` pins the field order so the declared
/// layout is always respected, preventing the compiler from reordering fields
/// into a layout that scatters hot fields across two cache lines.
///
/// Without `#[repr(C)]`, rustc's default greedy-alignment algorithm places the
/// largest field (`payload: Bytes`, 32 bytes) first within the struct.  That
/// puts `media_type`, `is_keyframe`, and `pts`/`dts` at offsets 52–63 inside
/// `ArcInner`, spanning two 64-byte cache lines — reading `media_type` to
/// dispatch the packet requires the *second* cache line.
///
/// With the declared field order the `ArcInner<MediaPacket>` layout is:
/// ```text
/// Byte  0– 7  strong refcount          (ArcInner header)
/// Byte  8–15  weak refcount            (ArcInner header)
/// Byte 16     media_type               ← cache line 0 (bytes 0–63)
/// Byte 17     format
/// Byte 18     is_keyframe
/// Byte 19     (1 byte padding)
/// Byte 20–23  track_index
/// Byte 24–31  pts
/// Byte 32–39  dts
/// Byte 40–47  payload.ptr              ← pointer to codec output, needed immediately
/// Byte 48–55  payload.len
/// Byte 56–63  payload.data             ← cache line 1 (bytes 64+): Arc management only
/// Byte 64–71  payload.vtable
/// ```
///
/// All hot consumer fields — type dispatch, track routing, timestamps, and the
/// payload pointer+length — fit in the first cache line.  Only the `Bytes` Arc
/// management fields (`data`, `vtable`) land in the second cache line, and those
/// are only touched on clone/drop, not on every field read.
///
/// Access groups ordered by first-touch in the hot path:
///   1. Type dispatch  : `media_type`, `format`, `is_keyframe` (offset  0– 2)
///   2. Track routing  : `track_index`                          (offset  4– 7)
///   3. Timestamps     : `pts`, `dts`                          (offset  8–23)
///   4. Payload        : `payload`                             (offset 24–55)
#[derive(Clone, Debug)]
#[repr(C)]
pub struct MediaPacket {
    // Group 1: type dispatch — read first in every consumer (3 bytes + 1 pad = 4)
    pub media_type: MediaType,
    pub format: PayloadFormat,
    pub is_keyframe: bool,
    // 1 byte implicit C padding before the u32
    // Group 2: track routing
    pub track_index: u32,
    // Group 3: timestamps — DTS enforcer reads both together
    pub pts: i64,
    pub dts: i64,
    // Group 4: payload — largest field, accessed after codec dispatch
    pub payload: Bytes,
}

pub struct RingSlot {
    data: ArcSwapOption<MediaPacket>,
    published_at_us: AtomicU64,
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

pub struct ReaderInfo {
    pub name: String,
    pub read_idx: AtomicUsize,
    pub overflow_count: AtomicUsize,
    /// Total `pull_burst` calls that returned ≥ 1 packet.
    pub burst_count: AtomicU64,
    /// Total packets returned across all bursts (avg = packet_sum / burst_count).
    pub packet_sum: AtomicU64,
    /// Histogram of burst sizes across 6 buckets (see `burst_bucket`).
    pub burst_hist: [AtomicU64; BURST_HIST_BUCKETS],
}

#[derive(Debug, Clone)]
pub struct ReaderSnapshot {
    pub name: String,
    pub read_idx: usize,
    pub write_idx: usize,
    pub lag_slots: usize,
    pub overflow_count: usize,
    pub packet_age_ms: Option<u64>,
    pub burst_count: u64,
    pub avg_burst_size: f64,
    pub median_burst_size: usize,
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

impl ReaderInfo {
    fn new(name: String, read_idx: usize) -> Self {
        Self {
            name,
            read_idx: AtomicUsize::new(read_idx),
            overflow_count: AtomicUsize::new(0),
            burst_count: AtomicU64::new(0),
            packet_sum: AtomicU64::new(0),
            burst_hist: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    /// Snapshot of burst size statistics: (avg, approx_median, burst_count).
    pub fn burst_stats(&self) -> (f64, usize, u64) {
        let bursts = self.burst_count.load(Ordering::Relaxed);
        let pkts = self.packet_sum.load(Ordering::Relaxed);
        let avg = if bursts > 0 {
            pkts as f64 / bursts as f64
        } else {
            0.0
        };

        // Approximate median: walk histogram buckets until cumulative count ≥ 50%
        let hist: [u64; BURST_HIST_BUCKETS] =
            std::array::from_fn(|i| self.burst_hist[i].load(Ordering::Relaxed));
        let median = {
            let half = bursts.div_ceil(2);
            let mut cum = 0u64;
            let mut median_bucket = 0usize;
            for (i, &count) in hist.iter().enumerate() {
                cum += count;
                if cum >= half {
                    median_bucket = i;
                    break;
                }
            }
            // Return representative value for the bucket midpoint
            match median_bucket {
                0 => 1,
                1 => 2,
                2 => 3,
                3 => 6,
                4 => 12,
                _ => 24,
            }
        };
        (avg, median, bursts)
    }
}

pub struct RingBuffer {
    slots: Vec<RingSlot>,
    write_idx: AlignedAtomicUsize,
    last_keyframe_idx: AlignedAtomicUsize,
    capacity: usize,
    created_at: Instant,
    notify: Arc<tokio::sync::Notify>,
    pub readers: std::sync::Mutex<Vec<std::sync::Weak<ReaderInfo>>>,
    /// Video codec of packets in this ring, set once by the producer.
    /// `"h264"`, `"hevc"`, or empty string (= infer from ingest metadata).
    /// All packets in a ring share one codec — this avoids per-packet tagging.
    pub codec_hint: std::sync::OnceLock<String>,
    /// Raw Annex B parameter sets captured at ingest so late-joining live
    /// stages can seed H.264/H.265 decoders before the next keyframe arrives.
    /// Stored behind ArcSwapOption so a later complete set can replace an
    /// earlier partial cache on live ingest reconnects or delayed header arrival.
    pub video_parameter_sets: ArcSwapOption<Vec<u8>>,
    /// Audio tracks metadata of packets in this ring.
    /// Stored behind ArcSwapOption so it can be updated when a publisher reconnects
    /// with a different track configuration (OnceLock would silently ignore updates).
    pub audio_tracks: ArcSwapOption<Vec<crate::media::engine::AudioMeta>>,
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
}

impl RingBuffer {
    pub fn new(capacity: usize) -> Self {
        let mut slots = Vec::with_capacity(capacity);
        for _ in 0..capacity {
            slots.push(RingSlot {
                data: ArcSwapOption::empty(),
                published_at_us: AtomicU64::new(0),
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
            capacity,
            created_at: Instant::now(),
            notify: Arc::new(tokio::sync::Notify::new()),
            readers: std::sync::Mutex::new(Vec::new()),
            codec_hint: std::sync::OnceLock::new(),
            video_parameter_sets: ArcSwapOption::empty(),
            audio_tracks: ArcSwapOption::empty(),
            estimated_pkt_rate: std::sync::atomic::AtomicU32::new(0),
            end_of_stream: AtomicBool::new(false),
            next: ArcSwapOption::empty(),
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
    /// the grown ring without advancing the write cursor.
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
            self.slots[slot_idx]
                .published_at_us
                .store(self.elapsed_us().max(1), Ordering::Release);
            self.slots[slot_idx].data.store(Some(packet.clone()));
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
        self.next.store(Some(new_ring));
        // Wake all readers blocked on this ring so they can discover `next`.
        self.notify.notify_waiters();
    }

    pub fn mark_end_of_stream(&self) {
        self.end_of_stream.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }

    pub fn is_end_of_stream(&self) -> bool {
        self.end_of_stream.load(Ordering::Acquire)
    }

    pub fn capacity(&self) -> usize {
        self.capacity
    }

    /// Number of readers whose `Arc<ReaderInfo>` is still alive.
    pub fn active_reader_count(&self) -> usize {
        self.readers
            .lock()
            .map(|mut g| {
                g.retain(|w| w.upgrade().is_some());
                g.len()
            })
            .unwrap_or(0)
    }

    /// Store the probed packet rate so telemetry can show buffer depth in seconds.
    pub fn set_estimated_pkt_rate(&self, pkt_per_sec: f64) {
        self.estimated_pkt_rate
            .store(pkt_per_sec.round() as u32, Ordering::Relaxed);
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

    /// Set the video codec hint for this ring.  Called once by the producer
    /// (e.g. external transcoder, hevc_to_h264 stage).  No-op if already set.
    pub fn set_codec_hint(&self, codec: &str) {
        if self.codec_hint.set(codec.to_string()).is_ok() {
            debug!(codec, "ring codec hint set");
        }
    }

    /// Return the codec hint if set, or empty string.
    pub fn codec_hint_str(&self) -> &str {
        self.codec_hint.get().map(|s| s.as_str()).unwrap_or("")
    }

    pub fn set_video_parameter_sets(&self, parameter_sets: Vec<u8>) {
        let bytes = parameter_sets.len();
        if bytes == 0 {
            return;
        }
        self.video_parameter_sets
            .store(Some(std::sync::Arc::new(parameter_sets)));
        debug!(bytes, "ring video parameter sets cached");
    }

    /// Returns a snapshot clone of the current parameter-set cache.
    pub fn video_parameter_sets(&self) -> Option<Vec<u8>> {
        self.video_parameter_sets
            .load_full()
            .map(|arc| (*arc).clone())
    }

    /// Set (or update) the audio track metadata for this ring.
    /// An empty `tracks` vec clears the metadata, signalling "not yet known" to
    /// downstream stages — this is used by RTMP ingest on publisher reconnect.
    pub fn set_audio_tracks(&self, tracks: Vec<crate::media::engine::AudioMeta>) {
        let count = tracks.len();
        if tracks.is_empty() {
            self.audio_tracks.store(None);
            debug!("ring audio tracks cleared");
        } else {
            self.audio_tracks.store(Some(std::sync::Arc::new(tracks)));
            debug!(count, "ring audio tracks set");
        }
    }

    /// Returns a snapshot clone of the audio track metadata, or `None` if not yet known.
    /// Clones the inner Vec; `audio_tracks` is only read at stage startup, not on the
    /// hot packet path, so this clone is acceptable.
    pub fn audio_tracks(&self) -> Option<Vec<crate::media::engine::AudioMeta>> {
        self.audio_tracks.load_full().map(|arc| (*arc).clone())
    }

    pub fn push(&self, packet: MediaPacket) {
        let idx = self.write_idx.val.load(Ordering::Relaxed);
        let slot_idx = idx % self.capacity;
        let is_keyframe = packet.media_type == MediaType::Video && packet.is_keyframe;

        self.slots[slot_idx]
            .published_at_us
            .store(self.elapsed_us().max(1), Ordering::Release);
        self.slots[slot_idx].data.store(Some(Arc::new(packet)));

        if is_keyframe {
            self.last_keyframe_idx.val.store(idx, Ordering::Release);
        }

        self.write_idx.val.store(idx + 1, Ordering::Release);
        self.notify.notify_waiters();
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

        for packet in packets {
            let idx = start_idx + count;
            let slot_idx = idx % self.capacity;
            let is_keyframe = packet.media_type == MediaType::Video && packet.is_keyframe;

            self.slots[slot_idx]
                .published_at_us
                .store(self.elapsed_us().max(1), Ordering::Release);
            self.slots[slot_idx].data.store(Some(Arc::new(packet)));
            if is_keyframe {
                self.last_keyframe_idx.val.store(idx, Ordering::Release);
            }
            count += 1;
        }

        if count > 0 {
            self.write_idx
                .val
                .store(start_idx + count, Ordering::Release);
            self.notify.notify_waiters();
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

    pub fn read_at(&self, idx: usize) -> Option<Arc<MediaPacket>> {
        let slot_idx = idx % self.capacity;
        self.slots[slot_idx].data.load_full()
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
        if let Ok(readers) = self.readers.lock() {
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
        } else {
            write_idx
        }
    }

    pub fn fill_and_capacity(&self) -> (usize, usize) {
        let write_idx = self.write_idx.val.load(Ordering::Relaxed);
        if let Ok(readers) = self.readers.lock() {
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
        } else {
            (write_idx.min(self.capacity), self.capacity)
        }
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

    /// Estimate the aggregate encoded payload bitrate from packets currently
    /// retained in the ring.
    ///
    /// This is sampled only during stage startup. It deliberately uses media
    /// DTS rather than wall-clock arrival time: a publisher can deliver in
    /// bursts, while FFmpeg's probe must cover a bounded amount of media time.
    /// Require a meaningful window so a single large keyframe cannot be
    /// mistaken for the steady stream rate.
    pub fn observed_payload_bitrate_bps(&self) -> Option<u64> {
        const MIN_OBSERVATION_MS: i64 = 250;

        let mut payload_bytes = 0u64;
        let mut min_dts = i64::MAX;
        let mut max_dts = i64::MIN;
        let mut packets = 0usize;

        for slot in &self.slots {
            let Some(packet) = slot.data.load_full() else {
                continue;
            };
            payload_bytes = payload_bytes.saturating_add(packet.payload.len() as u64);
            min_dts = min_dts.min(packet.dts);
            max_dts = max_dts.max(packet.dts);
            packets += 1;
        }

        let span_ms = max_dts.checked_sub(min_dts)?;
        if packets < 2 || payload_bytes == 0 || span_ms < MIN_OBSERVATION_MS {
            return None;
        }

        Some(
            payload_bytes
                .saturating_mul(8)
                .saturating_mul(1_000)
                .div_ceil(span_ms as u64),
        )
    }

    pub fn reader_snapshots(&self) -> Vec<ReaderSnapshot> {
        let write_idx = self.get_write_idx();
        let now_us = self.elapsed_us();
        let mut snapshots = Vec::new();

        let mut readers = self.readers.lock().unwrap_or_else(|e| e.into_inner());
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

pub struct Reader {
    buffer: Arc<RingBuffer>,
    pub info: Arc<ReaderInfo>,
    read_idx: usize,
    migration_preroll_packets: usize,
}

impl Drop for Reader {
    fn drop(&mut self) {
        // Remove our entry and any other stale Weak refs from the ring's reader
        // list.  Called while self.info still has strong_count = 1 (our field),
        // so we use Arc::ptr_eq to identify our slot; entries where upgrade()
        // returns None are also pruned.
        //
        // unwrap_or_else instead of if-let-Ok: a poisoned mutex (from a panic
        // while holding the lock) must not silently skip cleanup — leaving our
        // Weak in the list would artificially inflate min_read_idx and stall
        // producer overflow recovery.
        let mut readers = self
            .buffer
            .readers
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        readers.retain(|w| match w.upgrade() {
            Some(info) => !Arc::ptr_eq(&info, &self.info),
            None => false,
        });
        info!(reader = %self.info.name, overflows = self.info.overflow_count.load(Ordering::Relaxed), "ring reader deregistered");
    }
}

impl Reader {
    fn register(
        name: String,
        buffer: Arc<RingBuffer>,
        start_idx: usize,
        migration_preroll_packets: usize,
    ) -> Self {
        let info = Arc::new(ReaderInfo::new(name.clone(), start_idx));

        {
            let mut r = buffer.readers.lock().unwrap_or_else(|e| e.into_inner());
            r.push(Arc::downgrade(&info));
        }

        Self {
            buffer,
            info,
            read_idx: start_idx,
            migration_preroll_packets,
        }
    }

    /// Current ring this reader is consuming from.  Changes after a successful
    /// migration triggered by `wait_for_data` following `seal_and_forward`.
    pub fn current_ring(&self) -> &Arc<RingBuffer> {
        &self.buffer
    }

    pub fn is_caught_up_to_end_of_stream(&self) -> bool {
        self.buffer.is_end_of_stream() && self.buffer.get_write_idx() == self.read_idx
    }

    pub fn new(name: String, buffer: Arc<RingBuffer>) -> Self {
        let current_write = buffer.get_write_idx();
        let start_idx = buffer.fast_forward(current_write);
        let reader = Self::register(name, buffer, start_idx, 0);
        info!(reader = %reader.info.name, start_idx, "ring reader registered");
        reader
    }

    pub fn new_with_keyframe_preroll(
        name: String,
        buffer: Arc<RingBuffer>,
        preroll_packets: usize,
    ) -> Self {
        let current_write = buffer.get_write_idx();
        let keyframe_start = buffer.fast_forward(current_write);
        let oldest_available = current_write.saturating_sub(buffer.capacity.saturating_sub(1));
        let start_idx = if keyframe_start < current_write {
            keyframe_start
                .saturating_sub(preroll_packets)
                .max(oldest_available)
        } else {
            keyframe_start
        };
        let reader = Self::register(name, buffer, start_idx, 0);
        info!(
            reader = %reader.info.name,
            start_idx,
            preroll_packets,
            "ring reader registered (keyframe preroll)"
        );
        reader
    }

    pub(crate) fn new_stage_input(
        name: String,
        buffer: Arc<RingBuffer>,
        preroll_packets: usize,
    ) -> Self {
        let current_write = buffer.get_write_idx();
        let keyframe_start = buffer.fast_forward(current_write);
        let oldest_available = current_write.saturating_sub(buffer.capacity.saturating_sub(1));
        let start_idx = if keyframe_start < current_write {
            keyframe_start
                .saturating_sub(preroll_packets)
                .max(oldest_available)
        } else {
            keyframe_start
        };
        let reader = Self::register(name, buffer, start_idx, preroll_packets);
        info!(
            reader = %reader.info.name,
            start_idx,
            preroll_packets,
            "ring reader registered (stage input)"
        );
        reader
    }

    pub fn new_live(name: String, buffer: Arc<RingBuffer>) -> Self {
        let current_write = buffer.get_write_idx();
        let reader = Self::register(name, buffer, current_write, 0);
        info!(reader = %reader.info.name, start_idx = current_write, "ring reader registered (live edge)");
        reader
    }

    pub fn pull(&mut self) -> Result<Option<Arc<MediaPacket>>, &'static str> {
        let write_idx = self.buffer.get_write_idx();

        if write_idx > self.read_idx && write_idx - self.read_idx >= self.buffer.capacity {
            let new_idx = self.buffer.fast_forward(write_idx);
            let lag = write_idx.saturating_sub(self.read_idx);
            self.read_idx = new_idx;
            self.info.read_idx.store(new_idx, Ordering::Relaxed);
            self.info.overflow_count.fetch_add(1, Ordering::Relaxed);
            warn!(reader = %self.info.name, lag_packets = lag, "ring reader overflowed — fast-forwarding to keyframe");
            return Err("Overflow: reader lagged and was fast-forwarded");
        }

        if self.read_idx == write_idx {
            return Ok(None);
        }

        let packet = self.buffer.read_at(self.read_idx);
        let post_write_idx = self.buffer.get_write_idx();
        if post_write_idx > self.read_idx && post_write_idx - self.read_idx >= self.buffer.capacity
        {
            let new_idx = self.buffer.fast_forward(post_write_idx);
            let lag = post_write_idx.saturating_sub(self.read_idx);
            self.read_idx = new_idx;
            self.info.read_idx.store(new_idx, Ordering::Relaxed);
            self.info.overflow_count.fetch_add(1, Ordering::Relaxed);
            warn!(reader = %self.info.name, lag_packets = lag, "ring reader overflowed mid-read — fast-forwarding to keyframe");
            return Err("Overflow: reader lagged and was fast-forwarded");
        }

        if packet.is_some() {
            self.read_idx += 1;
            self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
        }
        Ok(packet)
    }

    /// Load up to `max_packets` using one write-index acquisition.
    ///
    /// Appends packets to `output` and returns the number appended. Overflow
    /// behavior matches `pull()`.
    pub fn pull_burst(
        &mut self,
        output: &mut Vec<Arc<MediaPacket>>,
        max_packets: usize,
    ) -> Result<usize, &'static str> {
        if max_packets == 0 {
            return Ok(0);
        }

        let write_idx = self.buffer.get_write_idx();
        if write_idx > self.read_idx && write_idx - self.read_idx >= self.buffer.capacity {
            self.read_idx = self.buffer.fast_forward(write_idx);
            self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
            self.info.overflow_count.fetch_add(1, Ordering::Relaxed);
            return Err("Overflow: reader lagged and was fast-forwarded");
        }

        let available = write_idx.saturating_sub(self.read_idx).min(max_packets);
        output.reserve(available);
        let start_len = output.len();

        for idx in self.read_idx..self.read_idx + available {
            let Some(packet) = self.buffer.read_at(idx) else {
                break;
            };
            output.push(packet);
        }

        let post_write_idx = self.buffer.get_write_idx();
        if post_write_idx > self.read_idx && post_write_idx - self.read_idx >= self.buffer.capacity
        {
            output.truncate(start_len);
            self.read_idx = self.buffer.fast_forward(post_write_idx);
            self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
            self.info.overflow_count.fetch_add(1, Ordering::Relaxed);
            return Err("Overflow: reader lagged and was fast-forwarded");
        }

        let loaded = output.len() - start_len;
        self.read_idx += loaded;
        self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
        if loaded > 0 {
            self.info.burst_count.fetch_add(1, Ordering::Relaxed);
            self.info
                .packet_sum
                .fetch_add(loaded as u64, Ordering::Relaxed);
            self.info.burst_hist[burst_bucket(loaded)].fetch_add(1, Ordering::Relaxed);
        }
        Ok(loaded)
    }

    /// Wait until the ring (or its successor) has unread data past `read_idx`.
    ///
    /// If the current ring was sealed while we were waiting (`ring.next` is set),
    /// we drain any remaining unread slots in the current ring, then silently
    /// migrate to the next ring.  External egress connections are never
    /// disrupted — they just observe a brief pause in data flow.
    pub async fn wait_for_data(&mut self) {
        loop {
            let notify = self.buffer.get_notify();
            // Re-check for data before blocking to avoid a TOCTOU race:
            // the writer could notify_waiters() between our pull() returning
            // None and this notified().await registering — Notify does NOT
            // store notifications for future waiters, so we'd sleep forever.
            if self.buffer.get_write_idx() > self.read_idx {
                return;
            }
            // Check if this ring was superseded by a larger one.
            if let Some(next) = self.buffer.next.load_full() {
                // De-register from old ring's reader list and migrate.
                self.migrate_to(next);
                continue; // re-check write_idx on new ring
            }
            // Subscribe BEFORE the final check so `notify_waiters()` fired
            // during seal_and_forward() cannot be missed.
            let notified = notify.notified();
            if self.buffer.get_write_idx() > self.read_idx {
                return;
            }
            if self.buffer.next.load().is_some() {
                // sealed between our last check and notified subscription; retry
                continue;
            }
            if self.buffer.is_end_of_stream() {
                return;
            }
            notified.await;
        }
    }

    /// Migrate this reader to `new_ring`, carrying `read_idx` forward.
    fn migrate_to(&mut self, new_ring: Arc<RingBuffer>) {
        let old_read_idx = self.read_idx;
        // Drain any final unread slots in the old ring before switching.
        // In practice the old ring is sealed only after write_idx stabilises,
        // so lag is 0 or very small.
        // (Nothing to drain here — the loop in wait_for_data already pulled
        //  everything via pull_burst before calling us.)
        let old = std::mem::replace(&mut self.buffer, new_ring.clone());
        // Re-register reader with new ring so active_reader_count() is accurate.
        if let Ok(mut guard) = new_ring.readers.lock() {
            guard.push(Arc::downgrade(&self.info));
        }
        let new_write_idx = new_ring.get_write_idx();
        if self.migration_preroll_packets > 0 && old_read_idx == new_write_idx {
            let keyframe_start = new_ring.fast_forward(new_write_idx);
            if keyframe_start < new_write_idx {
                let oldest_available =
                    new_write_idx.saturating_sub(new_ring.capacity.saturating_sub(1));
                self.read_idx = keyframe_start
                    .saturating_sub(self.migration_preroll_packets)
                    .max(oldest_available);
                self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
            }
        }
        // Remove from old ring's reader list (best-effort; Weak will expire anyway).
        if let Ok(mut guard) = old.readers.lock() {
            guard.retain(|w| w.upgrade().is_some());
        }
        debug!(
            read_idx = self.read_idx,
            name = %self.info.name,
            "reader migrated to resized ring"
        );
    }

    /// Number of slots this reader is behind the write cursor.
    ///
    /// Zero means fully caught up; values approaching `capacity` mean
    /// the reader is at risk of overflow. Useful as a health metric for slow
    /// egress consumers.
    pub fn lag(&self) -> usize {
        self.buffer.get_write_idx().saturating_sub(self.read_idx)
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
            dts = *prev + 1;
        }
        let pts = if pts < dts { dts } else { pts };
        if let Some(slot) = self.last_dts.get_mut(stream_idx) {
            *slot = dts;
        }
        (pts, dts)
    }
}

#[cfg(test)]
#[path = "ring_buffer_tests.rs"]
mod tests;
