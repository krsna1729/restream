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
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Instant;
use tracing::debug;

/// Compatibility path for the historical ring-owned packet vocabulary.
///
/// Remove only with an explicit downstream API migration; in-tree consumers
/// import these types from `media::packet`.
pub use super::packet::{MediaPacket, MediaType, PayloadFormat};

mod reader;
pub use reader::{Reader, ReaderInfo, ReaderSnapshot};

pub const DEFAULT_RING_CAPACITY: usize = 1024;

/// Max media packets a runtime reader processes per hot-loop burst.
pub const MEDIA_PULL_BURST_PACKETS: usize = 32;

/// Soft cap for producer-side publications into a packet ring.
pub const MEDIA_PRODUCER_BATCH_PACKETS: usize = MEDIA_PULL_BURST_PACKETS;

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
    pub fn set_audio_tracks(&self, tracks: Vec<crate::media::metadata::AudioMeta>) {
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
    pub fn audio_tracks(&self) -> Option<Vec<crate::media::metadata::AudioMeta>> {
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
    /// Use both the retained-window average and a short media-time peak. Early
    /// live ingest can be VBR-heavy around the first keyframe, and FFmpeg needs
    /// enough bytes to see the audio/video headers in that burst even when the
    /// whole retained window averages lower. The startup policy applies the
    /// final cap, so this estimator should err toward stable startup.
    pub fn observed_payload_bitrate_bps(&self) -> Option<u64> {
        const MIN_OBSERVATION_MS: i64 = 250;
        const PEAK_WINDOW_MS: i64 = 250;

        let mut samples = Vec::with_capacity(self.capacity);

        for slot in &self.slots {
            let Some(packet) = slot.data.load_full() else {
                continue;
            };
            samples.push((packet.dts, packet.payload.len() as u64));
        }

        if samples.len() < 2 {
            return None;
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

        let min_dts = grouped.first()?.0;
        let max_dts = grouped.last()?.0;
        let payload_bytes = grouped
            .iter()
            .fold(0u64, |acc, (_, bytes)| acc.saturating_add(*bytes));
        let span_ms = max_dts.checked_sub(min_dts)?;
        if payload_bytes == 0 || span_ms < MIN_OBSERVATION_MS {
            return None;
        }

        let average_bps = payload_bytes
            .saturating_mul(8)
            .saturating_mul(1_000)
            .div_ceil(span_ms as u64);

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

#[cfg(test)]
#[path = "ring_buffer_tests.rs"]
mod tests;
