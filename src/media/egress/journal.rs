//! Bounded shared feed adaptors implementing [`EgressFeed`].
//!
//! Phase 2 wraps the existing ring structures behind the trait without
//! rewriting them.  Two concrete adaptors are provided:
//!
//! - [`RingFeed`] — wraps [`RingBuffer`] for RTMP-compatible packet fan-out.
//! - [`TsFeed`] — wraps the inner ring of [`TsChunkRing`] for SRT/HLS
//!   MPEG-TS transport messages.
//!
//! ## Sequence numbering
//!
//! Both rings use `usize` write indices internally.  We expose them as `u64`
//! sequences (monotonically increasing within an epoch) so the fabric's cursor
//! arithmetic stays consistent:
//!
//! ```text
//! sequence == ring_write_idx as u64
//! ```
//!
//! ## Epoch changes
//!
//! An epoch is a monotonically increasing generation counter.  The producer
//! bumps it (via [`FeedEpoch::advance`]) whenever the feed has a
//! discontinuity (source replacement, seal-and-forward).  Existing cursors
//! with an old epoch receive [`FeedRead::EpochMismatch`] and must
//! resynchronize.
//!
//! ## Wake coalescing
//!
//! One `AtomicBool` wake-pending flag per `(FeedId, shard)` pair.  Protocol:
//! 1. Publisher sets the flag with `Release` after every push.
//! 2. Shard calls `WakeGate::take` (AcqRel) before sleeping.
//! 3. Shard **re-reads** the feed head after clearing the flag.
//!
//! If the publisher advances between steps 2 and 3, the shard observes the
//! new head before sleeping, so no wakeup is lost.
//!
//! ## Retention
//!
//! `RingBuffer` enforces a slot-count ceiling.  `FeedLimits` expresses the
//! same bound in bytes for observability and policy decisions; the fabric uses
//! it to emit a `FEED_OVERSIZED_UNIT` condition rather than deadlock.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;

use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::packet::MediaPacket;
use crate::media::ring_buffer::RingBuffer;
use crate::media::ts_chunk_ring::TsChunkRing;

fn oldest_retained_sequence(ring: &RingBuffer) -> u64 {
    ring.get_write_idx().saturating_sub(ring.capacity()) as u64
}

// ---------------------------------------------------------------------------
// FeedLimits
// ---------------------------------------------------------------------------

/// Retention policy limits expressed at the feed level.
#[derive(Debug, Clone)]
pub struct FeedLimits {
    /// Maximum total payload bytes retained.
    pub max_retained_bytes: u64,
    /// Maximum number of media units retained (slot count).
    pub max_retained_units: usize,
    pub max_retained_media_age: Duration,
    pub max_unit_bytes: u64,
}

impl Default for FeedLimits {
    fn default() -> Self {
        Self {
            max_retained_bytes: 32 * 1024 * 1024, // 32 MiB
            max_retained_units: 1024,
            max_retained_media_age: Duration::from_secs(30),
            max_unit_bytes: 4 * 1024 * 1024,
        }
    }
}

