//! Per-leaf state: `Leaf<P>`, `LeafCommon`, and associated types.
//!
//! Every output has one protocol-neutral shell (`LeafCommon`) and one
//! specialized protocol value (`P`). The shell owns all shared policy,
//! scheduling, and accounting state.

use std::time::Instant;

use crate::media::egress::command::{FeedId, OutputId};
use crate::media::egress::feed::FeedCursor;
use crate::media::egress::lifecycle::LeafLifecycle;
use crate::media::egress::policy::{LeafLimits, RetryState};
use crate::media::egress::scheduler::ScheduleState;

// ---------------------------------------------------------------------------
// LeafDeadlines
// ---------------------------------------------------------------------------

/// Absolute deadlines tracked per-leaf.
#[derive(Debug, Clone)]
pub struct LeafDeadlines {
    /// If `Some`, the leaf must complete its resolve phase by this instant.
    pub resolve_by: Option<Instant>,
    /// If `Some`, the leaf must complete the connect phase by this instant.
    pub connect_by: Option<Instant>,
    /// If `Some`, the leaf must complete the handshake by this instant.
    pub handshake_by: Option<Instant>,
    /// If `Some`, the leaf must make byte progress by this instant or be closed.
    pub progress_by: Option<Instant>,
    /// If `Some`, the leaf must not remain backpressured beyond this instant.
    pub backpressure_by: Option<Instant>,
}

impl LeafDeadlines {
    pub fn none() -> Self {
        Self {
            resolve_by: None,
            connect_by: None,
            handshake_by: None,
            progress_by: None,
            backpressure_by: None,
        }
    }

    /// Returns `true` if any deadline has passed.
    pub fn any_expired(&self, now: Instant) -> bool {
        self.resolve_by.is_some_and(|d| now >= d)
            || self.connect_by.is_some_and(|d| now >= d)
            || self.handshake_by.is_some_and(|d| now >= d)
            || self.progress_by.is_some_and(|d| now >= d)
            || self.backpressure_by.is_some_and(|d| now >= d)
    }
}

// ---------------------------------------------------------------------------
// ProgressState
// ---------------------------------------------------------------------------

/// Byte-progress tracking used to detect stalled leaves.
#[derive(Debug, Clone)]
pub struct ProgressState {
    /// Instant of the last successful application-byte send.
    pub last_byte_progress: Option<Instant>,
    /// Instant of the last protocol-level acknowledgement or event.
    pub last_protocol_progress: Option<Instant>,
    /// Total bytes sent over this connection lifetime.
    pub total_bytes_sent: u64,
    /// Total media units consumed.
    pub total_units_sent: u64,
    pub total_bytes_discarded: u64,
    pub total_units_discarded: u64,
    /// Number of times the leaf has been resynchronized.
    pub resync_count: u32,
    /// Number of feed overruns recorded.
    pub overrun_count: u32,
}

impl ProgressState {
    pub fn new() -> Self {
        Self {
            last_byte_progress: None,
            last_protocol_progress: None,
            total_bytes_sent: 0,
            total_units_sent: 0,
            total_bytes_discarded: 0,
            total_units_discarded: 0,
            resync_count: 0,
            overrun_count: 0,
        }
    }

    /// Record that `bytes` were sent and `units` consumed.
    pub fn record_send(&mut self, bytes: usize, units: usize) {
        let now = Instant::now();
        if bytes > 0 {
            self.last_byte_progress = Some(now);
            self.total_bytes_sent = self.total_bytes_sent.saturating_add(bytes as u64);
        }
        if units > 0 {
            self.last_protocol_progress = Some(now);
            self.total_units_sent = self.total_units_sent.saturating_add(units as u64);
        }
    }

    pub fn record_discard(&mut self, bytes: usize, units: usize) {
        let now = Instant::now();
        if bytes > 0 {
            self.last_byte_progress = Some(now);
            self.total_bytes_discarded = self.total_bytes_discarded.saturating_add(bytes as u64);
        }
        if units > 0 {
            self.last_protocol_progress = Some(now);
            self.total_units_discarded = self.total_units_discarded.saturating_add(units as u64);
        }
    }

    /// Age of the last byte progress in seconds, or `None` if no bytes sent.
    pub fn byte_progress_age_secs(&self, now: Instant) -> Option<f64> {
        self.last_byte_progress
            .map(|t| now.duration_since(t).as_secs_f64())
    }
}

impl Default for ProgressState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// LeafCommon
// ---------------------------------------------------------------------------

/// Protocol-neutral state carried by every egress leaf.
///
/// The shard has exclusive mutable access; no hot-path global lock is needed.
#[derive(Debug)]
pub struct LeafCommon {
    /// Stable identity of the output this leaf serves.
    pub output_id: OutputId,
    /// Monotonically increasing generation. Events from an older generation
    /// are silently ignored.
    pub generation: u64,
    /// Which prepared feed this leaf is consuming.
    pub feed: FeedId,
    /// Current read position in the feed.
    pub cursor: FeedCursor,
    /// Current lifecycle state.
    pub lifecycle: LeafLifecycle,
    /// Scheduler visibility and deficit accounting.
    pub schedule: ScheduleState,
    /// Absolute deadlines (resolve, connect, handshake, progress).
    pub deadlines: LeafDeadlines,
    /// Retry backoff state.
    pub retry: RetryState,
    /// Byte-progress tracking.
    pub progress: ProgressState,
    /// Strict per-leaf resource ceilings.
    pub limits: LeafLimits,
    /// Application bytes currently pending on this leaf (headers + payload
    /// retained but not yet accepted by the transport).
    pub pending_application_bytes: usize,
}

