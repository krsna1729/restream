//! Bounded shared feed adaptors implementing [`EgressFeed`].
//!
//! They wrap the existing ring structures behind the trait. Two adaptors:
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
//! One pending-wake flag and one Full-retry flag per shard. Protocol:
//! 1. Publisher calls `WakeGate::notify` (AcqRel swap) after every push; a
//!    clear-to-set transition obligates one bounded delivery attempt.
//!    A `false` return coalesces into the already queued wake. If delivery is
//!    Full, clear pending and arm the separate retry bit. Closed clears
//!    pending but requires no service from the stopped shard.
//! 2. On a queued wake, the shard clears pending **before** re-reading heads.
//! 3. On fallback service, the shard clears the retry bit **before** re-reading
//!    heads, without clearing pending: an already queued wake still owns it.
//!
//! An outstanding hint is either a queued wake or the Full-retry bit, not
//! necessarily a delivery in flight. If Full produces no queued wake, the
//! active shard's bounded idle deadline still guarantees another service turn;
//! no later publication is required. `tests/egress_feed_wake_loom.rs` models
//! both queued delivery and bounded retry service.
//!
//! ## Retention
//!
//! `RingBuffer` enforces a slot-count ceiling. A reader the producer laps
//! gets an overrun and resynchronizes at the next sync point.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use bytes::Bytes;

use crate::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::ts_chunk_ring::TsChunkRing;
use restream_core::NotRetained;
use restream_core::TrackConfig;
use restream_core::{MediaPacket, MediaType};
use restream_sync::RingBuffer;

fn oldest_retained_sequence(ring: &RingBuffer) -> u64 {
    ring.get_write_idx().saturating_sub(ring.capacity()) as u64
}

fn refresh_oldest(ring: &RingBuffer, cached: &AtomicU64) -> u64 {
    let oldest = oldest_retained_sequence(ring);
    cached.store(oldest, Ordering::Relaxed);
    oldest
}

/// Both feeds' read: the epoch and retention checks, then one contiguous run
/// of `ring`'s units from `cursor` into `units` (empty on entry), each made
/// by `unit`, within `budget`.
///
/// A slot that no longer holds its sequence's packet is an overrun, even
/// while the write index still calls it retained (a batch writes its slots
/// before it moves the write index): the common run checks that once, with
/// [`RingBuffer::run_intact`]. A cursor awaiting a keyframe first skips the
/// video interframes at its position, without charging them to the budget,
/// and the run stops before the next one; the first video keyframe ends the
/// wait.
fn read_ring<U>(
    ring: &RingBuffer,
    epoch: u64,
    cached_oldest: &AtomicU64,
    cursor: FeedCursor,
    budget: ReadBudget,
    units: &mut Vec<U>,
    unit: impl Fn(&Arc<MediaPacket>) -> U,
) -> FeedRead<()> {
    if cursor.epoch != epoch {
        return FeedRead::EpochMismatch {
            current_epoch: epoch,
        };
    }
    let head = ring.get_write_idx() as u64;
    let oldest = refresh_oldest(ring, cached_oldest);
    if cursor.next_sequence < oldest {
        return FeedRead::Overrun {
            oldest_sequence: oldest,
        };
    }

    let mut next = cursor;
    let mut total_bytes = 0usize;
    // Rare: the run starts while waiting for a video keyframe. Interframes at
    // the cursor are skipped uncharged, and the run stops before the next one
    // after anything was taken; the first video keyframe ends the wait.
    while next.awaiting_keyframe
        && next.next_sequence < head
        && units.len() < budget.max_units
        && total_bytes < budget.max_bytes
    {
        let Some(packet) = ring.read_at(next.next_sequence as usize) else {
            return FeedRead::Overrun {
                oldest_sequence: refresh_oldest(ring, cached_oldest),
            };
        };
        if packet.is_video_interframe() {
            if !units.is_empty() {
                break;
            }
            next.next_sequence += 1;
            continue;
        }
        next.awaiting_keyframe = !packet.is_video_keyframe();
        total_bytes += packet.payload.len();
        units.push(unit(&packet));
        next.next_sequence += 1;
    }
    // The common run, past (or never in) a keyframe wait. A wait the loop
    // above stopped on an interframe ends this read there. Its slots are
    // loaded unchecked and the whole run checked once.
    let run_start = next.next_sequence;
    while !next.awaiting_keyframe
        && next.next_sequence < head
        && units.len() < budget.max_units
        && total_bytes < budget.max_bytes
    {
        let Some((payload_len, made)) = ring
            .read_unverified_with(next.next_sequence as usize, |packet| {
                (packet.payload.len(), unit(packet))
            })
        else {
            units.clear();
            return FeedRead::Overrun {
                oldest_sequence: refresh_oldest(ring, cached_oldest),
            };
        };
        total_bytes += payload_len;
        units.push(made);
        next.next_sequence += 1;
    }
    if next.next_sequence > run_start
        && !ring.run_intact(run_start as usize, next.next_sequence as usize)
    {
        units.clear();
        return FeedRead::Overrun {
            oldest_sequence: refresh_oldest(ring, cached_oldest),
        };
    }
    if next.next_sequence == cursor.next_sequence {
        return FeedRead::Empty;
    }
    FeedRead::Units {
        units: Vec::new(),
        next_cursor: next,
    }
}