impl FeedLimits {
    pub fn evaluate(&self, snapshot: FeedRetentionSnapshot) -> FeedLimitStatus {
        let max_age_ms = self
            .max_retained_media_age
            .as_millis()
            .min(u128::from(u64::MAX)) as u64;
        FeedLimitStatus {
            retained_bytes_exceeded: snapshot.retained_bytes > self.max_retained_bytes,
            retained_units_exceeded: snapshot.retained_units > self.max_retained_units,
            media_age_exceeded: snapshot.media_age_ms.is_some_and(|age| age > max_age_ms),
            oversized_unit_count: u64::from(snapshot.largest_unit_bytes > self.max_unit_bytes),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedRetentionSnapshot {
    pub head_sequence: u64,
    pub oldest_sequence: u64,
    pub retained_units: usize,
    pub retained_bytes: u64,
    pub media_age_ms: Option<u64>,
    pub largest_unit_bytes: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FeedLimitStatus {
    pub retained_bytes_exceeded: bool,
    pub retained_units_exceeded: bool,
    pub media_age_exceeded: bool,
    pub oversized_unit_count: u64,
}

impl FeedLimitStatus {
    pub fn is_within_limits(self) -> bool {
        !self.retained_bytes_exceeded
            && !self.retained_units_exceeded
            && !self.media_age_exceeded
            && self.oversized_unit_count == 0
    }
}

fn retention_snapshot(ring: &RingBuffer) -> FeedRetentionSnapshot {
    let head_sequence = ring.get_write_idx() as u64;
    let oldest_sequence = oldest_retained_sequence(ring);
    let mut retained_units = 0usize;
    let mut retained_bytes = 0u64;
    let mut largest_unit_bytes = 0u64;
    let mut min_dts: Option<i64> = None;
    let mut max_dts: Option<i64> = None;

    for sequence in oldest_sequence..head_sequence {
        let Some(packet) = ring.read_at(sequence as usize) else {
            continue;
        };
        let payload_len = packet.payload.len() as u64;
        retained_units += 1;
        retained_bytes = retained_bytes.saturating_add(payload_len);
        largest_unit_bytes = largest_unit_bytes.max(payload_len);
        min_dts = Some(min_dts.map_or(packet.dts, |dts| dts.min(packet.dts)));
        max_dts = Some(max_dts.map_or(packet.dts, |dts| dts.max(packet.dts)));
    }

    let media_age_ms = min_dts
        .zip(max_dts)
        .map(|(min, max)| max.saturating_sub(min) as u64);

    FeedRetentionSnapshot {
        head_sequence,
        oldest_sequence,
        retained_units,
        retained_bytes,
        media_age_ms,
        largest_unit_bytes,
    }
}

// ---------------------------------------------------------------------------
// WakeGate — one outstanding notification per (feed, shard)
// ---------------------------------------------------------------------------

/// Coalescing wake gate: at most one outstanding notification per consumer.
///
/// ### Protocol
/// 1. Publisher calls [`notify`](WakeGate::notify) after every push (`Release`).
/// 2. Shard calls [`take`](WakeGate::take) before sleeping (`AcqRel`) — returns
///    `true` if a notification was pending (consumed atomically).
/// 3. Shard **must** re-read the feed head after `take` to close the ABA
///    window: if the publisher advanced between `take` and the re-read, the
///    shard observes new data instead of sleeping.
#[derive(Debug, Default)]
pub struct WakeGate {
    pending: AtomicBool,
}

impl WakeGate {
    pub fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
        }
    }

    /// Mark that new data is available.  Idempotent — does not count calls.
    #[inline]
    pub fn notify(&self) {
        self.pending.store(true, Ordering::Release);
    }

    /// Clear the pending flag; returns `true` if it was set.
    ///
    /// The caller **must** re-read the feed head after this call.
    #[inline]
    pub fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    /// Non-destructive peek (for metrics only — never use for scheduling).
    #[inline]
    pub fn is_pending(&self) -> bool {
        self.pending.load(Ordering::Relaxed)
    }
}

// ---------------------------------------------------------------------------
// FeedEpoch — shared discontinuity counter
// ---------------------------------------------------------------------------

/// Monotonically increasing epoch, shared between producer and [`EgressFeed`]
/// adaptors.
#[derive(Debug, Default)]
pub struct FeedEpoch {
    epoch: AtomicU64,
}

impl FeedEpoch {
    pub fn new() -> Self {
        Self {
            epoch: AtomicU64::new(0),
        }
    }

    /// Current epoch value.
    #[inline]
    pub fn current(&self) -> u64 {
        self.epoch.load(Ordering::Acquire)
    }

    /// Bump to the next epoch (call on discontinuity / seal).
    /// Returns the new epoch value.
    pub fn advance(&self) -> u64 {
        self.epoch.fetch_add(1, Ordering::AcqRel).wrapping_add(1)
    }
}

// ---------------------------------------------------------------------------
// RingFeed — EgressFeed adaptor over RingBuffer
// ---------------------------------------------------------------------------

/// [`EgressFeed`] adaptor over a shared [`RingBuffer`].
///
/// `Unit` is `Arc<MediaPacket>` — zero-copy fan-out via ref-counting.
/// Sequence numbers equal the ring's `write_idx` cast to `u64`.
pub struct RingFeed {
    ring: Arc<RingBuffer>,
    epoch: Arc<FeedEpoch>,
    /// Cached oldest sequence; updated lazily on overrun detection.
    cached_oldest: AtomicU64,
}

impl RingFeed {
    pub fn new(ring: Arc<RingBuffer>, epoch: Arc<FeedEpoch>) -> Self {
        Self {
            ring,
            epoch,
            cached_oldest: AtomicU64::new(0),
        }
    }

    fn refresh_oldest(&self) -> u64 {
        let oldest = oldest_retained_sequence(&self.ring);
        self.cached_oldest.store(oldest, Ordering::Relaxed);
        oldest
    }

    pub fn retention_snapshot(&self) -> FeedRetentionSnapshot {
        retention_snapshot(&self.ring)
    }

    pub fn limit_status(&self, limits: &FeedLimits) -> FeedLimitStatus {
        limits.evaluate(self.retention_snapshot())
    }
}

impl std::fmt::Debug for RingFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RingFeed")
            .field("head", &self.head_sequence())
            .field("oldest", &self.oldest_sequence())
            .field("epoch", &self.epoch.current())
            .finish()
    }
}

