//! `EgressFeed` trait, `FeedCursor`, and related read types.
//!
//! The fabric consumes a bounded, sequence-addressed, immutable feed.
//! The contract is defined here; concrete adapters (for `RingBuffer` and
//! `TsChunkRing`) are implemented in Phase 2 (`journal.rs`).

// ---------------------------------------------------------------------------
// FeedCursor
// ---------------------------------------------------------------------------

/// Bookmark into a feed, carrying only position and epoch information.
///
/// Cursors do not pin retained entries. If a cursor falls behind
/// `oldest_sequence`, the feed reports [`FeedRead::Overrun`] and the leaf
/// follows the common resynchronization policy.
///
/// Feed epochs change when a discontinuity invalidates old cursors (e.g. a
/// source replacement or preparation-stage restart). An epoch mismatch is
/// handled as a resynchronization event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedCursor {
    /// Current epoch. Incremented whenever a discontinuity invalidates
    /// old cursors.
    pub epoch: u64,
    /// The sequence number the leaf will read *next*.
    pub next_sequence: u64,
}

impl FeedCursor {
    pub fn new(epoch: u64, next_sequence: u64) -> Self {
        Self {
            epoch,
            next_sequence,
        }
    }

    /// Returns `true` if this cursor has an older epoch than `other`.
    pub fn epoch_mismatch(self, other: FeedCursor) -> bool {
        self.epoch != other.epoch
    }
}

// ---------------------------------------------------------------------------
// ReadBudget
// ---------------------------------------------------------------------------

/// Maximum resources granted to a single `EgressFeed::read_from` call.
#[derive(Debug, Clone, Copy)]
pub struct ReadBudget {
    /// Maximum number of units to return in one batch.
    pub max_units: usize,
    /// Maximum total bytes to return in one batch.
    pub max_bytes: usize,
}

impl ReadBudget {
    pub fn new(max_units: usize, max_bytes: usize) -> Self {
        Self {
            max_units,
            max_bytes,
        }
    }
}

impl Default for ReadBudget {
    fn default() -> Self {
        Self {
            max_units: 64,
            max_bytes: 512 * 1024,
        }
    }
}

// ---------------------------------------------------------------------------
// FeedRead
// ---------------------------------------------------------------------------

/// Result of one `EgressFeed::read_from` call.
#[derive(Debug)]
pub enum FeedRead<U> {
    /// One or more units were available and returned.
    Units {
        units: Vec<U>,
        /// Updated cursor for the next read.
        next_cursor: FeedCursor,
    },
    /// No units are currently available at the cursor position, but the feed
    /// is healthy and the leaf should wait for a wakeup.
    Empty,
    /// The cursor fell behind the oldest retained sequence. The leaf must
    /// resynchronize.
    Overrun {
        /// Oldest sequence still available, if the caller wants to log it.
        oldest_sequence: u64,
    },
    /// The cursor's epoch does not match the feed's current epoch.
    EpochMismatch {
        /// Current feed epoch.
        current_epoch: u64,
    },
}

// ---------------------------------------------------------------------------
// EgressFeed
// ---------------------------------------------------------------------------

/// Stable contract for a bounded, immutable, sequence-addressed feed.
///
/// Implementors wrap existing ring structures (`RingBuffer`, `TsChunkRing`)
/// without rewriting them. The interface is defined here so the scheduler,
/// leaf, and test driver can work against a stable boundary before any
/// concrete implementation exists.
pub trait EgressFeed {
    /// The unit produced by this feed (e.g. `Bytes` for MPEG-TS messages,
    /// `Arc<MediaPacket>` for RTMP-compatible units).
    type Unit: Clone;

    /// The sequence number of the most recently published unit.
    /// Monotonically increasing within an epoch.
    fn head_sequence(&self) -> u64;

    /// The sequence number of the oldest retained unit.
    /// A cursor with `next_sequence < oldest_sequence` is overrun.
    fn oldest_sequence(&self) -> u64;

    /// Read up to `budget` units starting from `cursor`.
    ///
    /// Must be non-blocking and must not hold any lock that a publishing
    /// thread also holds for media work.
    fn read_from(&self, cursor: FeedCursor, budget: ReadBudget) -> FeedRead<Self::Unit>;

    /// Returns the most recent known-good synchronization point, if any.
    ///
    /// A synchronization point is a feed position from which a protocol
    /// engine can safely reconnect (e.g. a keyframe boundary).
    fn latest_sync_point(&self) -> Option<FeedCursor>;

    /// Returns the first synchronization point at or after `sequence`, or
    /// `None` if no such point exists in the retained window.
    fn sync_point_at_or_after(&self, sequence: u64) -> Option<FeedCursor>;

    /// Current epoch of this feed. An epoch mismatch invalidates cursors.
    fn epoch(&self) -> u64;

    /// Returns `true` if any data has been published at or after `sequence`.
    fn has_data_at(&self, sequence: u64) -> bool {
        self.head_sequence() >= sequence
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cursor_epoch_mismatch() {
        let a = FeedCursor::new(1, 100);
        let b = FeedCursor::new(2, 100);
        assert!(a.epoch_mismatch(b));
        assert!(!a.epoch_mismatch(a));
    }

    #[test]
    fn read_budget_defaults() {
        let b = ReadBudget::default();
        assert_eq!(b.max_units, 64);
        assert_eq!(b.max_bytes, 512 * 1024);
    }
}
