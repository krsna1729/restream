//! Policy types: per-leaf limits, work budgets.
//!
//! Policy belongs to the protocol-neutral fabric. Protocol engines observe
//! limits through `WorkBudget` and report results through `EngineProgress`;
//! they do not set their own timeouts or retry delays.

use std::time::{Duration, Instant};

// ---------------------------------------------------------------------------
// LeafPolicy — per-output configuration
// ---------------------------------------------------------------------------

/// Operational policy for one egress leaf.
///
/// Exposed as configuration through `RESTREAM_EGRESS_*` environment variables.
/// Invalid zero, overflow, or contradictory values fail validation at startup.
#[derive(Debug, Clone)]
pub struct LeafPolicy {
    /// Maximum application bytes queued for this leaf before backpressure is
    /// enforced (includes protocol headers and pending wire units).
    pub max_pending_bytes: usize,

    /// Maximum feed lag (in units) before the leaf is resynchronized.
    pub max_feed_lag_units: u64,

    /// Maximum feed lag duration before resynchronization.
    pub max_feed_lag: Duration,

    /// If the leaf makes no byte progress for this long, it is closed.
    pub no_progress_timeout: Duration,

    /// Maximum time allowed for DNS resolution.
    pub resolve_timeout: Duration,

    /// Maximum time allowed for the TCP or SRT connect phase.
    pub connect_timeout: Duration,

    /// Maximum time allowed for the application-level handshake (RTMP or SRT).
    pub handshake_timeout: Duration,
}

impl Default for LeafPolicy {
    fn default() -> Self {
        Self {
            max_pending_bytes: 256 * 1024,
            max_feed_lag_units: 300,
            max_feed_lag: Duration::from_secs(10),
            no_progress_timeout: Duration::from_secs(15),
            resolve_timeout: Duration::from_secs(5),
            connect_timeout: Duration::from_secs(30),
            handshake_timeout: Duration::from_secs(10),
        }
    }
}

// ---------------------------------------------------------------------------
// LeafLimits — strict per-leaf accounting ceilings
// ---------------------------------------------------------------------------

/// Strict ceilings applied to one leaf during operation.
///
/// The first exceeded limit triggers recovery. These are enforced, not
/// advisory. A write larger than the remaining budget is split, rejected, or
/// retained as one explicitly accounted pending unit.
#[derive(Debug, Clone)]
pub struct LeafLimits {
    /// Maximum application bytes pending on the leaf at any instant.
    pub max_pending_bytes: usize,
    /// Maximum number of feed units the leaf may lag behind the head.
    pub max_lag_units: u64,
    /// Maximum time the leaf may be continuously backpressured before recovery.
    pub max_backpressure_duration: Duration,
    /// Maximum byte count of protocol handshake or control output.
    pub max_handshake_bytes: usize,
}

impl Default for LeafLimits {
    fn default() -> Self {
        Self {
            max_pending_bytes: 256 * 1024,
            max_lag_units: 300,
            max_backpressure_duration: Duration::from_secs(30),
            max_handshake_bytes: 64 * 1024,
        }
    }
}

impl LeafLimits {
    /// Derive strict limits from a `LeafPolicy`.
    pub fn from_policy(policy: &LeafPolicy) -> Self {
        Self {
            max_pending_bytes: policy.max_pending_bytes,
            max_lag_units: policy.max_feed_lag_units,
            max_backpressure_duration: policy.no_progress_timeout,
            max_handshake_bytes: 64 * 1024,
        }
    }
}

// ---------------------------------------------------------------------------
// Stall classification
// ---------------------------------------------------------------------------

/// Send-path health for one leaf, derived from combined pending state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LeafStallClass {
    /// Nothing waiting anywhere on the send path.
    Idle,
    /// Data is waiting but the most recent progress is within the deadline.
    Backpressured,
    /// Data is waiting and no progress has occurred within
    /// `max_backpressure_duration` — the leaf needs recovery.
    Stalled,
}

/// Classify a leaf's send path from its total pending bytes (application
/// plus native transport buffers), the age of its most recent progress of
/// any kind (bytes sent, protocol event, or native buffer drain), and how
/// far its read cursor lags the live feed edge.
///
/// The lag ceiling is a strict loss-of-liveness bound: a leaf whose cursor
/// sits more than `max_lag_units` behind the feed head can never catch up
/// without losing data (the ring has already advanced past its position),
/// so it is stalled regardless of progress age. It applies to protocols
/// whose catch-up is lossy (SRT's TSBPD/TLPKTDROP); lossless-TCP protocols
/// pass 0 because a lagging leaf recovers by reading flat-out.
pub fn classify_stall(
    pending_bytes: u64,
    age_since_progress: Duration,
    lag_units: u64,
    limits: &LeafLimits,
) -> LeafStallClass {
    if pending_bytes == 0 {
        return LeafStallClass::Idle;
    }
    if lag_units > limits.max_lag_units {
        return LeafStallClass::Stalled;
    }
    if age_since_progress >= limits.max_backpressure_duration {
        return LeafStallClass::Stalled;
    }
    LeafStallClass::Backpressured
}