/// `read_from` through `read_from_into`: the same read into a new batch.
fn into_owned<U>(read: FeedRead<()>, units: Vec<U>) -> FeedRead<U> {
    match read {
        FeedRead::Units { next_cursor, .. } => FeedRead::Units { units, next_cursor },
        FeedRead::Empty => FeedRead::Empty,
        FeedRead::Overrun { oldest_sequence } => FeedRead::Overrun { oldest_sequence },
        FeedRead::EpochMismatch { current_epoch } => FeedRead::EpochMismatch { current_epoch },
    }
}

// ---------------------------------------------------------------------------
// WakeGate — one outstanding notification per (feed, shard)
// ---------------------------------------------------------------------------

/// Coalescing wake gate: at most one outstanding notification per consumer.
///
/// ### Protocol
/// 1. Publisher calls [`notify`](WakeGate::notify) after every push (`AcqRel`
///    swap).  A `true` return is a clear-to-set transition: the publisher must
///    make one bounded delivery attempt. `false` means a queued wake already
///    owns pending — delivery is coalesced away.
/// 2. Shard calls [`take`](WakeGate::take) when consuming the queued wake,
///    **before** the backend checks any head (`AcqRel`).
/// 3. Shard re-reads heads after the clear; a later publication arms a new wake.
///
/// If the command channel is full, a separate retry bit preserves the hint
/// for the active loop. Clearing that bit never clears a queued wake's gate.
/// The bounded idle wait covers a full-channel retry arriving after the loop
/// has already drained its commands. `tests/egress_feed_wake_loom.rs` and the
/// production-loop regressions model/check these clear-before-observe seams.
#[derive(Debug, Default)]
pub struct WakeGate {
    pending: AtomicBool,
    /// A full command channel already keeps the shard active. Preserve one
    /// out-of-channel wake so the final publish does not require a retry.
    full_retry: AtomicBool,
}

impl WakeGate {
    pub fn new() -> Self {
        Self {
            pending: AtomicBool::new(false),
            full_retry: AtomicBool::new(false),
        }
    }

    /// Mark that new data is available. A clear-to-set transition returns
    /// `true`, obligating one bounded delivery attempt (or a Full retry hint).
    /// Returns `false` when a queued wake already owns pending.
    #[inline]
    #[must_use = "a true return obligates a delivery attempt or Full retry hint"]
    pub fn notify(&self) -> bool {
        !self.pending.swap(true, Ordering::AcqRel)
    }

    /// Clear the pending flag; returns `true` if it was set.
    ///
    /// The caller **must** re-read the feed head after this call.
    #[inline]
    pub fn take(&self) -> bool {
        self.pending.swap(false, Ordering::AcqRel)
    }

    pub(crate) fn record_full_delivery(&self) {
        self.take();
        self.full_retry.swap(true, Ordering::AcqRel);
    }

