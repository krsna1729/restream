//! `MemoryQueue`: the bounded byte queue between a media producer and a
//! blocking consumer (an external `ffmpeg` process's stdin writer, the
//! recording writer). Data flows through a `VecDeque<u8>` protected by
//! `Mutex` + `Condvar`, with bulk `as_slices()` + `drain()` reads instead of
//! per-byte `pop_front()`.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::debug;

pub const DEFAULT_AVIO_QUEUE_CAPACITY: usize = 512 * 1024;

// 512 KB absorbs bursts between the async TS reader and the blocking SRT/RTMP
// sender thread. Scale-test evidence: max per-queue HWM = 398 KB at 8 Mb/s
// across all config×bitrate combinations, 0 blocked_writes throughout.
// Operators running very high latency links (> 1 s) can raise this with
// RESTREAM_AVIO_QUEUE_CAPACITY (bytes).

fn default_avio_queue_capacity() -> usize {
    DEFAULT_AVIO_QUEUE_CAPACITY
}

/// Produces a queue's input on the reading thread, on demand (WI11): the
/// external `ffmpeg` stdin-writer thread calls it when the queue runs dry, so
/// a stage's input is pulled from its source ring on that thread with no
/// producer task or thread in between.
pub trait QueueRefill: Send {
    /// Block until more input is available and append it to `out` (which is
    /// empty on entry). Return `false` at end of input or on cancellation;
    /// the queue then closes.
    fn refill(&mut self, out: &mut Vec<u8>) -> bool;
}

struct RefillState {
    source: Box<dyn QueueRefill>,
    pending: Vec<u8>,
    offset: usize,
}

pub struct MemoryQueue {
    inner: Mutex<MemoryQueueInner>,
    /// Set by [`MemoryQueue::set_refill`]; only the reading thread touches it.
    refill: Mutex<Option<RefillState>>,
    /// Lets plain queues skip the `refill` lock on every read.
    has_refill: AtomicBool,
    /// Bytes of the current refilled batch not yet read. Kept outside the
    /// `refill` lock, which the reading thread holds while it waits for
    /// input, so `len`/`stats` never block on it.
    refill_pending: AtomicUsize,
    cvar: Condvar,
    space_available: Notify,
    capacity: usize,
    high_water_bytes: AtomicUsize,
    blocked_writes: AtomicU64,
    blocked_write_us: AtomicU64,
}

struct MemoryQueueInner {
    buf: VecDeque<u8>,
    closed: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemoryQueueStats {
    pub len: usize,
    pub capacity: usize,
    pub high_water_bytes: usize,
    pub blocked_writes: u64,
    pub blocked_write_us: u64,
    pub closed: bool,
}

impl Default for MemoryQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryQueue {
    pub fn new() -> Self {
        Self::new_with_capacity(default_avio_queue_capacity())
    }

    pub fn new_with_capacity(capacity: usize) -> Self {
        debug!(capacity_bytes = capacity, "memory queue created");
        Self {
            inner: Mutex::new(MemoryQueueInner {
                buf: VecDeque::new(),
                closed: false,
            }),
            refill: Mutex::new(None),
            has_refill: AtomicBool::new(false),
            refill_pending: AtomicUsize::new(0),
            cvar: Condvar::new(),
            space_available: Notify::new(),
            capacity,
            high_water_bytes: AtomicUsize::new(0),
            blocked_writes: AtomicU64::new(0),
            blocked_write_us: AtomicU64::new(0),
        }
    }

    pub async fn write(&self, data: &[u8]) {
        let mut blocked_since = None;
        loop {
            // Arm the `Notified` future *before* checking capacity: it snapshots
            // the notify generation on creation, so a `notify_waiters()` racing
            // in right after we drop the lock below is still observed. Creating
            // it only after the check (the previous bug) left a gap where a
            // reader could drain and notify before we started waiting, losing
            // the wakeup and hanging the writer forever.
            let notified = self.space_available.notified();
            {
                let inner = crate::sync::lock(&self.inner);
                if inner.closed || inner.buf.len() < self.capacity {
                    break;
                }
            }
            if blocked_since.is_none() {
                blocked_since = Some(Instant::now());
                self.blocked_writes.fetch_add(1, Ordering::Relaxed);
            }
            notified.await;
        }
        if let Some(start) = blocked_since {
            self.blocked_write_us
                .fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
        }

        let mut inner = crate::sync::lock(&self.inner);
        if inner.closed {
            return;
        }
        inner.buf.extend(data.iter().copied());
        self.record_depth(inner.buf.len());
        self.cvar.notify_all();
    }

