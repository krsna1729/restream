//! Per-reader position and burst statistics, shared by the ring (which
//! registers and snapshots readers) and the reader (which updates them).

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

/// Compact histogram for `pull_burst` yield sizes.
///
/// Buckets cover [1], [2], [3-4], [5-8], [9-16], [17-32].
/// Burst size 0 (nothing available) is not counted — callers skip stat
/// recording when `available == 0`.
pub const BURST_HIST_BUCKETS: usize = 6;
pub(crate) const fn burst_bucket(n: usize) -> usize {
    match n {
        1 => 0,
        2 => 1,
        3..=4 => 2,
        5..=8 => 3,
        9..=16 => 4,
        _ => 5,
    }
}

pub struct ReaderInfo {
    pub name: String,
    pub read_idx: AtomicUsize,
    pub overflow_count: AtomicUsize,
    pub burst_count: AtomicU64,
    pub packet_sum: AtomicU64,
    pub burst_hist: [AtomicU64; BURST_HIST_BUCKETS],
}

#[derive(Debug, Clone)]
pub struct ReaderSnapshot {
    pub name: String,
    pub read_idx: usize,
    pub write_idx: usize,
    pub lag_slots: usize,
    pub overflow_count: usize,
    pub packet_age_ms: Option<u64>,
    pub burst_count: u64,
    pub avg_burst_size: f64,
    pub median_burst_size: usize,
}

impl ReaderInfo {
    pub(crate) fn new(name: String, read_idx: usize) -> Self {
        Self {
            name,
            read_idx: AtomicUsize::new(read_idx),
            overflow_count: AtomicUsize::new(0),
            burst_count: AtomicU64::new(0),
            packet_sum: AtomicU64::new(0),
            burst_hist: std::array::from_fn(|_| AtomicU64::new(0)),
        }
    }

    pub fn burst_stats(&self) -> (f64, usize, u64) {
        let bursts = self.burst_count.load(Ordering::Relaxed);
        let packets = self.packet_sum.load(Ordering::Relaxed);
        let average = if bursts > 0 {
            packets as f64 / bursts as f64
        } else {
            0.0
        };

        let histogram: [u64; BURST_HIST_BUCKETS] =
            std::array::from_fn(|index| self.burst_hist[index].load(Ordering::Relaxed));
        let half = bursts.div_ceil(2);
        let mut cumulative = 0u64;
        let mut median_bucket = 0usize;
        for (index, count) in histogram.into_iter().enumerate() {
            cumulative += count;
            if cumulative >= half {
                median_bucket = index;
                break;
            }
        }
        let median = match median_bucket {
            0 => 1,
            1 => 2,
            2 => 3,
            3 => 6,
            4 => 12,
            _ => 24,
        };

        (average, median, bursts)
    }
}
