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
