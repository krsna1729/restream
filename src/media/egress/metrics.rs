//! Metric structures for the egress fabric.
//!
//! Metrics are separated into three dimensions: per-shard, per-leaf, and
//! per-feed. Per-leaf details belong in runtime snapshots; aggregate time
//! series are labeled by shard, protocol, lifecycle, and reason enums to
//! avoid unbounded cardinality.
//!
//! Hot-path metric updates use local counters flushed periodically rather than
//! shared atomics on every packet.

use std::time::{Duration, Instant};

use crate::media::egress::command::ShardId;
use crate::media::egress::lifecycle::LeafLifecycle;

// ---------------------------------------------------------------------------
// ShardMetrics
// ---------------------------------------------------------------------------

/// Counters and gauges for one egress shard, updated locally and published
/// on a configurable cadence.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ShardMetrics {
    pub shard_id: Option<ShardId>,

    // --- Leaf counts ---
    /// Total leaves currently assigned to this shard.
    pub leaves_total: u32,
    /// Leaves in each lifecycle state (indexed by ordinal — filled at publish
    /// time from the actual slab in Phase 3).
    pub leaves_active: u32,
    pub leaves_connecting: u32,
    pub leaves_retry_wait: u32,
    pub leaves_closing: u32,

    // --- Ready queue ---
    /// Current depth of the ready queue.
    pub ready_depth: u32,
    /// Highest ready_depth observed since last publish.
    pub ready_depth_hwm: u32,

    // --- Command channel ---
    pub command_depth: u32,
    pub commands_processed: u64,

    pub timers_processed: u64,
    pub pending_timers: u32,

    // --- Feed wakes ---
    /// Number of wakeups that found new feed data.
    pub feed_wakes_useful: u64,
    /// Number of wakeups that found nothing new.
    pub feed_wakes_empty: u64,
    /// Total leaf feed overruns / resynchronization events.
    pub feed_resyncs: u64,

    // --- Loop statistics ---
    pub loop_iterations: u64,
    pub media_ticks: u64,
    /// Sum of loop durations for latency percentile computation (Phase 3).
    pub loop_duration_sum_us: u64,

    // --- Connect / handshake concurrency ---
    pub concurrent_connects: u32,
    pub concurrent_handshakes: u32,

    // --- Retry ---
    pub retry_events: u64,

    // --- Driver call violations ---
    /// Number of advance() calls that overran their time budget.
    pub driver_budget_violations: u64,
    /// Total wall time spent in overrunning advance() calls.
    pub driver_overrun_us: u64,

    /// Time this snapshot was collected.
    pub collected_at: Option<Instant>,
}

impl ShardMetrics {
    pub fn new(shard_id: ShardId) -> Self {
        Self {
            shard_id: Some(shard_id),
            collected_at: Some(Instant::now()),
            ..Default::default()
        }
    }

    /// Update the ready-queue high-water mark.
    pub fn observe_ready_depth(&mut self, depth: u32) {
        self.ready_depth = depth;
        if depth > self.ready_depth_hwm {
            self.ready_depth_hwm = depth;
        }
    }

    /// Record a useful (non-empty) feed wakeup.
    pub fn record_useful_wake(&mut self) {
        self.feed_wakes_useful += 1;
    }

    /// Record an empty (spurious or coalesced) feed wakeup.
    pub fn record_empty_wake(&mut self) {
        self.feed_wakes_empty += 1;
    }

    /// Record a loop iteration with its measured duration.
    pub fn record_loop_iteration(&mut self, duration: Duration) {
        self.loop_iterations += 1;
        self.loop_duration_sum_us = self
            .loop_duration_sum_us
            .saturating_add(duration.as_micros() as u64);
    }

    /// Record a driver (advance()) budget violation.
    pub fn record_driver_violation(&mut self, overrun: Duration) {
        self.driver_budget_violations += 1;
        self.driver_overrun_us = self
            .driver_overrun_us
            .saturating_add(overrun.as_micros() as u64);
    }
}

