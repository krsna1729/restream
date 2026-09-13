//! Fixed media storage and cursor-driven retention.
//!
//! The owner thread allocates media slots once, publishes references into a
//! bounded ring, and recycles storage only after every retained reference is
//! released. A slow cursor can overrun the ring; it never extends retention.

use std::time::{Duration, Instant};

/// A copyable reference into a [`MediaArena`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MediaRef {
    pub slot: u32,
    pub generation: u32,
    pub len: u32,
}

#[derive(Debug)]
struct ArenaSlot {
    bytes: Box<[u8]>,
    generation: u32,
    refs: u32,
    live: bool,
}

/// Fixed-size media storage. All allocation happens in `new`; hot-path
/// acquire/write/retain/release operations only mutate preallocated state.
#[derive(Debug)]
pub struct MediaArena {
    slots: Box<[ArenaSlot]>,
    free: Vec<u32>,
    slot_size: usize,
}

impl MediaArena {
    pub fn new(slot_count: usize, slot_size: usize) -> Result<Self, MediaError> {
        if slot_count == 0 || slot_size == 0 {
            return Err(MediaError::InvalidCapacity {
                slots: slot_count,
                slot_size,
            });
        }
        let mut slots = Vec::with_capacity(slot_count);
        let mut free = Vec::with_capacity(slot_count);
        for slot in (0..slot_count as u32).rev() {
            slots.push(ArenaSlot {
                bytes: vec![0; slot_size].into_boxed_slice(),
                generation: 0,
                refs: 0,
                live: false,
            });
            free.push(slot);
        }
        Ok(Self {
            slots: slots.into_boxed_slice(),
            free,
            slot_size,
        })
    }