impl EgressFeed for RingFeed {
    type Unit = Arc<MediaPacket>;

    fn head_sequence(&self) -> u64 {
        self.ring.get_write_idx() as u64
    }

    fn oldest_sequence(&self) -> u64 {
        self.refresh_oldest()
    }

    fn epoch(&self) -> u64 {
        self.epoch.current()
    }

    fn read_from(&self, cursor: FeedCursor, budget: ReadBudget) -> FeedRead<Self::Unit> {
        // Epoch check first.
        let current_epoch = self.epoch.current();
        if cursor.epoch != current_epoch {
            return FeedRead::EpochMismatch { current_epoch };
        }

        let head = self.ring.get_write_idx() as u64;
        let oldest = self.refresh_oldest();

        if cursor.next_sequence < oldest {
            return FeedRead::Overrun {
                oldest_sequence: oldest,
            };
        }
        if cursor.next_sequence >= head {
            return FeedRead::Empty;
        }

        let mut units: Vec<Arc<MediaPacket>> = Vec::new();
        let mut seq = cursor.next_sequence;
        let mut total_bytes = 0usize;

        while seq < head && units.len() < budget.max_units && total_bytes < budget.max_bytes {
            match self.ring.read_at(seq as usize) {
                Some(pkt) => {
                    total_bytes += pkt.payload.len();
                    units.push(pkt);
                    seq += 1;
                }
                None => {
                    // Slot was overwritten while we were reading.
                    let new_oldest = self.refresh_oldest();
                    return FeedRead::Overrun {
                        oldest_sequence: new_oldest,
                    };
                }
            }
        }

        if units.is_empty() {
            return FeedRead::Empty;
        }

        FeedRead::Units {
            units,
            next_cursor: FeedCursor::new(cursor.epoch, seq),
        }
    }

    fn latest_sync_point(&self) -> Option<FeedCursor> {
        let epoch = self.epoch.current();
        let head = self.ring.get_write_idx();
        // `fast_forward` returns the most recent keyframe index; we use it
        // as a read-only O(1) lookup (it only makes sense when head > 0).
        if head == 0 {
            return None;
        }
        let kf_idx = self.ring.fast_forward(head);
        if kf_idx >= self.refresh_oldest() as usize && kf_idx < head {
            Some(FeedCursor::new(epoch, kf_idx as u64))
        } else {
            None
        }
    }