// ---------------------------------------------------------------------------
// WorkBudget — per-visit resource envelope
// ---------------------------------------------------------------------------

/// Resource envelope granted to a protocol engine for one scheduler visit.
///
/// The engine must respect all three dimensions. Time is a guard against an
/// unexpectedly expensive serializer or native call; bytes and units provide
/// deterministic fairness.
#[derive(Debug, Clone)]
pub struct WorkBudget {
    max_units: usize,
    max_bytes: usize,
    /// Hard deadline — the engine must yield before or at this instant.
    deadline: Instant,
    spent_units: usize,
    spent_bytes: usize,
}

impl WorkBudget {
    /// Construct a budget with explicit limits.
    pub fn new(max_units: usize, max_bytes: usize, duration: Duration) -> Self {
        Self {
            // Zero would end every visit before any work; one unit is the
            // least a visit may do.
            max_units: max_units.max(1),
            max_bytes,
            deadline: Instant::now() + duration,
            spent_units: 0,
            spent_bytes: 0,
        }
    }

    /// Record `bytes` written or processed in this visit.
    pub fn debit_bytes(&mut self, bytes: usize) {
        self.spent_bytes = self.spent_bytes.saturating_add(bytes);
    }

    /// Record one media or protocol unit finished in this visit.
    pub fn debit_unit(&mut self) {
        self.spent_units = self.spent_units.saturating_add(1);
    }

    /// The visit must yield: units, bytes or time ran out.
    pub fn is_exhausted(&self) -> bool {
        self.spent_units >= self.max_units
            || self.spent_bytes >= self.max_bytes
            || Instant::now() >= self.deadline
    }

    pub fn remaining_bytes(&self) -> usize {
        self.max_bytes.saturating_sub(self.spent_bytes)
    }

    pub fn spent_bytes(&self) -> usize {
        self.spent_bytes
    }

    pub fn spent_units(&self) -> usize {
        self.spent_units
    }

    /// The visit's unit limit, for sizing a feed read (not a debit).
    pub fn max_units(&self) -> usize {
        self.max_units
    }

    /// The visit's byte limit, for sizing a feed read (not a debit).
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

/// Rung 4 (docs/assurance-roadmap.md): a visit that takes work only while
/// its budget is not exhausted, and at most `remaining_bytes()` per unit,
/// never takes more than its unit or byte limit, for any limits and any
/// amounts asked for; debits saturate instead of wrapping.
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    /// Kani cannot read the clock: every `Instant::now()` is one instant,
    /// so only the unit and byte limits end a visit.
    // Used through `#[kani::stub]`, which rustc does not count as a use.
    #[allow(dead_code)]
    fn fixed_now() -> Instant {
        // SAFETY: the all-zero `Instant` is a valid value on Linux (zero
        // seconds and nanoseconds since boot).
        unsafe { std::mem::zeroed() }
    }

    #[kani::proof]
    #[kani::stub(std::time::Instant::now, fixed_now)]
    #[kani::unwind(7)]
    #[kani::solver(kissat)]
    fn a_visit_never_takes_more_than_its_budget() {
        let max_units: usize = kani::any();
        kani::assume(max_units <= 4);
        let max_bytes: usize = kani::any();
        let mut budget = WorkBudget::new(max_units, max_bytes, Duration::from_secs(1));
        let (mut units, mut bytes) = (0_usize, 0_usize);
        for _ in 0..5 {
            if budget.is_exhausted() {
                break;
            }
            let wanted: usize = kani::any();
            let taken = wanted.min(budget.remaining_bytes());
            budget.debit_bytes(taken);
            budget.debit_unit();
            units += 1;
            bytes += taken;
        }
        kani::cover!(units == max_units.max(1), "the unit limit ends the visit");
        kani::cover!(
            units < max_units.max(1) && budget.remaining_bytes() == 0,
            "the byte limit ends the visit"
        );
        // `new` raises a zero unit limit to one: a visit may always do one unit.
        assert!(units <= max_units.max(1));
        assert!(bytes <= max_bytes);
        assert_eq!(budget.spent_units(), units);
        assert_eq!(budget.spent_bytes(), bytes);
    }

