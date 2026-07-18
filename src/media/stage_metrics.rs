//! Lock-free throughput counters for a processing stage.
//!
//! [`StageMetrics`] is updated atomically on the hot path and read by the
//! `/graph` endpoint for operator visibility. It is shared across all stage
//! types (HLS, recording, external transcoder, h264 transcoder).

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

/// Typed snapshot of a [`StageMetrics`] counter set. JSON assembly (if
/// needed) happens at the API/runtime-view edge, not here.
#[derive(Debug, Clone, Copy, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StageMetricsSnapshot {
    pub packets_in: u64,
    pub packets_out: u64,
    pub bytes_in: u64,
    pub bytes_out: u64,
    pub processing_us: u64,
    pub avg_us_per_packet: f64,
    pub uptime_secs: f64,
    pub packets_per_sec: f64,
}

#[derive(Debug)]
pub struct StageMetrics {
    pub packets_in: AtomicU64,
    pub packets_out: AtomicU64,
    pub bytes_in: AtomicU64,
    pub bytes_out: AtomicU64,
    /// Cumulative processing time in microseconds.
    pub processing_us: AtomicU64,
    pub start_instant: Instant,
}

impl Default for StageMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl StageMetrics {
    pub fn new() -> Self {
        Self {
            packets_in: AtomicU64::new(0),
            packets_out: AtomicU64::new(0),
            bytes_in: AtomicU64::new(0),
            bytes_out: AtomicU64::new(0),
            processing_us: AtomicU64::new(0),
            start_instant: Instant::now(),
        }
    }

    #[inline]
    pub fn record_in(&self, bytes: u64) {
        self.packets_in.fetch_add(1, Ordering::Relaxed);
        self.bytes_in.fetch_add(bytes, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_in_batch(&self, packets: u64, bytes: u64) {
        self.packets_in.fetch_add(packets, Ordering::Relaxed);
        self.bytes_in.fetch_add(bytes, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_out(&self, bytes: u64) {
        self.packets_out.fetch_add(1, Ordering::Relaxed);
        self.bytes_out.fetch_add(bytes, Ordering::Relaxed);
    }

    #[inline]
    pub fn record_processing(&self, us: u64) {
        self.processing_us.fetch_add(us, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> StageMetricsSnapshot {
        let pkts_in = self.packets_in.load(Ordering::Relaxed);
        let pkts_out = self.packets_out.load(Ordering::Relaxed);
        let bytes_in = self.bytes_in.load(Ordering::Relaxed);
        let bytes_out = self.bytes_out.load(Ordering::Relaxed);
        let proc_us = self.processing_us.load(Ordering::Relaxed);
        let elapsed = self.start_instant.elapsed().as_secs_f64();

        let avg_us_per_packet = if pkts_in > 0 {
            proc_us as f64 / pkts_in as f64
        } else {
            0.0
        };

        StageMetricsSnapshot {
            packets_in: pkts_in,
            packets_out: pkts_out,
            bytes_in,
            bytes_out,
            processing_us: proc_us,
            avg_us_per_packet,
            uptime_secs: elapsed,
            packets_per_sec: if elapsed > 0.0 {
                pkts_in as f64 / elapsed
            } else {
                0.0
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_snapshot_has_zeroed_counters_and_no_division_by_zero() {
        let metrics = StageMetrics::new();
        let snap = metrics.snapshot();
        assert_eq!(snap.packets_in, 0);
        assert_eq!(snap.packets_out, 0);
        assert_eq!(snap.bytes_in, 0);
        assert_eq!(snap.bytes_out, 0);
        assert_eq!(snap.processing_us, 0);
        // With zero packets recorded, avg_us_per_packet must be the guarded
        // 0.0, not NaN from a 0/0 division.
        assert_eq!(snap.avg_us_per_packet, 0.0);
        assert!(!snap.avg_us_per_packet.is_nan());
    }

    #[test]
    fn record_in_and_record_out_accumulate_independently() {
        let metrics = StageMetrics::new();
        metrics.record_in(100);
        metrics.record_in(50);
        metrics.record_out(30);

        let snap = metrics.snapshot();
        assert_eq!(snap.packets_in, 2);
        assert_eq!(snap.bytes_in, 150);
        assert_eq!(snap.packets_out, 1);
        assert_eq!(snap.bytes_out, 30);
    }

    #[test]
    fn record_in_batch_adds_packet_and_byte_counts_in_one_call() {
        let metrics = StageMetrics::new();
        metrics.record_in_batch(7, 700);
        metrics.record_in(1);

        let snap = metrics.snapshot();
        assert_eq!(snap.packets_in, 8);
        assert_eq!(snap.bytes_in, 701);
    }

    #[test]
    fn record_in_batch_with_zero_packets_but_nonzero_bytes_is_not_rejected() {
        // The API has no invariant tying packet count to byte count; a
        // zero-packet batch with bytes must still land in bytes_in without
        // incrementing packets_in.
        let metrics = StageMetrics::new();
        metrics.record_in_batch(0, 500);

        let snap = metrics.snapshot();
        assert_eq!(snap.packets_in, 0);
        assert_eq!(snap.bytes_in, 500);
        // avg_us_per_packet must still guard on packets_in == 0, not on
        // bytes_in == 0.
        assert_eq!(snap.avg_us_per_packet, 0.0);
    }

    #[test]
    fn counters_wrap_on_u64_overflow_without_panicking() {
        // Atomic fetch_add wraps unconditionally (never panics, even in
        // debug builds, unlike checked integer arithmetic). Pin that a
        // counter pushed past u64::MAX wraps to a small value instead of
        // aborting the stage.
        let metrics = StageMetrics::new();
        metrics.packets_in.store(u64::MAX, Ordering::Relaxed);
        metrics.bytes_in.store(u64::MAX, Ordering::Relaxed);

        metrics.record_in(10);

        let snap = metrics.snapshot();
        assert_eq!(snap.packets_in, 0);
        assert_eq!(snap.bytes_in, 9);
    }

    #[test]
    fn record_processing_accumulates_and_feeds_average() {
        let metrics = StageMetrics::new();
        metrics.record_in(1);
        metrics.record_in(1);
        metrics.record_processing(100);
        metrics.record_processing(50);

        let snap = metrics.snapshot();
        assert_eq!(snap.processing_us, 150);
        assert_eq!(snap.avg_us_per_packet, 75.0);
    }
}
