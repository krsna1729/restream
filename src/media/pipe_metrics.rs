//! Back-pressure counters for external subprocess stages (FFmpeg stdin/stdout pipe).
//!
//! [`PipeMetrics`] is kept separate from [`super::stage_metrics::StageMetrics`]
//! because it only exists for the external transcoder: internal and
//! MemoryQueue-backed stages have no kernel pipe to observe.
//!
//! The engine stores `Arc<PipeMetrics>` on the owning `StageRuntime`. The
//! processing graph and telemetry read it from that runtime to populate
//! `pipeMetrics` on transcoder nodes.

use std::sync::atomic::{AtomicU64, Ordering};

/// Typed snapshot of a [`PipeMetrics`] counter set. JSON assembly (if
/// needed) happens at the API/runtime-view edge, not here.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PipeMetricsSnapshot {
    pub stalls: u64,
    pub stall_us: u64,
    pub avg_stall_us: u64,
    pub idles: u64,
    pub idle_us: u64,
    pub avg_idle_us: u64,
}

#[derive(Debug, Default)]
pub struct PipeMetrics {
    /// Stdin writes that stalled: the kernel pipe buffer was full because
    /// FFmpeg was not consuming input fast enough.
    pub stalls: AtomicU64,
    /// Cumulative microseconds spent blocked on stalled stdin writes.
    pub stall_us: AtomicU64,
    /// Stdout reads that idled: the kernel pipe was empty because FFmpeg
    /// had not produced output yet (encode is CPU-bound or stalled).
    pub idles: AtomicU64,
    /// Cumulative microseconds spent waiting for idle stdout reads.
    pub idle_us: AtomicU64,
}

impl PipeMetrics {
    #[inline]
    pub fn record_stall(&self, us: u64) {
        self.stalls.fetch_add(1, Ordering::Relaxed);
        self.stall_us.fetch_add(us, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_idle(&self, us: u64) {
        self.idles.fetch_add(1, Ordering::Relaxed);
        self.idle_us.fetch_add(us, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> PipeMetricsSnapshot {
        let stalls = self.stalls.load(Ordering::Relaxed);
        let stall_us = self.stall_us.load(Ordering::Relaxed);
        let idles = self.idles.load(Ordering::Relaxed);
        let idle_us = self.idle_us.load(Ordering::Relaxed);
        PipeMetricsSnapshot {
            stalls,
            stall_us,
            avg_stall_us: stall_us.checked_div(stalls).unwrap_or(0),
            idles,
            idle_us,
            avg_idle_us: idle_us.checked_div(idles).unwrap_or(0),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_snapshot_has_zeroed_counters_and_no_division_by_zero() {
        let metrics = PipeMetrics::default();
        let snap = metrics.snapshot();
        assert_eq!(snap.stalls, 0);
        assert_eq!(snap.stall_us, 0);
        assert_eq!(snap.avg_stall_us, 0);
        assert_eq!(snap.idles, 0);
        assert_eq!(snap.idle_us, 0);
        assert_eq!(snap.avg_idle_us, 0);
    }

    #[test]
    fn record_stall_and_record_idle_accumulate_independently() {
        let metrics = PipeMetrics::default();
        metrics.record_stall(100);
        metrics.record_stall(50);
        metrics.record_idle(9);

        let snap = metrics.snapshot();
        assert_eq!(snap.stalls, 2);
        assert_eq!(snap.stall_us, 150);
        assert_eq!(snap.avg_stall_us, 75);
        assert_eq!(snap.idles, 1);
        assert_eq!(snap.idle_us, 9);
        assert_eq!(snap.avg_idle_us, 9);
    }

    #[test]
    fn avg_stall_integer_division_truncates_towards_zero() {
        let metrics = PipeMetrics::default();
        metrics.record_stall(10);
        metrics.record_stall(10);
        metrics.record_stall(9);

        let snap = metrics.snapshot();
        assert_eq!(snap.stall_us, 29);
        assert_eq!(snap.avg_stall_us, 9);
    }

    #[test]
    fn counters_wrap_on_u64_overflow_without_panicking() {
        let metrics = PipeMetrics::default();
        metrics.stalls.store(u64::MAX, Ordering::Relaxed);
        metrics.stall_us.store(u64::MAX, Ordering::Relaxed);

        metrics.record_stall(10);

        let snap = metrics.snapshot();
        assert_eq!(snap.stalls, 0);
        assert_eq!(snap.stall_us, 9);
        // stalls wrapped to 0, so the checked_div guard must still catch it.
        assert_eq!(snap.avg_stall_us, 0);
    }
}