    #[kani::proof]
    #[kani::stub(std::time::Instant::now, fixed_now)]
    fn debits_saturate() {
        let mut budget = WorkBudget::new(1, kani::any(), Duration::from_secs(1));
        let (first, second): (usize, usize) = (kani::any(), kani::any());
        budget.debit_bytes(first);
        budget.debit_bytes(second);
        assert_eq!(budget.spent_bytes(), first.saturating_add(second));
        assert_eq!(
            budget.remaining_bytes(),
            budget.max_bytes().saturating_sub(budget.spent_bytes())
        );
    }
}

/// Per-visit limits retained while a shard is being created. Unlike
/// `WorkBudget`, this has no absolute deadline that can expire during startup.
#[derive(Debug, Clone, Copy)]
pub(crate) struct WorkBudgetConfig {
    pub(crate) max_units: usize,
    pub(crate) max_bytes: usize,
    pub(crate) window: Duration,
}

impl WorkBudgetConfig {
    pub(crate) fn new(max_units: usize, max_bytes: usize, window: Duration) -> Self {
        Self {
            max_units,
            max_bytes,
            window,
        }
    }

    pub(crate) fn new_visit(self) -> WorkBudget {
        WorkBudget::new(self.max_units, self.max_bytes, self.window)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn work_budget_exhausted_on_units() {
        let mut budget = WorkBudget::new(5, 10_000, Duration::from_secs(10));
        for _ in 0..4 {
            budget.debit_unit();
        }
        assert!(!budget.is_exhausted());
        budget.debit_unit();
        assert!(budget.is_exhausted());
    }

    #[test]
    fn work_budget_config_starts_a_fresh_deadline_after_factory_delay() {
        let config = WorkBudgetConfig::new(5, 100, Duration::from_millis(50));
        std::thread::sleep(Duration::from_millis(60));
        assert!(!config.new_visit().is_exhausted());
    }

    #[test]
    fn work_budget_exhausted_on_bytes() {
        let mut budget = WorkBudget::new(1000, 100, Duration::from_secs(10));
        budget.debit_bytes(99);
        assert!(!budget.is_exhausted());
        assert_eq!(budget.remaining_bytes(), 1);
        budget.debit_bytes(usize::MAX);
        assert!(budget.is_exhausted(), "an over-debit saturates");
        assert_eq!(budget.remaining_bytes(), 0);
    }

    #[test]
    fn leaf_limits_from_policy() {
        let policy = LeafPolicy {
            max_pending_bytes: 512 * 1024,
            max_feed_lag_units: 500,
            ..LeafPolicy::default()
        };
        let limits = LeafLimits::from_policy(&policy);
        assert_eq!(limits.max_pending_bytes, 512 * 1024);
        assert_eq!(limits.max_lag_units, 500);
    }

    #[test]
    fn classify_stall_splits_idle_backpressured_stalled() {
        let limits = LeafLimits {
            max_backpressure_duration: Duration::from_secs(15),
            ..LeafLimits::default()
        };
        assert_eq!(
            classify_stall(0, Duration::from_secs(3600), 0, &limits),
            LeafStallClass::Idle
        );
        assert_eq!(
            classify_stall(1, Duration::from_secs(14), 0, &limits),
            LeafStallClass::Backpressured
        );
        assert_eq!(
            classify_stall(1, Duration::from_secs(15), 0, &limits),
            LeafStallClass::Stalled
        );
    }

    /// The lag ceiling is a strict liveness bound independent of progress
    /// age: a leaf whose cursor is more than `max_lag_units` behind the
    /// head is stalled even while its transport still drains.
    #[test]
    fn classify_stall_lag_over_limit_is_stalled_regardless_of_progress() {
        let limits = LeafLimits {
            max_lag_units: 300,
            max_backpressure_duration: Duration::from_secs(15),
            ..LeafLimits::default()
        };
        assert_eq!(
            classify_stall(1, Duration::from_secs(1), 301, &limits),
            LeafStallClass::Stalled
        );
        assert_eq!(
            classify_stall(1, Duration::from_secs(1), 300, &limits),
            LeafStallClass::Backpressured
        );
        // Idle precedence is unchanged: nothing pending is not a stall even
        // if the cursor sits far behind (the next read resyncs to head).
        assert_eq!(
            classify_stall(0, Duration::from_secs(1), 10_000, &limits),
            LeafStallClass::Idle
        );
    }
}