// ---------------------------------------------------------------------------
// LeafMetrics
// ---------------------------------------------------------------------------

/// Per-leaf progress snapshot.
///
/// Emitted as a diagnostic snapshot; not exported as time-series directly
/// to avoid cardinality explosion.
#[derive(Debug, Clone, Default)]
pub struct LeafMetrics {
    pub lifecycle: Option<LeafLifecycle>,

    /// Age of last byte progress in milliseconds.
    pub byte_progress_age_ms: Option<u64>,
    /// Age of last protocol progress in milliseconds.
    pub protocol_progress_age_ms: Option<u64>,

    /// Current feed lag in units behind the head.
    pub feed_lag_units: u64,
    /// Pending application bytes.
    pub pending_bytes: usize,

    // --- Would-block and partial-write counts ---
    pub would_block_count: u64,
    pub partial_write_count: u64,

    // --- Resyncs and overruns ---
    pub resync_count: u32,
    pub overrun_count: u32,

    // --- Retry ---
    pub retry_attempt: u32,

    // --- Total sent ---
    pub bytes_sent: u64,
    pub units_sent: u64,

    pub bytes_discarded: u64,
    pub units_discarded: u64,
}

impl LeafMetrics {
    pub fn record_sent(&mut self, bytes: u64, units: u64) {
        self.bytes_sent = self.bytes_sent.saturating_add(bytes);
        self.units_sent = self.units_sent.saturating_add(units);
    }

    pub fn record_discarded(&mut self, bytes: u64, units: u64) {
        self.bytes_discarded = self.bytes_discarded.saturating_add(bytes);
        self.units_discarded = self.units_discarded.saturating_add(units);
    }
}

// ---------------------------------------------------------------------------
// FeedMetrics
// ---------------------------------------------------------------------------

/// Per-feed statistics.
#[derive(Debug, Clone, Default)]
pub struct FeedMetrics {
    /// Sequence number of the newest entry.
    pub head_sequence: u64,
    /// Sequence number of the oldest retained entry.
    pub oldest_sequence: u64,
    /// Retained bytes.
    pub retained_bytes: u64,
    pub retained_media_age_ms: Option<u64>,
    pub oversized_unit_count: u64,
    /// Number of shards currently subscribed.
    pub subscriber_shard_count: u32,
    /// Number of coalesced wakeup notifications sent.
    pub coalesced_wake_count: u64,
    /// Cumulative overrun events.
    pub overrun_count: u64,
    /// Age of the most recent synchronization point in milliseconds.
    pub sync_point_age_ms: Option<u64>,
}

// ---------------------------------------------------------------------------
// Metric name constants
// ---------------------------------------------------------------------------

