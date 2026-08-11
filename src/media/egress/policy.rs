//! Policy types: per-leaf limits, retry state, work budgets.
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

    /// If the leaf makes no byte progress for this long, it is closed and
    /// enters `RetryWait`.
    pub no_progress_timeout: Duration,

    /// Maximum time allowed for DNS resolution.
    pub resolve_timeout: Duration,

    /// Maximum time allowed for the TCP or SRT connect phase.
    pub connect_timeout: Duration,

    /// Maximum time allowed for the application-level handshake (RTMP or SRT).
    pub handshake_timeout: Duration,

    /// Retry backoff parameters.
    pub retry: RetryPolicy,
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
            retry: RetryPolicy::default(),
        }
    }
}

// ---------------------------------------------------------------------------
// RetryPolicy
// ---------------------------------------------------------------------------

/// Capped exponential backoff parameters with jitter.
#[derive(Debug, Clone)]
pub struct RetryPolicy {
    /// Minimum delay before the first reconnect attempt.
    pub min_delay: Duration,
    /// Maximum delay cap for exponential backoff.
    pub max_delay: Duration,
    /// Multiplier applied to the delay after each failure (e.g. 2.0).
    pub multiplier: f64,
    /// Fractional jitter factor in `[0.0, 1.0]`. A value of 0.25 adds up to
    /// 25 percent random spread to each computed delay.
    pub jitter: f64,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        Self {
            min_delay: Duration::from_millis(500),
            max_delay: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: 0.25,
        }
    }
}

impl RetryPolicy {
    /// Compute the next retry delay given the number of consecutive failures.
    ///
    /// Returns a duration in `[min_delay, max_delay]` with jitter applied.
    /// Uses a simple deterministic LCG for jitter so the result is pure and
    /// testable. Production callers may seed with process entropy if desired.
    pub fn next_delay(&self, attempt: u32, jitter_seed: u64) -> Duration {
        // Exponential component, capped at max.
        let base_secs =
            self.min_delay.as_secs_f64() * self.multiplier.powi(attempt.saturating_sub(1) as i32);
        let capped_secs = base_secs.min(self.max_delay.as_secs_f64());

        // Jitter: scale by [1 - jitter, 1 + jitter].
        // LCG noise in [0, 1).
        let noise = lcg_f64(jitter_seed);
        let jitter_factor = 1.0 - self.jitter + 2.0 * self.jitter * noise;

        let total_secs = (capped_secs * jitter_factor).max(self.min_delay.as_secs_f64());
        Duration::from_secs_f64(total_secs.min(self.max_delay.as_secs_f64()))
    }
}

/// Simple LCG pseudo-random value in `[0, 1)` for jitter.
fn lcg_f64(seed: u64) -> f64 {
    // Constants from Knuth.
    let v = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    (v >> 11) as f64 / (1u64 << 53) as f64
}

// ---------------------------------------------------------------------------
// RetryState — per-leaf mutable state
// ---------------------------------------------------------------------------

/// Mutable retry tracking for one leaf.
#[derive(Debug, Clone, Default)]
pub struct RetryState {
    /// Number of consecutive connection or handshake failures.
    pub consecutive_failures: u32,
    /// Monotonically increasing seed for jitter, advanced after each use.
    pub jitter_seed: u64,
    /// Absolute time when the leaf may next attempt to connect.
    pub retry_after: Option<Instant>,
}

impl RetryState {
    /// Record a failure and compute the next retry-after time.
    pub fn record_failure(&mut self, policy: &RetryPolicy) -> Instant {
        self.consecutive_failures = self.consecutive_failures.saturating_add(1);
        self.jitter_seed = self.jitter_seed.wrapping_add(1);
        let delay = policy.next_delay(self.consecutive_failures, self.jitter_seed);
        let after = Instant::now() + delay;
        self.retry_after = Some(after);
        after
    }

    /// Reset failure count after a successful connection.
    pub fn record_success(&mut self) {
        self.consecutive_failures = 0;
        self.retry_after = None;
    }

    /// Whether the leaf may attempt to connect right now.
    pub fn can_connect_now(&self) -> bool {
        self.retry_after.is_none_or(|t| Instant::now() >= t)
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
#[derive(Debug, Clone, Copy)]
pub struct WorkBudget {
    /// Maximum media or protocol units to process in this visit.
    pub max_units: usize,
    /// Maximum bytes to write or process in this visit.
    pub max_bytes: usize,
    /// Hard deadline — the engine must yield before or at this instant.
    pub deadline: Instant,
}

impl WorkBudget {
    /// Construct a budget with explicit limits.
    pub fn new(max_units: usize, max_bytes: usize, duration: Duration) -> Self {
        Self {
            max_units,
            max_bytes,
            deadline: Instant::now() + duration,
        }
    }

    /// Returns true if any budget dimension is exhausted.
    pub fn is_exhausted(&self, consumed_units: usize, consumed_bytes: usize) -> bool {
        consumed_units >= self.max_units
            || consumed_bytes >= self.max_bytes
            || Instant::now() >= self.deadline
    }

    /// Returns remaining byte budget.
    pub fn remaining_bytes(&self, consumed: usize) -> usize {
        self.max_bytes.saturating_sub(consumed)
    }

    /// Returns remaining unit budget.
    pub fn remaining_units(&self, consumed: usize) -> usize {
        self.max_units.saturating_sub(consumed)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retry_delay_respects_min() {
        let policy = RetryPolicy {
            min_delay: Duration::from_millis(200),
            max_delay: Duration::from_secs(30),
            multiplier: 2.0,
            jitter: 0.0,
        };
        // First attempt should be min_delay exactly (no jitter, multiplier^0 = 1).
        let delay = policy.next_delay(1, 0);
        assert_eq!(delay, Duration::from_millis(200));
    }

    #[test]
    fn retry_delay_respects_max() {
        let policy = RetryPolicy {
            min_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(2),
            multiplier: 10.0,
            jitter: 0.0,
        };
        // Large attempt count should be capped at max_delay.
        for attempt in 10..20 {
            let delay = policy.next_delay(attempt, 0);
            assert!(
                delay <= Duration::from_secs(2),
                "attempt {attempt}: {delay:?} > 2s"
            );
        }
    }

    #[test]
    fn retry_delay_grows_monotonically_without_jitter() {
        let policy = RetryPolicy {
            min_delay: Duration::from_millis(100),
            max_delay: Duration::from_secs(60),
            multiplier: 2.0,
            jitter: 0.0,
        };
        let delays: Vec<_> = (1..8).map(|a| policy.next_delay(a, 0)).collect();
        for w in delays.windows(2) {
            assert!(w[0] <= w[1], "not monotonic: {:?} > {:?}", w[0], w[1]);
        }
    }

    #[test]
    fn retry_state_resets_on_success() {
        let policy = RetryPolicy::default();
        let mut state = RetryState::default();
        state.record_failure(&policy);
        state.record_failure(&policy);
        assert_eq!(state.consecutive_failures, 2);
        state.record_success();
        assert_eq!(state.consecutive_failures, 0);
        assert!(state.retry_after.is_none());
    }

    #[test]
    fn work_budget_exhausted_on_units() {
        let budget = WorkBudget::new(5, 10_000, Duration::from_secs(10));
        assert!(!budget.is_exhausted(4, 0));
        assert!(budget.is_exhausted(5, 0));
    }

    #[test]
    fn work_budget_exhausted_on_bytes() {
        let budget = WorkBudget::new(1000, 100, Duration::from_secs(10));
        assert!(!budget.is_exhausted(0, 99));
        assert!(budget.is_exhausted(0, 100));
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
