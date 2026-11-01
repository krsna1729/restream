//! Reading a slot by sequence, and knowing it was not lapped.
//!
//! Publication stores a slot's unit word, tagged with its sequence, before
//! the packet, so [`RingBuffer::read_at`] rejects a slot a later publication
//! wrote over, even a batch that has not yet moved the write index.
//!
//! # Lapped runs
//!
//! Before it writes a slot, publication raises `write_begin` (one past the
//! sequence it is writing); the slot's Release stores that follow publish
//! it. A reader of a run of slots checks it once afterwards
//! ([`RingBuffer::run_intact`]): when no write had begun a lap over the
//! run's first sequence, every packet it loaded is its own sequence's, with
//! no per-slot tag check. Within a lap of the writer it checks each slot's
//! tag as [`RingBuffer::read_at`] does. `tests/ring_frame_unit_loom.rs`
//! models both.

use std::sync::Arc;
use std::sync::atomic::{Ordering, fence};

use restream_core::{FrameUnit, MediaPacket};

use super::RingBuffer;

impl RingBuffer {
    /// The packet published at `sequence`, or `None` when its slot holds no
    /// packet or another sequence's: one a later publication wrote over it,
    /// even a batch that has not yet stored its write index. The caller
    /// bounds `sequence` by the write index.
    ///
    /// Reading the packet first and the unit word second sees a newer
    /// packet's word whenever it got the newer packet.
    #[inline]
    pub fn read_at(&self, sequence: usize) -> Option<Arc<MediaPacket>> {
        let slot = &self.slots[sequence % self.capacity];
        let packet = slot.data.load_full()?;
        FrameUnit::unpack(slot.unit.load(Ordering::Acquire), sequence)?;
        Some(packet)
    }

    /// The packet in `sequence`'s slot, unchecked: it may be a later lap's.
    /// The caller bounds the run by the write index and then accepts it only
    /// if [`Self::run_intact`] holds for it.
    #[inline]
    pub fn read_unverified(&self, sequence: usize) -> Option<Arc<MediaPacket>> {
        self.slots[sequence % self.capacity].data.load_full()
    }

    /// Whether every packet [`Self::read_unverified`] returned for
    /// `start..end` (each below a write index loaded before the reads) is
    /// its own sequence's. One check while no publication has begun a lap
    /// over `start`; otherwise each slot's unit tag, as [`Self::read_at`]
    /// checks it. The fence orders the packet loads before both.
    pub fn run_intact(&self, start: usize, end: usize) -> bool {
        fence(Ordering::Acquire);
        if self.write_begin.val.load(Ordering::Relaxed) <= start.saturating_add(self.capacity) {
            return true;
        }
        (start..end).all(|sequence| {
            FrameUnit::unpack(
                self.slots[sequence % self.capacity]
                    .unit
                    .load(Ordering::Acquire),
                sequence,
            )
            .is_some()
        })
    }

    /// Record that the producer is about to write slots below `end`. The
    /// slot's Release stores that follow (its unit word, then the packet's
    /// swap) publish it, so a reader that loads any packet written after this
    /// sees `end` in [`Self::run_intact`].
    #[inline]
    pub(super) fn begin_write(&self, end: usize) {
        self.write_begin.val.store(end, Ordering::Relaxed);
    }
}