impl LeafCommon {
    /// Create a new leaf shell in the `Created` state.
    pub fn new(output_id: OutputId, generation: u64, feed: FeedId, limits: LeafLimits) -> Self {
        Self {
            output_id,
            generation,
            feed,
            cursor: FeedCursor::new(0, 0),
            lifecycle: LeafLifecycle::Created,
            schedule: ScheduleState::new(),
            deadlines: LeafDeadlines::none(),
            retry: RetryState::default(),
            progress: ProgressState::new(),
            limits,
            pending_application_bytes: 0,
        }
    }

    /// Returns `true` if this event's generation matches the leaf's current
    /// generation. Stale events (lower generation) are silently ignored.
    pub fn is_current_generation(&self, generation_val: u64) -> bool {
        generation_val == self.generation
    }

    /// Returns `true` if the leaf has exceeded any strict resource ceiling.
    pub fn is_limit_exceeded(&self) -> bool {
        self.pending_application_bytes > self.limits.max_pending_bytes
    }
}

// ---------------------------------------------------------------------------
// Leaf<P>
// ---------------------------------------------------------------------------

/// A complete egress leaf: protocol-neutral shell + protocol-specific state.
///
/// `P` is typically an enum or struct owned by the protocol module (e.g.
/// `RtmpLeaf`, `SrtLeaf`). During Phase 1 tests, `P` is [`FakeProtocol`].
#[derive(Debug)]
pub struct Leaf<P> {
    pub common: LeafCommon,
    pub protocol: P,
}

impl<P> Leaf<P> {
    pub fn new(common: LeafCommon, protocol: P) -> Self {
        Self { common, protocol }
    }

    pub fn output_id(&self) -> &OutputId {
        &self.common.output_id
    }

    pub fn generation(&self) -> u64 {
        self.common.generation
    }

    pub fn lifecycle(&self) -> LeafLifecycle {
        self.common.lifecycle
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::egress::command::{FeedId, OutputId};
    use crate::media::egress::policy::LeafLimits;
    use std::time::Duration;

    fn make_common(id: &str, generation_val: u64) -> LeafCommon {
        LeafCommon::new(
            OutputId::new(id),
            generation_val,
            FeedId::new("f"),
            LeafLimits::default(),
        )
    }

    #[test]
    fn new_leaf_starts_in_created() {
        let c = make_common("out-1", 1);
        assert_eq!(c.lifecycle, LeafLifecycle::Created);
        assert!(!c.is_limit_exceeded());
    }

    #[test]
    fn generation_check() {
        let c = make_common("out-1", 5);
        assert!(c.is_current_generation(5));
        assert!(!c.is_current_generation(4));
        assert!(!c.is_current_generation(6));
    }

    #[test]
    fn pending_bytes_limit() {
        let mut c = make_common("out-1", 1);
        assert!(!c.is_limit_exceeded());
        c.pending_application_bytes = c.limits.max_pending_bytes + 1;
        assert!(c.is_limit_exceeded());
    }

    #[test]
    fn progress_state_records_sends() {
        let mut p = ProgressState::new();
        assert!(p.last_byte_progress.is_none());
        p.record_send(1024, 2);
        assert!(p.last_byte_progress.is_some());
        assert_eq!(p.total_bytes_sent, 1024);
        assert_eq!(p.total_units_sent, 2);
        assert_eq!(p.total_bytes_discarded, 0);
        assert_eq!(p.total_units_discarded, 0);
    }

    #[test]
    fn progress_state_records_discards_without_counting_them_as_sends() {
        let mut p = ProgressState::new();
        assert!(p.last_byte_progress.is_none());
        p.record_discard(1024, 2);
        assert!(p.last_byte_progress.is_some());
        assert!(p.last_protocol_progress.is_some());
        assert_eq!(p.total_bytes_sent, 0);
        assert_eq!(p.total_units_sent, 0);
        assert_eq!(p.total_bytes_discarded, 1024);
        assert_eq!(p.total_units_discarded, 2);
    }

    #[test]
    fn deadlines_none_not_expired() {
        let d = LeafDeadlines::none();
        // Even far in the past the none-deadline should not expire.
        let past = Instant::now() - Duration::from_secs(100);
        assert!(!d.any_expired(past));
    }

    #[test]
    fn deadlines_expire() {
        let mut d = LeafDeadlines::none();
        d.connect_by = Some(Instant::now() - Duration::from_millis(1));
        assert!(d.any_expired(Instant::now()));
    }

    #[test]
    fn leaf_wrapper_delegates() {
        let c = make_common("out-1", 3);
        let leaf = Leaf::new(c, "fake-protocol");
        assert_eq!(leaf.output_id().as_str(), "out-1");
        assert_eq!(leaf.generation(), 3);
        assert_eq!(leaf.lifecycle(), LeafLifecycle::Created);
    }
}