/// Stable metric name constants matching the observability spec.
///
/// These are string literals intended for use with the repository's existing
/// metrics plumbing (Phase 3 integration).
pub mod names {
    pub const FABRIC_SHARDS: &str = "egress.fabric.shards";
    pub const FABRIC_LEAVES: &str = "egress.fabric.leaves";
    pub const SHARD_READY_DEPTH: &str = "egress.shard.ready_depth";
    pub const SHARD_SERVICE_DELAY_MS: &str = "egress.shard.service_delay_ms";
    pub const SHARD_LOOP_DURATION_US: &str = "egress.shard.loop_duration_us";
    pub const SHARD_HEARTBEAT_AGE_MS: &str = "egress.shard.heartbeat_age_ms";
    pub const SHARD_COMMAND_DEPTH: &str = "egress.shard.command_depth";
    pub const FEED_RETAINED_BYTES: &str = "egress.feed.retained_bytes";
    pub const FEED_RETAINED_MEDIA_AGE_MS: &str = "egress.feed.retained_media_age_ms";
    pub const FEED_OVERSIZED_UNITS: &str = "egress.feed.oversized_units";
    pub const FEED_LAG_MS: &str = "egress.feed.lag_ms";
    pub const FEED_WAKE_COALESCED: &str = "egress.feed.wake_coalesced";
    pub const FEED_OVERRUNS: &str = "egress.feed.overruns";
    pub const LEAF_PENDING_BYTES: &str = "egress.leaf.pending_bytes";
    pub const LEAF_PROGRESS_AGE_MS: &str = "egress.leaf.progress_age_ms";
    pub const LEAF_WOULD_BLOCK: &str = "egress.leaf.would_block";
    pub const LEAF_PARTIAL_WRITES: &str = "egress.leaf.partial_writes";
    pub const LEAF_RESYNCS: &str = "egress.leaf.resyncs";
    pub const LEAF_RETRY_ATTEMPT: &str = "egress.leaf.retry_attempt";
    pub const LEAF_BYTES_SENT: &str = "egress.leaf.bytes_sent";
    pub const LEAF_UNITS_SENT: &str = "egress.leaf.units_sent";
    pub const LEAF_BYTES_DISCARDED: &str = "egress.leaf.bytes_discarded";
    pub const LEAF_UNITS_DISCARDED: &str = "egress.leaf.units_discarded";
    pub const DRIVER_CALL_DURATION_US: &str = "egress.driver.call_duration_us";
    pub const DRIVER_BUDGET_VIOLATIONS: &str = "egress.driver.budget_violations";
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shard_metrics_hwm() {
        let mut m = ShardMetrics::new(ShardId::new(0));
        m.observe_ready_depth(10);
        m.observe_ready_depth(5);
        m.observe_ready_depth(20);
        assert_eq!(m.ready_depth, 20);
        assert_eq!(m.ready_depth_hwm, 20);
    }

    #[test]
    fn loop_iteration_accumulates() {
        let mut m = ShardMetrics::new(ShardId::new(0));
        m.record_loop_iteration(Duration::from_micros(500));
        m.record_loop_iteration(Duration::from_micros(300));
        assert_eq!(m.loop_iterations, 2);
        assert_eq!(m.loop_duration_sum_us, 800);
    }

    #[test]
    fn driver_violation_accumulates() {
        let mut m = ShardMetrics::new(ShardId::new(1));
        m.record_driver_violation(Duration::from_micros(1_000));
        m.record_driver_violation(Duration::from_micros(2_000));
        assert_eq!(m.driver_budget_violations, 2);
        assert_eq!(m.driver_overrun_us, 3_000);
    }

    #[test]
    fn wake_counts_separate() {
        let mut m = ShardMetrics::new(ShardId::new(0));
        m.record_useful_wake();
        m.record_useful_wake();
        m.record_empty_wake();
        assert_eq!(m.feed_wakes_useful, 2);
        assert_eq!(m.feed_wakes_empty, 1);
    }

    #[test]
    fn leaf_metrics_keep_sent_and_discarded_totals_separate() {
        let mut m = LeafMetrics::default();
        m.record_sent(10, 1);
        m.record_discarded(30, 3);
        m.record_sent(5, 2);
        m.record_discarded(7, 4);

        assert_eq!(m.bytes_sent, 15);
        assert_eq!(m.units_sent, 3);
        assert_eq!(m.bytes_discarded, 37);
        assert_eq!(m.units_discarded, 7);
    }

    #[test]
    fn metric_names_not_empty() {
        // Smoke-check that constants are non-empty (catches accidental blanks).
        assert!(!names::LEAF_PENDING_BYTES.is_empty());
        assert!(!names::DRIVER_BUDGET_VIOLATIONS.is_empty());
        assert!(!names::FEED_RETAINED_MEDIA_AGE_MS.is_empty());
        assert!(!names::FEED_OVERSIZED_UNITS.is_empty());
        assert!(!names::LEAF_BYTES_SENT.is_empty());
        assert!(!names::LEAF_BYTES_DISCARDED.is_empty());
    }
}
