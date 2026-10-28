//! `MemoryQueue`: the bounded byte queue between a media producer and a
//! blocking consumer (the recording writer). Data flows through a
//! `VecDeque<u8>` protected by `Mutex` + `Condvar`, with bulk `as_slices()` +
//! `drain()` reads instead of per-byte `pop_front()`.

#![forbid(unsafe_code)]

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Condvar, Mutex};
use std::time::Duration;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::lock::lock;

pub const DEFAULT_RECORDING_QUEUE_CAPACITY: usize = 512 * 1024;

// 512 KB is the pre-cutover default: it absorbed bursts between the async TS
// reader and the blocking SRT/RTMP sender threads that used this queue then
// (scale-test evidence: max per-queue HWM = 398 KB at 8 Mb/s across all
// config×bitrate combinations, 0 blocked_writes throughout). Recording is the
// only user left; operators can raise it with RESTREAM_RECORDING_QUEUE_CAPACITY.

fn default_recording_queue_capacity() -> usize {
    DEFAULT_RECORDING_QUEUE_CAPACITY
}

pub struct MemoryQueue {
    inner: Mutex<MemoryQueueInner>,
    cvar: Condvar,
    space_available: Notify,
    capacity: usize,
    high_water_bytes: AtomicUsize,
    blocked_writes: AtomicU64,
}

struct MemoryQueueInner {
    buf: VecDeque<u8>,
    closed: bool,
}

impl Default for MemoryQueue {
    fn default() -> Self {
        Self::new()
    }
}

impl MemoryQueue {
    pub fn new() -> Self {
        Self::new_with_capacity(default_recording_queue_capacity())
    }

    pub fn new_with_capacity(capacity: usize) -> Self {
        debug!(target: "restream::media::byte_queue", capacity_bytes = capacity, "memory queue created");
        Self {
            inner: Mutex::new(MemoryQueueInner {
                buf: VecDeque::new(),
                closed: false,
            }),
            cvar: Condvar::new(),
            space_available: Notify::new(),
            capacity,
            high_water_bytes: AtomicUsize::new(0),
            blocked_writes: AtomicU64::new(0),
        }
    }

    pub async fn write(&self, data: &[u8]) {
        let mut counted_block = false;
        loop {
            // Arm the `Notified` future *before* checking capacity: it snapshots
            // the notify generation on creation, so a `notify_waiters()` racing
            // in right after we drop the lock below is still observed. Creating
            // it only after the check (the previous bug) left a gap where a
            // reader could drain and notify before we started waiting, losing
            // the wakeup and hanging the writer forever.
            let notified = self.space_available.notified();
            {
                let inner = lock(&self.inner);
                if inner.closed || inner.buf.len() < self.capacity {
                    break;
                }
            }
            if !counted_block {
                counted_block = true;
                self.blocked_writes.fetch_add(1, Ordering::Relaxed);
            }
            notified.await;
        }
        let mut inner = lock(&self.inner);
        if inner.closed {
            return;
        }
        inner.buf.extend(data.iter().copied());
        self.record_depth(inner.buf.len());
        self.cvar.notify_all();
    }

    pub async fn write_cancellable(&self, data: &[u8], cancel: &CancellationToken) -> bool {
        let mut counted_block = false;
        loop {
            // See `write()` for why this future must be armed before the
            // capacity check — it closes a lost-wakeup race against a
            // concurrent `notify_waiters()`.
            let notified = self.space_available.notified();
            {
                let inner = lock(&self.inner);
                if inner.closed || inner.buf.len() < self.capacity {
                    break;
                }
            }
            if !counted_block {
                counted_block = true;
                self.blocked_writes.fetch_add(1, Ordering::Relaxed);
            }
            tokio::select! {
                _ = notified => {}
                _ = cancel.cancelled() => {
                    return false;
                }
            }
        }
        let mut inner = lock(&self.inner);
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

        let mut counted_block = false;
        loop {
            // See `write()` for why this future must be armed before the
            // capacity check — it closes a lost-wakeup race against a
            // concurrent `notify_waiters()`.
            let notified = self.space_available.notified();
            {
                let inner = lock(&self.inner);
                if inner.closed || inner.buf.len() < self.capacity {
                    break;
                }
            }
            if !counted_block {
                counted_block = true;
                self.blocked_writes.fetch_add(1, Ordering::Relaxed);
            }
            notified.await;
        }
        let mut inner = lock(&self.inner);
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
        let mut inner = lock(&self.inner);
        if inner.closed {
            return;
        }
        inner.buf.extend(data.iter().copied());
        self.record_depth(inner.buf.len());
        self.cvar.notify_all();
    }

    pub fn close(&self) {
        let mut inner = lock(&self.inner);
        inner.closed = true;
        self.cvar.notify_all();
        self.space_available.notify_waiters();
        let high = self.high_water_bytes.load(Ordering::Relaxed);
        let blocked = self.blocked_writes.load(Ordering::Relaxed);
        debug!(
            target: "restream::media::byte_queue",
            high_water_bytes = high,
            blocked_writes = blocked,
            "memory queue closed"
        );
    }

    pub fn is_closed(&self) -> bool {
        lock(&self.inner).closed
    }

    /// Current number of buffered bytes awaiting consumption.
    ///
    /// Useful for detecting producer/consumer imbalance (e.g., a slow FFmpeg
    /// thread unable to keep pace with ingest). Values consistently above a few
    /// megabytes indicate the downstream stage is falling behind.
    pub fn len(&self) -> usize {
        lock(&self.inner).buf.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn record_depth(&self, len: usize) {
        self.high_water_bytes.fetch_max(len, Ordering::Relaxed);
    }

    pub fn read(&self, target: &mut [u8]) -> usize {
        let mut inner = lock(&self.inner);
        while inner.buf.is_empty() && !inner.closed {
            // Use wait_timeout so the reading thread is not blocked indefinitely
            // if the producer panics without calling close(). Poison recovery via
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
        let mut inner = lock(&self.inner);
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
// These white-box tests deliberately poison and inspect the raw mutex.
#[allow(clippy::disallowed_methods)]
#[path = "byte_queue_tests.rs"]
mod tests;
