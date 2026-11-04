//! One ring slot: its packet, publication time and frame unit.
//!
//! # Why a per-slot mutex
//!
//! A slot's lock is held only to swap or clone one `Arc`, a few nanoseconds,
//! and the replaced packet drops after it is released, so publication costs
//! the same however many threads the process runs. `ArcSwap` reads are
//! lock-free, but every `ArcSwap` store walks arc-swap's process-wide debt
//! list, one node per thread that ever used any `ArcSwap`: a per-packet cost
//! that grew with the thread count of the whole engine.
//!
//! The producer overwrites the oldest slot, so it meets a reader on a slot
//! only when that reader is nearly a full lap behind.

use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Mutex};

use restream_core::MediaPacket;

use crate::lock::lock;

pub struct RingSlot {
    data: Mutex<Option<Arc<MediaPacket>>>,
    pub(super) published_at_us: AtomicU64,
    /// Packed [`restream_core::FrameUnit`] of the packet in `data`; 0 before
    /// the first write.
    pub(super) unit: AtomicU64,
}

impl RingSlot {
    pub(super) fn new() -> Self {
        Self {
            data: Mutex::new(None),
            published_at_us: AtomicU64::new(0),
            unit: AtomicU64::new(0),
        }
    }

    /// Store `packet`. The lock covers only the pointer swap: the packet it
    /// replaces drops after the lock is released.
    #[inline]
    pub(super) fn replace(&self, packet: Arc<MediaPacket>) {
        let replaced = lock(&self.data).replace(packet);
        drop(replaced);
    }

    /// The packet in this slot, whatever sequence it belongs to.
    #[inline]
    pub(super) fn packet(&self) -> Option<Arc<MediaPacket>> {
        lock(&self.data).clone()
    }

    /// Apply `inspect` to the packet in this slot without taking a reference.
    pub(super) fn inspect<R>(&self, inspect: impl FnOnce(&MediaPacket) -> R) -> Option<R> {
        lock(&self.data).as_deref().map(inspect)
    }
}