    pub fn slot_size(&self) -> usize {
        self.slot_size
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    pub fn available(&self) -> usize {
        self.free.len()
    }

    /// Reserves one slot. The returned reference owns the initial arena ref.
    pub fn acquire(&mut self, len: usize) -> Option<MediaRef> {
        if len > self.slot_size {
            return None;
        }
        let slot = self.free.pop()?;
        let entry = &mut self.slots[slot as usize];
        entry.live = true;
        entry.refs = 1;
        Some(MediaRef {
            slot,
            generation: entry.generation,
            len: len as u32,
        })
    }

    /// Acquires and copies one payload into a preallocated slot.
    pub fn acquire_copy(&mut self, payload: &[u8]) -> Option<MediaRef> {
        let reference = self.acquire(payload.len())?;
        if !self.write(reference, payload) {
            let _ = self.release(reference);
            return None;
        }
        Some(reference)
    }

    pub fn write(&mut self, reference: MediaRef, payload: &[u8]) -> bool {
        if payload.len() > reference.len as usize {
            return false;
        }
        let Some(slot) = self.valid_slot_mut(reference) else {
            return false;
        };
        slot.bytes[..payload.len()].copy_from_slice(payload);
        true
    }

    pub fn payload(&self, reference: MediaRef) -> Option<&[u8]> {
        let slot = self.valid_slot(reference)?;
        Some(&slot.bytes[..reference.len as usize])
    }

    pub fn retain(&mut self, reference: MediaRef) -> bool {
        let Some(slot) = self.valid_slot_mut(reference) else {
            return false;
        };
        slot.refs = slot.refs.saturating_add(1);
        true
    }

    /// Releases one reference and recycles the slot at zero references.
    pub fn release(&mut self, reference: MediaRef) -> bool {
        let slot_index = reference.slot as usize;
        let Some(slot) = self.slots.get_mut(slot_index) else {
            return false;
        };
        if !slot.live || slot.generation != reference.generation || slot.refs == 0 {
            return false;
        }
        slot.refs -= 1;
        if slot.refs == 0 {
            slot.live = false;
            slot.generation = slot.generation.wrapping_add(1);
            self.free.push(reference.slot);
        }
        true
    }

    pub fn refs(&self, reference: MediaRef) -> Option<u32> {
        let slot = self.valid_slot(reference)?;
        Some(slot.refs)
    }

    fn valid_slot(&self, reference: MediaRef) -> Option<&ArenaSlot> {
        self.slots
            .get(reference.slot as usize)
            .filter(|slot| slot.live && slot.generation == reference.generation)
    }

    fn valid_slot_mut(&mut self, reference: MediaRef) -> Option<&mut ArenaSlot> {
        self.slots
            .get_mut(reference.slot as usize)
            .filter(|slot| slot.live && slot.generation == reference.generation)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RingEntry {
    reference: MediaRef,
    sequence: u64,
    at: Instant,
    keyframe: bool,
}

/// A bounded, single-owner media history with independent consumer cursors.
#[derive(Debug)]
pub struct MediaRing {
    arena: MediaArena,
    entries: Box<[Option<RingEntry>]>,
    head_index: usize,
    len: usize,
    next_sequence: u64,
    bytes: usize,
    max_bytes: usize,
    max_age: Duration,
    epoch: u32,
}

impl MediaRing {
    pub fn new(
        arena: MediaArena,
        capacity: usize,
        max_bytes: usize,
        max_age: Duration,
    ) -> Result<Self, MediaError> {
        if capacity == 0 || max_bytes == 0 {
            return Err(MediaError::InvalidRing {
                capacity,
                max_bytes,
            });
        }
        Ok(Self {
            arena,
            entries: vec![None; capacity].into_boxed_slice(),
            head_index: 0,
            len: 0,
            next_sequence: 0,
            bytes: 0,
            max_bytes,
            max_age,
            epoch: 0,
        })
    }

    pub fn arena(&self) -> &MediaArena {
        &self.arena
    }

    pub fn arena_mut(&mut self) -> &mut MediaArena {
        &mut self.arena
    }

    pub fn capacity(&self) -> usize {
        self.entries.len()
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn bytes(&self) -> usize {
        self.bytes
    }

    pub fn oldest_sequence(&self) -> u64 {
        self.next_sequence.saturating_sub(self.len as u64)
    }

    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub fn epoch(&self) -> u32 {
        self.epoch
    }

    pub fn advance_epoch(&mut self) -> u32 {
        self.epoch = self.epoch.wrapping_add(1);
        self.epoch
    }

    /// Publishes `reference`; the ring takes ownership of its initial ref.
    pub fn push(
        &mut self,
        reference: MediaRef,
        at: Instant,
        keyframe: bool,
    ) -> Result<u64, MediaError> {
        let len = reference.len as usize;
        if self.arena.refs(reference).is_none() {
            return Err(MediaError::StaleReference);
        }
        if len > self.max_bytes {
            return Err(MediaError::PayloadTooLarge {
                len,
                max_bytes: self.max_bytes,
            });
        }
        self.evict_expired(at);
        while self.len == self.capacity() || self.bytes + len > self.max_bytes {
            self.evict_oldest();
        }
        let sequence = self.next_sequence;
        let index = (self.head_index + self.len) % self.capacity();
        self.entries[index] = Some(RingEntry {
            reference,
            sequence,
            at,
            keyframe,
        });
        self.len += 1;
        self.next_sequence = self.next_sequence.wrapping_add(1);
        self.bytes += len;
        Ok(sequence)
    }

    pub fn evict_expired(&mut self, now: Instant) -> usize {
        let mut evicted = 0;
        while self
            .oldest_entry()
            .is_some_and(|entry| now.saturating_duration_since(entry.at) > self.max_age)
        {
            self.evict_oldest();
            evicted += 1;
        }
        evicted
    }

    pub fn read(&self, sequence: u64) -> Result<MediaRef, CursorError> {
        if sequence < self.oldest_sequence() {
            return Err(CursorError::Overrun {
                oldest_sequence: self.oldest_sequence(),
            });
        }
        if sequence >= self.next_sequence {
            return Err(CursorError::Empty);
        }
        let index =
            (self.head_index + (sequence - self.oldest_sequence()) as usize) % self.capacity();
        self.entries[index]
            .filter(|entry| entry.sequence == sequence)
            .map(|entry| entry.reference)
            .ok_or(CursorError::Overrun {
                oldest_sequence: self.oldest_sequence(),
            })
    }

    pub fn read_cursor(&self, cursor: FeedCursor) -> Result<MediaRef, CursorError> {
        if cursor.epoch != self.epoch {
            return Err(CursorError::EpochMismatch {
                current: self.epoch,
            });
        }
        self.read(cursor.sequence)
    }

    pub fn latest_sync_point(&self) -> Option<FeedCursor> {
        (0..self.len).rev().find_map(|offset| {
            let index = (self.head_index + offset) % self.capacity();
            self.entries[index]
                .filter(|entry| entry.keyframe)
                .map(|entry| FeedCursor {
                    epoch: self.epoch,
                    sequence: entry.sequence,
                })
        })
    }

    pub fn retain(&mut self, reference: MediaRef) -> bool {
        self.arena.retain(reference)
    }

    pub fn release(&mut self, reference: MediaRef) -> bool {
        self.arena.release(reference)
    }

    fn oldest_entry(&self) -> Option<RingEntry> {
        self.entries[self.head_index]
    }

    fn evict_oldest(&mut self) {
        let Some(entry) = self.entries[self.head_index].take() else {
            return;
        };
        self.bytes = self.bytes.saturating_sub(entry.reference.len as usize);
        self.len -= 1;
        self.head_index = (self.head_index + 1) % self.capacity();
        let _ = self.arena.release(entry.reference);
    }
}

impl Drop for MediaRing {
    fn drop(&mut self) {
        while self.len != 0 {
            self.evict_oldest();
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeedCursor {
    pub epoch: u32,
    pub sequence: u64,
}

impl FeedCursor {
    pub const fn new(epoch: u32, sequence: u64) -> Self {
        Self { epoch, sequence }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorError {
    Empty,
    Overrun { oldest_sequence: u64 },
    EpochMismatch { current: u32 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaError {
    InvalidCapacity { slots: usize, slot_size: usize },
    InvalidRing { capacity: usize, max_bytes: usize },
    PayloadTooLarge { len: usize, max_bytes: usize },
    StaleReference,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring() -> MediaRing {
        MediaRing::new(
            MediaArena::new(3, 16).unwrap(),
            2,
            16,
            Duration::from_secs(1),
        )
        .unwrap()
    }

    #[test]
    fn stale_generation_cannot_reuse_a_slot() {
        let mut arena = MediaArena::new(1, 8).unwrap();
        let first = arena.acquire_copy(b"first").unwrap();
        assert!(arena.release(first));
        let second = arena.acquire_copy(b"second").unwrap();
        assert_ne!(first.generation, second.generation);
        assert!(!arena.write(first, b"bad"));
        assert_eq!(arena.payload(second), Some(&b"second"[..]));
    }

    #[test]
    fn ring_overrun_evicts_without_waiting_for_slow_cursor() {
        let mut ring = ring();
        let first = ring.arena_mut().acquire_copy(b"one").unwrap();
        let second = ring.arena_mut().acquire_copy(b"two").unwrap();
        let third = ring.arena_mut().acquire_copy(b"three").unwrap();
        assert_eq!(ring.push(first, Instant::now(), true), Ok(0));
        assert_eq!(ring.push(second, Instant::now(), false), Ok(1));
        assert_eq!(ring.push(third, Instant::now(), true), Ok(2));
        assert_eq!(
            ring.read(0),
            Err(CursorError::Overrun { oldest_sequence: 1 })
        );
        let reference = ring.read(2).unwrap();
        assert_eq!(ring.arena().payload(reference), Some(&b"three"[..]));
    }

    #[test]
    fn retained_reference_defers_recycle_until_release() {
        let mut ring = ring();
        let first = ring.arena_mut().acquire_copy(b"one").unwrap();
        assert!(ring.retain(first));
        ring.push(first, Instant::now(), false).unwrap();
        let second = ring.arena_mut().acquire_copy(b"two").unwrap();
        ring.push(second, Instant::now(), false).unwrap();
        let third = ring.arena_mut().acquire_copy(b"three").unwrap();
        ring.push(third, Instant::now(), false).unwrap();
        assert_eq!(ring.arena().refs(first), Some(1));
        assert!(ring.release(first));
        assert_eq!(ring.arena().refs(first), None);
    }

    #[test]
    fn cursor_epoch_mismatch_is_explicit() {
        let mut ring = ring();
        let reference = ring.arena_mut().acquire_copy(b"one").unwrap();
        ring.push(reference, Instant::now(), true).unwrap();
        let cursor = FeedCursor::new(ring.epoch(), 0);
        ring.advance_epoch();
        assert_eq!(
            ring.read_cursor(cursor),
            Err(CursorError::EpochMismatch { current: 1 })
        );
    }
}