    fn sync_point_at_or_after(&self, sequence: u64) -> Option<FeedCursor> {
        let epoch = self.epoch.current();
        let head = self.ring.get_write_idx() as u64;
        let oldest = self.refresh_oldest();
        let start = sequence.max(oldest);

        // Linear scan — only used during resync, not on the hot path.
        for idx in start..head {
            if self.ring.read_at(idx as usize).is_some_and(|p| {
                p.media_type == crate::media::packet::MediaType::Video && p.is_keyframe
            }) {
                return Some(FeedCursor::new(epoch, idx));
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// TsFeed — EgressFeed adaptor over TsChunkRing's inner RingBuffer
// ---------------------------------------------------------------------------

/// [`EgressFeed`] adaptor over the [`RingBuffer`] inside a [`TsChunkRing`].
///
/// `Unit` is `Bytes` — zero-copy via the packet's `payload` field.
/// The outer `TsChunkRing` wrapper owns the cancel token; we only borrow the
/// inner ring for non-blocking reads.
pub struct TsFeed {
    /// Shared access to the inner ring (packets hold pre-muxed TS payloads).
    ring: Arc<RingBuffer>,
    epoch: Arc<FeedEpoch>,
    cached_oldest: AtomicU64,
}

impl TsFeed {
    /// Construct from a `TsChunkRing` by cloning its inner `Arc<RingBuffer>`.
    pub fn new(ts_ring: &TsChunkRing, epoch: Arc<FeedEpoch>) -> Self {
        Self {
            ring: ts_ring.ring.clone(),
            epoch,
            cached_oldest: AtomicU64::new(0),
        }
    }

    fn refresh_oldest(&self) -> u64 {
        let oldest = oldest_retained_sequence(&self.ring);
        self.cached_oldest.store(oldest, Ordering::Relaxed);
        oldest
    }

    pub fn retention_snapshot(&self) -> FeedRetentionSnapshot {
        retention_snapshot(&self.ring)
    }

    pub fn limit_status(&self, limits: &FeedLimits) -> FeedLimitStatus {
        limits.evaluate(self.retention_snapshot())
    }
}

impl std::fmt::Debug for TsFeed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsFeed")
            .field("head", &self.head_sequence())
            .field("epoch", &self.epoch.current())
            .finish()
    }
}

impl EgressFeed for TsFeed {
    type Unit = Bytes;

    fn head_sequence(&self) -> u64 {
        self.ring.get_write_idx() as u64
    }

    fn oldest_sequence(&self) -> u64 {
        self.refresh_oldest()
    }

    fn epoch(&self) -> u64 {
        self.epoch.current()
    }

    fn read_from(&self, cursor: FeedCursor, budget: ReadBudget) -> FeedRead<Self::Unit> {
        let current_epoch = self.epoch.current();
        if cursor.epoch != current_epoch {
            return FeedRead::EpochMismatch { current_epoch };
        }

        let head = self.ring.get_write_idx() as u64;
        let oldest = self.refresh_oldest();

        if cursor.next_sequence < oldest {
            return FeedRead::Overrun {
                oldest_sequence: oldest,
            };
        }
        if cursor.next_sequence >= head {
            return FeedRead::Empty;
        }

        let mut units: Vec<Bytes> = Vec::new();
        let mut seq = cursor.next_sequence;
        let mut total_bytes = 0usize;

        while seq < head && units.len() < budget.max_units && total_bytes < budget.max_bytes {
            match self.ring.read_at(seq as usize) {
                Some(pkt) => {
                    total_bytes += pkt.payload.len();
                    // Clone is O(1) — `Bytes` is reference-counted.
                    units.push(pkt.payload.clone());
                    seq += 1;
                }
                None => {
                    let new_oldest = self.refresh_oldest();
                    return FeedRead::Overrun {
                        oldest_sequence: new_oldest,
                    };
                }
            }
        }

        if units.is_empty() {
            return FeedRead::Empty;
        }

        FeedRead::Units {
            units,
            next_cursor: FeedCursor::new(cursor.epoch, seq),
        }
    }

    fn latest_sync_point(&self) -> Option<FeedCursor> {
        let epoch = self.epoch.current();
        let head = self.ring.get_write_idx();
        if head == 0 {
            return None;
        }
        let kf_idx = self.ring.fast_forward(head);
        if kf_idx >= self.refresh_oldest() as usize && kf_idx < head {
            Some(FeedCursor::new(epoch, kf_idx as u64))
        } else {
            None
        }
    }

    fn sync_point_at_or_after(&self, sequence: u64) -> Option<FeedCursor> {
        let epoch = self.epoch.current();
        let head = self.ring.get_write_idx() as u64;
        let oldest = self.refresh_oldest();
        let start = sequence.max(oldest);
        for idx in start..head {
            if self
                .ring
                .read_at(idx as usize)
                .is_some_and(|p| p.is_keyframe)
            {
                return Some(FeedCursor::new(epoch, idx));
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// FeedOverrunStats — observable condition tracking
// ---------------------------------------------------------------------------

/// Per-feed overrun and discontinuity counters.
#[derive(Debug, Default, Clone)]
pub struct FeedOverrunStats {
    pub overrun_count: u64,
    pub epoch_count: u64,
    pub last_overrun_at: Option<Instant>,
    pub oversized_unit_count: u64,
}

impl FeedOverrunStats {
    pub fn record_overrun(&mut self) {
        self.overrun_count += 1;
        self.last_overrun_at = Some(Instant::now());
    }

    pub fn record_epoch(&mut self) {
        self.epoch_count += 1;
    }

    pub fn record_oversized_unit(&mut self) {
        self.oversized_unit_count += 1;
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::egress::feed::ReadBudget;
    use crate::media::packet::{MediaType, PayloadFormat};
    use tokio_util::sync::CancellationToken;

    // -----------------------------------------------------------------------
    // WakeGate
    // -----------------------------------------------------------------------

    #[test]
    fn wake_gate_set_and_take() {
        let gate = WakeGate::new();
        assert!(!gate.take());
        gate.notify();
        assert!(gate.is_pending());
        assert!(gate.take());
        assert!(!gate.take());
    }

    #[test]
    fn wake_gate_idempotent_notify() {
        let gate = WakeGate::new();
        gate.notify();
        gate.notify();
        gate.notify();
        assert!(gate.take());
        assert!(!gate.take());
    }

    /// ABA-window safety: notify between take and re-read must leave data visible.
    #[test]
    fn wake_gate_aba_window() {
        let gate = WakeGate::new();
        // Shard about to sleep: takes the flag (false → nothing pending yet).
        assert!(!gate.take());
        // Publisher pushes AFTER shard cleared flag.
        gate.notify();
        // Shard re-reads feed head and must see new data — the flag is set.
        assert!(gate.is_pending());
        // Next cycle: shard takes it.
        assert!(gate.take());
    }

    // -----------------------------------------------------------------------
    // FeedEpoch
    // -----------------------------------------------------------------------

    #[test]
    fn feed_epoch_advances_monotonically() {
        let ep = FeedEpoch::new();
        assert_eq!(ep.current(), 0);
        assert_eq!(ep.advance(), 1);
        assert_eq!(ep.current(), 1);
        assert_eq!(ep.advance(), 2);
    }

    // -----------------------------------------------------------------------
    // RingFeed
    // -----------------------------------------------------------------------

    fn push_packet_at(ring: &RingBuffer, payload: &[u8], is_keyframe: bool, dts: i64) {
        ring.push(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: dts,
            dts,
            is_keyframe,
            format: PayloadFormat::Raw,
            payload: Bytes::copy_from_slice(payload),
        });
    }

    fn push_packet(ring: &RingBuffer, payload: &[u8], is_keyframe: bool) {
        push_packet_at(ring, payload, is_keyframe, 0);
    }

    #[test]
    fn ring_feed_empty_when_nothing_pushed() {
        let ring = Arc::new(RingBuffer::new(16));
        let epoch = Arc::new(FeedEpoch::new());
        let feed = RingFeed::new(ring, epoch);
        let cursor = FeedCursor::new(0, 0);
        assert!(matches!(
            feed.read_from(cursor, ReadBudget::default()),
            FeedRead::Empty
        ));
    }

    #[test]
    fn ring_feed_reads_pushed_packets() {
        let ring = Arc::new(RingBuffer::new(16));
        let epoch = Arc::new(FeedEpoch::new());
        push_packet(&ring, b"hello", true);
        push_packet(&ring, b"world", false);

        let feed = RingFeed::new(ring, epoch);
        let cursor = FeedCursor::new(0, 0);
        match feed.read_from(cursor, ReadBudget::default()) {
            FeedRead::Units { units, next_cursor } => {
                assert_eq!(units.len(), 2);
                assert_eq!(next_cursor.next_sequence, 2);
                assert_eq!(&*units[0].payload, b"hello");
            }
            other => panic!("expected Units, got {other:?}"),
        }
    }

    #[test]
    fn ring_feed_epoch_mismatch() {
        let ring = Arc::new(RingBuffer::new(16));
        let epoch = Arc::new(FeedEpoch::new());
        push_packet(&ring, b"data", false);
        epoch.advance(); // now epoch = 1

        let feed = RingFeed::new(ring, epoch);
        let old_cursor = FeedCursor::new(0, 0);
        assert!(matches!(
            feed.read_from(old_cursor, ReadBudget::default()),
            FeedRead::EpochMismatch { current_epoch: 1 }
        ));
    }

    #[test]
    fn ring_feed_head_sequence_matches_write_idx() {
        let ring = Arc::new(RingBuffer::new(16));
        let epoch = Arc::new(FeedEpoch::new());
        let feed = RingFeed::new(ring.clone(), epoch);
        assert_eq!(feed.head_sequence(), 0);
        push_packet(&ring, b"a", false);
        assert_eq!(feed.head_sequence(), 1);
        push_packet(&ring, b"b", false);
        assert_eq!(feed.head_sequence(), 2);
    }

    #[test]
    fn ring_feed_respects_unit_budget() {
        let ring = Arc::new(RingBuffer::new(32));
        let epoch = Arc::new(FeedEpoch::new());
        for i in 0..10 {
            push_packet(&ring, &[i as u8; 8], false);
        }
        let feed = RingFeed::new(ring, epoch);
        let cursor = FeedCursor::new(0, 0);
        match feed.read_from(cursor, ReadBudget::new(3, usize::MAX)) {
            FeedRead::Units { units, .. } => assert!(units.len() <= 3),
            other => panic!("expected Units, got {other:?}"),
        }
    }

    #[test]
    fn ring_feed_retention_snapshot_tracks_retained_bytes_and_media_age() {
        let ring = Arc::new(RingBuffer::new(4));
        let epoch = Arc::new(FeedEpoch::new());
        for i in 0..6 {
            push_packet_at(&ring, &[i as u8; 3], i == 4, i * 10);
        }

        let feed = RingFeed::new(ring, epoch);
        let snapshot = feed.retention_snapshot();

        assert_eq!(snapshot.head_sequence, 6);
        assert_eq!(snapshot.oldest_sequence, 2);
        assert_eq!(snapshot.retained_units, 4);
        assert_eq!(snapshot.retained_bytes, 12);
        assert_eq!(snapshot.media_age_ms, Some(30));
        assert_eq!(snapshot.largest_unit_bytes, 3);
    }

    #[test]
    fn ring_feed_limit_status_reports_strict_retention_violations() {
        let ring = Arc::new(RingBuffer::new(4));
        let epoch = Arc::new(FeedEpoch::new());
        push_packet_at(&ring, &[0; 2], true, 10);
        push_packet_at(&ring, &[1; 8], false, 80);

        let feed = RingFeed::new(ring, epoch);
        let limits = FeedLimits {
            max_retained_bytes: 9,
            max_retained_units: 1,
            max_retained_media_age: Duration::from_millis(50),
            max_unit_bytes: 4,
        };
        let status = feed.limit_status(&limits);

        assert!(status.retained_bytes_exceeded);
        assert!(status.retained_units_exceeded);
        assert!(status.media_age_exceeded);
        assert_eq!(status.oversized_unit_count, 1);
        assert!(!status.is_within_limits());
    }

    #[test]
    fn ring_feed_reads_single_oversized_unit_under_nonzero_byte_budget() {
        let ring = Arc::new(RingBuffer::new(4));
        let epoch = Arc::new(FeedEpoch::new());
        push_packet(&ring, &[7; 8], true);

        let feed = RingFeed::new(ring, epoch);
        match feed.read_from(FeedCursor::new(0, 0), ReadBudget::new(8, 4)) {
            FeedRead::Units { units, next_cursor } => {
                assert_eq!(units.len(), 1);
                assert_eq!(units[0].payload.len(), 8);
                assert_eq!(next_cursor, FeedCursor::new(0, 1));
            }
            other => panic!("expected oversized unit to be admitted, got {other:?}"),
        }
    }

    #[test]
    fn ring_feed_reports_overrun_at_retention_boundary() {
        let ring = Arc::new(RingBuffer::new(4));
        let epoch = Arc::new(FeedEpoch::new());
        for i in 0..6 {
            push_packet(&ring, &[i as u8], i == 4);
        }

        let feed = RingFeed::new(ring, epoch);
        assert_eq!(feed.head_sequence(), 6);
        assert_eq!(feed.oldest_sequence(), 2);
        assert!(matches!(
            feed.read_from(FeedCursor::new(0, 1), ReadBudget::default()),
            FeedRead::Overrun { oldest_sequence: 2 }
        ));
    }

    #[test]
    fn ring_feed_finds_sync_points_without_registered_readers_after_wraparound() {
        let ring = Arc::new(RingBuffer::new(4));
        let epoch = Arc::new(FeedEpoch::new());
        for i in 0..6 {
            push_packet(&ring, &[i as u8], i == 4);
        }

        let feed = RingFeed::new(ring, epoch);
        assert_eq!(feed.latest_sync_point(), Some(FeedCursor::new(0, 4)));
        assert_eq!(feed.sync_point_at_or_after(2), Some(FeedCursor::new(0, 4)));
        assert_eq!(feed.sync_point_at_or_after(5), None);
    }

    #[test]
    fn ring_feed_many_leaf_cursors_share_payload_storage() {
        let ring = Arc::new(RingBuffer::new(16));
        let epoch = Arc::new(FeedEpoch::new());
        push_packet(&ring, b"shared-payload", true);

        let feed = RingFeed::new(ring.clone(), epoch);
        let retained = ring.read_at(0).expect("packet should be retained");
        assert_eq!(Arc::strong_count(&retained), 2);

        let mut batches = Vec::new();
        for _ in 0..1_000 {
            match feed.read_from(FeedCursor::new(0, 0), ReadBudget::new(1, usize::MAX)) {
                FeedRead::Units { units, .. } => batches.push(units),
                other => panic!("expected shared unit batch, got {other:?}"),
            }
        }

        assert_eq!(Arc::strong_count(&retained), 1_002);
        assert!(
            batches
                .iter()
                .all(|units| Arc::ptr_eq(&units[0], &retained))
        );
    }

    #[test]
    fn wake_gate_coalesces_one_publication_per_shard() {
        let shard_gates: Vec<WakeGate> = (0..1_000).map(|_| WakeGate::new()).collect();

        for gate in &shard_gates {
            gate.notify();
            gate.notify();
        }

        assert_eq!(
            shard_gates.iter().filter(|gate| gate.is_pending()).count(),
            1_000
        );
        assert_eq!(shard_gates.iter().filter(|gate| gate.take()).count(), 1_000);
        assert_eq!(shard_gates.iter().filter(|gate| gate.take()).count(), 0);
    }

    // -----------------------------------------------------------------------
    // TsFeed
    // -----------------------------------------------------------------------

    #[test]
    fn ts_feed_reads_pushed_chunks() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(16, cancel);
        ts_ring.push(Bytes::from_static(b"chunk1"), true);
        ts_ring.push(Bytes::from_static(b"chunk2"), false);

        let epoch = Arc::new(FeedEpoch::new());
        let feed = TsFeed::new(&ts_ring, epoch);
        let cursor = FeedCursor::new(0, 0);
        match feed.read_from(cursor, ReadBudget::default()) {
            FeedRead::Units { units, next_cursor } => {
                assert_eq!(units.len(), 2);
                assert_eq!(next_cursor.next_sequence, 2);
                assert_eq!(units[0].as_ref(), b"chunk1" as &[u8]);
            }
            other => panic!("expected Units, got {other:?}"),
        }
    }

    #[test]
    fn ts_feed_empty_initially() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(16, cancel);
        let epoch = Arc::new(FeedEpoch::new());
        let feed = TsFeed::new(&ts_ring, epoch);
        let cursor = FeedCursor::new(0, 0);
        assert!(matches!(
            feed.read_from(cursor, ReadBudget::default()),
            FeedRead::Empty
        ));
    }

    #[test]
    fn ts_feed_epoch_mismatch_on_advance() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(16, cancel);
        ts_ring.push(Bytes::from_static(b"data"), false);
        let epoch = Arc::new(FeedEpoch::new());
        epoch.advance();
        let feed = TsFeed::new(&ts_ring, epoch);
        let old_cursor = FeedCursor::new(0, 0);
        assert!(matches!(
            feed.read_from(old_cursor, ReadBudget::default()),
            FeedRead::EpochMismatch { current_epoch: 1 }
        ));
    }

    #[test]
    fn ts_feed_retention_snapshot_tracks_retained_bytes() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(4, cancel);
        for i in 0..6 {
            ts_ring.push(Bytes::from(vec![i as u8; 2]), i == 4);
        }

        let epoch = Arc::new(FeedEpoch::new());
        let feed = TsFeed::new(&ts_ring, epoch);
        let snapshot = feed.retention_snapshot();

        assert_eq!(snapshot.head_sequence, 6);
        assert_eq!(snapshot.oldest_sequence, 2);
        assert_eq!(snapshot.retained_units, 4);
        assert_eq!(snapshot.retained_bytes, 8);
        assert_eq!(snapshot.media_age_ms, Some(0));
        assert_eq!(snapshot.largest_unit_bytes, 2);
    }

    #[test]
    fn ts_feed_reports_overrun_at_retention_boundary() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(4, cancel);
        for i in 0..6 {
            ts_ring.push(Bytes::from(vec![i as u8]), i == 4);
        }

        let epoch = Arc::new(FeedEpoch::new());
        let feed = TsFeed::new(&ts_ring, epoch);
        assert_eq!(feed.head_sequence(), 6);
        assert_eq!(feed.oldest_sequence(), 2);
        assert!(matches!(
            feed.read_from(FeedCursor::new(0, 1), ReadBudget::default()),
            FeedRead::Overrun { oldest_sequence: 2 }
        ));
    }

    #[test]
    fn ts_feed_finds_sync_points_without_registered_readers_after_wraparound() {
        let cancel = CancellationToken::new();
        let ts_ring = TsChunkRing::new(4, cancel);
        for i in 0..6 {
            ts_ring.push(Bytes::from(vec![i as u8]), i == 4);
        }

        let epoch = Arc::new(FeedEpoch::new());
        let feed = TsFeed::new(&ts_ring, epoch);
        assert_eq!(feed.latest_sync_point(), Some(FeedCursor::new(0, 4)));
        assert_eq!(feed.sync_point_at_or_after(2), Some(FeedCursor::new(0, 4)));
        assert_eq!(feed.sync_point_at_or_after(5), None);
    }

    // -----------------------------------------------------------------------
    // FeedOverrunStats
    // -----------------------------------------------------------------------

    #[test]
    fn overrun_stats_accumulate() {
        let mut stats = FeedOverrunStats::default();
        stats.record_overrun();
        stats.record_overrun();
        assert_eq!(stats.overrun_count, 2);
        assert!(stats.last_overrun_at.is_some());
        stats.record_epoch();
        assert_eq!(stats.epoch_count, 1);
        stats.record_oversized_unit();
        assert_eq!(stats.oversized_unit_count, 1);
    }
}