    pub(crate) fn take_full_delivery(&self) -> bool {
        self.full_retry.swap(false, Ordering::AcqRel)
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
    /// Cumulative payload bytes published to this feed (offered load).
    pub(crate) fn published_bytes(&self) -> u64 {
        self.current_ring().published_bytes()
    }

    pub fn new(ring: Arc<RingBuffer>, epoch: Arc<FeedEpoch>) -> Self {
        Self {
            ring,
            epoch,
            cached_oldest: AtomicU64::new(0),
        }
    }

    pub(crate) fn current_ring(&self) -> Arc<RingBuffer> {
        let mut ring = Arc::clone(&self.ring);
        while let Some(next) = ring.next.load_full() {
            ring = next;
        }
        ring
    }

    fn refresh_oldest_from(&self, ring: &RingBuffer) -> u64 {
        refresh_oldest(ring, &self.cached_oldest)
    }

    fn refresh_oldest(&self) -> u64 {
        self.refresh_oldest_from(&self.current_ring())
    }

    /// Clone the shared ring/epoch handles into a new, independent reader
    /// over the same underlying data — mirrors [`TsFeed::clone_reader`].
    pub fn clone_reader(&self) -> Self {
        Self {
            ring: self.ring.clone(),
            epoch: self.epoch.clone(),
            cached_oldest: AtomicU64::new(self.cached_oldest.load(Ordering::Relaxed)),
        }
    }

    /// The ring the feed's producer currently publishes into, for fabric
    /// wake subscriptions.
    pub fn publication_ring(&self) -> Arc<RingBuffer> {
        self.current_ring()
    }

    /// The track configuration that applies to the unit at `sequence`, read
    /// from the ring the feed currently follows
    /// ([`RingBuffer::track_config_at`]).
    pub fn track_config_at(
        &self,
        sequence: u64,
        media_type: MediaType,
        track_index: u32,
    ) -> Result<Option<Arc<TrackConfig>>, NotRetained> {
        let sequence = usize::try_from(sequence).map_err(|_| NotRetained)?;
        self.current_ring()
            .track_config_at(sequence, media_type, track_index)
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
        self.current_ring().get_write_idx() as u64
    }

    fn oldest_sequence(&self) -> u64 {
        self.refresh_oldest()
    }

    fn epoch(&self) -> u64 {
        self.epoch.current()
    }

    fn read_from(&self, cursor: FeedCursor, budget: ReadBudget) -> FeedRead<Self::Unit> {
        let mut units = Vec::new();
        let read = self.read_from_into(cursor, budget, &mut units);
        into_owned(read, units)
    }

    fn read_from_into(
        &self,
        cursor: FeedCursor,
        budget: ReadBudget,
        units: &mut Vec<Self::Unit>,
    ) -> FeedRead<()> {
        units.clear();
        read_ring(
            &self.current_ring(),
            self.epoch.current(),
            &self.cached_oldest,
            cursor,
            budget,
            units,
            Arc::clone,
        )
    }

    fn latest_sync_point(&self) -> Option<FeedCursor> {
        let epoch = self.epoch.current();
        let ring = self.current_ring();
        let head = ring.get_write_idx();
        // `fast_forward` returns the most recent keyframe index; we use it
        // as a read-only O(1) lookup (it only makes sense when head > 0).
        if head == 0 {
            return None;
        }
        let kf_idx = ring.fast_forward(head);
        if kf_idx >= self.refresh_oldest_from(&ring) as usize && kf_idx < head {
            Some(FeedCursor::new(epoch, kf_idx as u64))
        } else {
            None
        }
    }

    fn sync_point_at_or_after(&self, sequence: u64) -> Option<FeedCursor> {
        let epoch = self.epoch.current();
        let ring = self.current_ring();
        let head = ring.get_write_idx() as u64;
        let oldest = self.refresh_oldest_from(&ring);
        let start = sequence.max(oldest);

        // Linear scan — only used during resync, not on the hot path.
        for idx in start..head {
            if ring
                .read_at(idx as usize)
                .is_some_and(|p| p.media_type == restream_core::MediaType::Video && p.is_keyframe)
            {
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
    /// Cumulative TS payload bytes published to this feed (offered load).
    pub(crate) fn published_bytes(&self) -> u64 {
        self.ring.published_bytes()
    }

    /// Construct from a `TsChunkRing` by cloning its inner `Arc<RingBuffer>`.
    pub fn new(ts_ring: &TsChunkRing, epoch: Arc<FeedEpoch>) -> Self {
        Self {
            ring: ts_ring.ring.clone(),
            epoch,
            cached_oldest: AtomicU64::new(0),
        }
    }

    pub fn clone_reader(&self) -> Self {
        Self {
            ring: self.ring.clone(),
            epoch: self.epoch.clone(),
            cached_oldest: AtomicU64::new(self.cached_oldest.load(Ordering::Relaxed)),
        }
    }

    fn refresh_oldest(&self) -> u64 {
        refresh_oldest(&self.ring, &self.cached_oldest)
    }

    /// The inner ring the TS producer publishes into, for fabric wake
    /// subscriptions.
    pub fn publication_ring(&self) -> Arc<RingBuffer> {
        Arc::clone(&self.ring)
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
        let mut units = Vec::new();
        let read = self.read_from_into(cursor, budget, &mut units);
        into_owned(read, units)
    }

    fn read_from_into(
        &self,
        cursor: FeedCursor,
        budget: ReadBudget,
        units: &mut Vec<Self::Unit>,
    ) -> FeedRead<()> {
        units.clear();
        // `Bytes` clones are O(1): reference-counted. The payload is cloned
        // under the slot's lock, without a reference to its packet.
        read_ring(
            &self.ring,
            self.epoch.current(),
            &self.cached_oldest,
            cursor,
            budget,
            units,
            |packet| packet.payload.clone(),
        )
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
                .is_some_and(|p| p.is_video_keyframe())
            {
                return Some(FeedCursor::new(epoch, idx));
            }
        }
        None
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests;