    pub async fn write_cancellable(&self, data: &[u8], cancel: &CancellationToken) -> bool {
        let mut blocked_since = None;
        loop {
            // See `write()` for why this future must be armed before the
            // capacity check — it closes a lost-wakeup race against a
            // concurrent `notify_waiters()`.
            let notified = self.space_available.notified();
            {
                let inner = crate::sync::lock(&self.inner);
                if inner.closed || inner.buf.len() < self.capacity {
                    break;
                }
            }
            if blocked_since.is_none() {
                blocked_since = Some(Instant::now());
                self.blocked_writes.fetch_add(1, Ordering::Relaxed);
            }
            tokio::select! {
                _ = notified => {}
                _ = cancel.cancelled() => {
                    if let Some(start) = blocked_since {
                        self.blocked_write_us
                            .fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
                    }
                    return false;
                }
            }
        }
        if let Some(start) = blocked_since {
            self.blocked_write_us
                .fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
        }

        let mut inner = crate::sync::lock(&self.inner);
        if inner.closed || cancel.is_cancelled() {
            return false;
        }
        inner.buf.extend(data.iter().copied());
        self.record_depth(inner.buf.len());
        self.cvar.notify_all();
        true
    }

    /// Append multiple chunks while taking the queue lock and notifying once.
    pub async fn write_batch<'a, I>(&self, chunks: I) -> usize
    where
        I: IntoIterator<Item = &'a [u8]> + Clone,
    {
        let total_bytes: usize = chunks.clone().into_iter().map(|c| c.len()).sum();
        if total_bytes == 0 {
            return 0;
        }

        let mut blocked_since = None;
        loop {
            // See `write()` for why this future must be armed before the
            // capacity check — it closes a lost-wakeup race against a
            // concurrent `notify_waiters()`.
            let notified = self.space_available.notified();
            {
                let inner = crate::sync::lock(&self.inner);
                if inner.closed || inner.buf.len() < self.capacity {
                    break;
                }
            }
            if blocked_since.is_none() {
                blocked_since = Some(Instant::now());
                self.blocked_writes.fetch_add(1, Ordering::Relaxed);
            }
            notified.await;
        }
        if let Some(start) = blocked_since {
            self.blocked_write_us
                .fetch_add(start.elapsed().as_micros() as u64, Ordering::Relaxed);
        }

        let mut inner = crate::sync::lock(&self.inner);
        if inner.closed {
            return 0;
        }
        let mut bytes = 0usize;
        for chunk in chunks {
            bytes += chunk.len();
            inner.buf.extend(chunk.iter().copied());
        }
        if bytes > 0 {
            self.record_depth(inner.buf.len());
            self.cvar.notify_all();
        }
        bytes
    }

    pub fn write_sync(&self, data: &[u8]) {
        let mut inner = crate::sync::lock(&self.inner);
        if inner.closed {
            return;
        }
        inner.buf.extend(data.iter().copied());
        self.record_depth(inner.buf.len());
        self.cvar.notify_all();
    }

    pub fn close(&self) {
        let mut inner = crate::sync::lock(&self.inner);
        inner.closed = true;
        self.cvar.notify_all();
        self.space_available.notify_waiters();
        let high = self.high_water_bytes.load(Ordering::Relaxed);
        let blocked = self.blocked_writes.load(Ordering::Relaxed);
        debug!(
            high_water_bytes = high,
            blocked_writes = blocked,
            "memory queue closed"
        );
    }

    pub fn is_closed(&self) -> bool {
        crate::sync::lock(&self.inner).closed
    }

