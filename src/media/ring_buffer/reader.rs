use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};

use tracing::{debug, info, warn};

use super::{BURST_HIST_BUCKETS, MediaPacket, RingBuffer, burst_bucket};

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
    pub(super) fn new(name: String, read_idx: usize) -> Self {
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

pub struct Reader {
    pub(super) buffer: Arc<RingBuffer>,
    pub info: Arc<ReaderInfo>,
    pub(super) read_idx: usize,
    pub(super) migration_preroll_packets: usize,
}

impl Drop for Reader {
    fn drop(&mut self) {
        let mut readers = self
            .buffer
            .readers
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        readers.retain(|weak| match weak.upgrade() {
            Some(info) => !Arc::ptr_eq(&info, &self.info),
            None => false,
        });
        info!(
            reader = %self.info.name,
            overflows = self.info.overflow_count.load(Ordering::Relaxed),
            "ring reader deregistered"
        );
    }
}

impl Reader {
    fn register(
        name: String,
        buffer: Arc<RingBuffer>,
        start_idx: usize,
        migration_preroll_packets: usize,
    ) -> Self {
        let info = Arc::new(ReaderInfo::new(name.clone(), start_idx));

        {
            let mut readers = buffer
                .readers
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            readers.push(Arc::downgrade(&info));
        }

        Self {
            buffer,
            info,
            read_idx: start_idx,
            migration_preroll_packets,
        }
    }

    pub fn current_ring(&self) -> &Arc<RingBuffer> {
        &self.buffer
    }

    pub fn is_caught_up_to_end_of_stream(&self) -> bool {
        self.buffer.is_end_of_stream() && self.buffer.get_write_idx() == self.read_idx
    }

    pub fn new(name: String, buffer: Arc<RingBuffer>) -> Self {
        let current_write = buffer.get_write_idx();
        let start_idx = buffer.fast_forward(current_write);
        let reader = Self::register(name, buffer, start_idx, 0);
        info!(reader = %reader.info.name, start_idx, "ring reader registered");
        reader
    }

    pub fn new_with_keyframe_preroll(
        name: String,
        buffer: Arc<RingBuffer>,
        preroll_packets: usize,
    ) -> Self {
        let current_write = buffer.get_write_idx();
        let keyframe_start = buffer.fast_forward(current_write);
        let oldest_available = current_write.saturating_sub(buffer.capacity.saturating_sub(1));
        let start_idx = if keyframe_start < current_write {
            keyframe_start
                .saturating_sub(preroll_packets)
                .max(oldest_available)
        } else {
            keyframe_start
        };
        let reader = Self::register(name, buffer, start_idx, 0);
        info!(
            reader = %reader.info.name,
            start_idx,
            preroll_packets,
            "ring reader registered (keyframe preroll)"
        );
        reader
    }

    pub(crate) fn new_stage_input(
        name: String,
        buffer: Arc<RingBuffer>,
        preroll_packets: usize,
    ) -> Self {
        let current_write = buffer.get_write_idx();
        let keyframe_start = buffer.fast_forward(current_write);
        let oldest_available = current_write.saturating_sub(buffer.capacity.saturating_sub(1));
        let start_idx = if keyframe_start < current_write {
            keyframe_start
                .saturating_sub(preroll_packets)
                .max(oldest_available)
        } else {
            keyframe_start
        };
        let reader = Self::register(name, buffer, start_idx, preroll_packets);
        info!(
            reader = %reader.info.name,
            start_idx,
            preroll_packets,
            "ring reader registered (stage input)"
        );
        reader
    }

    pub fn new_live(name: String, buffer: Arc<RingBuffer>) -> Self {
        let current_write = buffer.get_write_idx();
        let reader = Self::register(name, buffer, current_write, 0);
        info!(
            reader = %reader.info.name,
            start_idx = current_write,
            "ring reader registered (live edge)"
        );
        reader
    }

    pub(crate) fn sync_read_idx(&mut self, read_idx: usize) {
        self.read_idx = read_idx;
        self.info.read_idx.store(read_idx, Ordering::Relaxed);
    }

    pub fn pull(&mut self) -> Result<Option<Arc<MediaPacket>>, &'static str> {
        let write_idx = self.buffer.get_write_idx();

        if write_idx > self.read_idx && write_idx - self.read_idx >= self.buffer.capacity {
            let new_idx = self.buffer.fast_forward(write_idx);
            let lag = write_idx.saturating_sub(self.read_idx);
            self.read_idx = new_idx;
            self.info.read_idx.store(new_idx, Ordering::Relaxed);
            self.info.overflow_count.fetch_add(1, Ordering::Relaxed);
            warn!(
                reader = %self.info.name,
                lag_packets = lag,
                "ring reader overflowed — fast-forwarding to keyframe"
            );
            return Err("Overflow: reader lagged and was fast-forwarded");
        }

        if self.read_idx == write_idx {
            return Ok(None);
        }

        let packet = self.buffer.read_at(self.read_idx);
        let post_write_idx = self.buffer.get_write_idx();
        if post_write_idx > self.read_idx && post_write_idx - self.read_idx >= self.buffer.capacity
        {
            let new_idx = self.buffer.fast_forward(post_write_idx);
            let lag = post_write_idx.saturating_sub(self.read_idx);
            self.read_idx = new_idx;
            self.info.read_idx.store(new_idx, Ordering::Relaxed);
            self.info.overflow_count.fetch_add(1, Ordering::Relaxed);
            warn!(
                reader = %self.info.name,
                lag_packets = lag,
                "ring reader overflowed mid-read — fast-forwarding to keyframe"
            );
            return Err("Overflow: reader lagged and was fast-forwarded");
        }

        if packet.is_some() {
            self.read_idx += 1;
            self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
        }
        Ok(packet)
    }

    pub fn pull_burst(
        &mut self,
        output: &mut Vec<Arc<MediaPacket>>,
        max_packets: usize,
    ) -> Result<usize, &'static str> {
        if max_packets == 0 {
            return Ok(0);
        }

        let write_idx = self.buffer.get_write_idx();
        if write_idx > self.read_idx && write_idx - self.read_idx >= self.buffer.capacity {
            self.read_idx = self.buffer.fast_forward(write_idx);
            self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
            self.info.overflow_count.fetch_add(1, Ordering::Relaxed);
            return Err("Overflow: reader lagged and was fast-forwarded");
        }

        let available = write_idx.saturating_sub(self.read_idx).min(max_packets);
        output.reserve(available);
        let start_len = output.len();

        for index in self.read_idx..self.read_idx + available {
            let Some(packet) = self.buffer.read_at(index) else {
                break;
            };
            output.push(packet);
        }

        let post_write_idx = self.buffer.get_write_idx();
        if post_write_idx > self.read_idx && post_write_idx - self.read_idx >= self.buffer.capacity
        {
            output.truncate(start_len);
            self.read_idx = self.buffer.fast_forward(post_write_idx);
            self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
            self.info.overflow_count.fetch_add(1, Ordering::Relaxed);
            return Err("Overflow: reader lagged and was fast-forwarded");
        }

        let loaded = output.len() - start_len;
        self.read_idx += loaded;
        self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
        if loaded > 0 {
            self.info.burst_count.fetch_add(1, Ordering::Relaxed);
            self.info
                .packet_sum
                .fetch_add(loaded as u64, Ordering::Relaxed);
            self.info.burst_hist[burst_bucket(loaded)].fetch_add(1, Ordering::Relaxed);
        }
        Ok(loaded)
    }

    pub async fn wait_for_data(&mut self) {
        loop {
            let notify = self.buffer.get_notify();
            if self.buffer.get_write_idx() > self.read_idx {
                return;
            }
            if let Some(next) = self.buffer.next.load_full() {
                self.migrate_to(next);
                continue;
            }
            let notified = notify.notified();
            if self.buffer.get_write_idx() > self.read_idx {
                return;
            }
            if self.buffer.next.load().is_some() {
                continue;
            }
            if self.buffer.is_end_of_stream() {
                return;
            }
            notified.await;
        }
    }

    fn migrate_to(&mut self, new_ring: Arc<RingBuffer>) {
        let old_read_idx = self.read_idx;
        let old_ring = std::mem::replace(&mut self.buffer, new_ring.clone());
        if let Ok(mut readers) = new_ring.readers.lock() {
            readers.push(Arc::downgrade(&self.info));
        }
        let new_write_idx = new_ring.get_write_idx();
        if self.migration_preroll_packets > 0 && old_read_idx == new_write_idx {
            let keyframe_start = new_ring.fast_forward(new_write_idx);
            if keyframe_start < new_write_idx {
                let oldest_available =
                    new_write_idx.saturating_sub(new_ring.capacity.saturating_sub(1));
                self.read_idx = keyframe_start
                    .saturating_sub(self.migration_preroll_packets)
                    .max(oldest_available);
                self.info.read_idx.store(self.read_idx, Ordering::Relaxed);
            }
        }
        if let Ok(mut readers) = old_ring.readers.lock() {
            readers.retain(|weak| weak.upgrade().is_some());
        }
        debug!(
            read_idx = self.read_idx,
            name = %self.info.name,
            "reader migrated to resized ring"
        );
    }

    pub fn lag(&self) -> usize {
        self.buffer.get_write_idx().saturating_sub(self.read_idx)
    }
}