    /// Current number of buffered bytes awaiting consumption.
    ///
    /// Useful for detecting producer/consumer imbalance (e.g., a slow FFmpeg
    /// thread unable to keep pace with ingest). Values consistently above a few
    /// megabytes indicate the downstream stage is falling behind.
    pub fn len(&self) -> usize {
        crate::sync::lock(&self.inner).buf.len() + self.refill_pending.load(Ordering::Relaxed)
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    pub fn stats(&self) -> MemoryQueueStats {
        let inner = crate::sync::lock(&self.inner);
        MemoryQueueStats {
            len: inner.buf.len() + self.refill_pending.load(Ordering::Relaxed),
            capacity: self.capacity,
            high_water_bytes: self.high_water_bytes.load(Ordering::Relaxed),
            blocked_writes: self.blocked_writes.load(Ordering::Relaxed),
            blocked_write_us: self.blocked_write_us.load(Ordering::Relaxed),
            closed: inner.closed,
        }
    }

    fn record_depth(&self, len: usize) {
        self.high_water_bytes.fetch_max(len, Ordering::Relaxed);
    }

    /// Make this queue pull its input from `source` on the reading thread
    /// instead of waiting for writers. Reads then serve refilled batches
    /// directly, without staging them in the queue buffer.
    pub fn set_refill(&self, source: Box<dyn QueueRefill>) {
        *crate::sync::lock(&self.refill) = Some(RefillState {
            source,
            pending: Vec::with_capacity(crate::media::MEDIA_TS_BATCH_TARGET_BYTES),
            offset: 0,
        });
        self.has_refill.store(true, Ordering::Release);
    }

    fn read_refilled(&self, state: &mut RefillState, target: &mut [u8]) -> usize {
        loop {
            let available = state.pending.len() - state.offset;
            if available > 0 {
                let to_read = available.min(target.len());
                target[..to_read]
                    .copy_from_slice(&state.pending[state.offset..state.offset + to_read]);
                state.offset += to_read;
                self.refill_pending
                    .store(state.pending.len() - state.offset, Ordering::Relaxed);
                return to_read;
            }
            state.pending.clear();
            state.offset = 0;
            if self.is_closed() || !state.source.refill(&mut state.pending) {
                self.close();
                return 0;
            }
            self.record_depth(state.pending.len());
            self.refill_pending
                .store(state.pending.len(), Ordering::Relaxed);
        }
    }

    pub fn read(&self, target: &mut [u8]) -> usize {
        if self.has_refill.load(Ordering::Acquire)
            && let Some(state) = crate::sync::lock(&self.refill).as_mut()
        {
            return self.read_refilled(state, target);
        }
        let mut inner = crate::sync::lock(&self.inner);
        while inner.buf.is_empty() && !inner.closed {
            // Use wait_timeout so the reading thread is not blocked indefinitely
            // if the producer panics without calling close().  Poison recovery via
            // unwrap_or_else ensures we don't panic on a poisoned mutex here.
            inner = self
                .cvar
                .wait_timeout(inner, Duration::from_secs(5))
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        if inner.buf.is_empty() && inner.closed {
            return 0;
        }
        let to_read = std::cmp::min(target.len(), inner.buf.len());
        let (front, back) = inner.buf.as_slices();
        if to_read <= front.len() {
            target[..to_read].copy_from_slice(&front[..to_read]);
        } else {
            target[..front.len()].copy_from_slice(front);
            target[front.len()..to_read].copy_from_slice(&back[..to_read - front.len()]);
        }
        inner.buf.drain(..to_read);

        if inner.buf.len() < self.capacity {
            self.space_available.notify_waiters();
        }

        to_read
    }

    pub fn read_nonblocking(&self, target: &mut [u8]) -> usize {
        let mut inner = crate::sync::lock(&self.inner);
        if inner.buf.is_empty() {
            return 0;
        }
        let to_read = std::cmp::min(target.len(), inner.buf.len());
        let (front, back) = inner.buf.as_slices();
        if to_read <= front.len() {
            target[..to_read].copy_from_slice(&front[..to_read]);
        } else {
            target[..front.len()].copy_from_slice(front);
            target[front.len()..to_read].copy_from_slice(&back[..to_read - front.len()]);
        }
        inner.buf.drain(..to_read);

        if inner.buf.len() < self.capacity {
            self.space_available.notify_waiters();
        }

        to_read
    }
}

#[cfg(test)]
#[path = "avio_tests.rs"]
mod tests;
