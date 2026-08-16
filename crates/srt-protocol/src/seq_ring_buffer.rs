//! Fixed-capacity, sequence-indexed ring buffer for in-flight SRT packets.
//!
//! Both `SenderBuffer` and `ReceiverBuffer` never hold more than
//! `flow_window` packets at once -- that is the whole point of SRT's
//! window-based flow control. This makes a fixed-capacity ring buffer,
//! sized once at construction and never reallocated, a direct fit for
//! `docs/srt-pure-rust-design.md`'s "Arena-allocated" data structure
//! category (bounded capacity, carved from the connection arena at setup,
//! fixed thereafter).
//!
//! Replaces the vendored srt-rs `BTreeMap<u32, T>` this crate originally
//! used for the same storage, which allocates a tree node per insert and
//! frees one per remove -- real per-packet heap traffic on the hot path,
//! plus pointer-chasing lookups instead of a direct array index (see
//! `crates/srt-protocol/VENDOR.md`'s perf-review local patch entry).

use crate::srt_packet::sequence_less_than;

#[derive(Debug)]
pub(crate) struct SeqRingBuffer<T> {
    slots: Vec<Option<(u32, T)>>,
    capacity: u32,
    len: u32,
}

impl<T> SeqRingBuffer<T> {
    /// `capacity` is fixed for the life of the buffer -- one allocation
    /// here, none after. Must be at least as large as the largest flow
    /// window this connection will ever enforce (see callers).
    pub(crate) fn new(capacity: u32) -> Self {
        let mut slots = Vec::with_capacity(capacity as usize);
        slots.resize_with(capacity as usize, || None);
        Self {
            slots,
            capacity,
            len: 0,
        }
    }

    fn index(&self, seq: u32) -> usize {
        (seq % self.capacity) as usize
    }

    /// Inserts, overwriting whatever (if anything) previously occupied this
    /// slot. Callers are expected to only ever have at most `capacity`
    /// live entries at once (the flow-window invariant); if that invariant
    /// is upheld, a slot is only reused after its previous occupant was
    /// already removed.
    pub(crate) fn insert(&mut self, seq: u32, value: T) {
        let idx = self.index(seq);
        if self.slots[idx].is_none() {
            self.len += 1;
        }
        self.slots[idx] = Some((seq, value));
    }

    pub(crate) fn get(&self, seq: u32) -> Option<&T> {
        match &self.slots[self.index(seq)] {
            Some((s, v)) if *s == seq => Some(v),
            _ => None,
        }
    }

    pub(crate) fn get_mut(&mut self, seq: u32) -> Option<&mut T> {
        let idx = self.index(seq);
        match &mut self.slots[idx] {
            Some((s, v)) if *s == seq => Some(v),
            _ => None,
        }
    }

    pub(crate) fn remove(&mut self, seq: u32) -> Option<T> {
        let idx = self.index(seq);
        let matches = matches!(&self.slots[idx], Some((s, _)) if *s == seq);
        if !matches {
            return None;
        }
        self.len -= 1;
        self.slots[idx].take().map(|(_, v)| v)
    }

    pub(crate) fn contains(&self, seq: u32) -> bool {
        self.get(seq).is_some()
    }

    pub(crate) fn len(&self) -> u32 {
        self.len
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub(crate) fn capacity(&self) -> u32 {
        self.capacity
    }

    /// Iterates all occupied slots in array order (not sequence order --
    /// callers that need wraparound-safe sequence order must derive it
    /// themselves, e.g. via [`Self::remove_less_than`]).
    pub(crate) fn iter(&self) -> impl Iterator<Item = (u32, &T)> {
        self.slots
            .iter()
            .filter_map(|s| s.as_ref().map(|(seq, v)| (*seq, v)))
    }

    /// Removes every entry whose sequence number is wraparound-less-than
    /// `bound`, returning their sequence numbers. Bounded to one pass over
    /// `capacity` slots regardless of `bound`'s value -- deliberately does
    /// *not* walk sequence-by-sequence from some starting point up to
    /// `bound`, since `bound` comes off the wire (an ACK) and an
    /// adversarial or corrupted peer could claim a `bound` far ahead,
    /// turning a sequence walk into an unbounded loop.
    pub(crate) fn remove_less_than(&mut self, bound: u32) -> Vec<u32> {
        let mut removed = Vec::new();
        for slot in &mut self.slots {
            if let Some((seq, _)) = slot
                && sequence_less_than(*seq, bound)
            {
                removed.push(*seq);
                *slot = None;
                self.len -= 1;
            }
        }
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_is_empty() {
        let buf: SeqRingBuffer<u8> = SeqRingBuffer::new(4);
        assert!(buf.is_empty());
        assert_eq!(buf.len(), 0);
    }

    #[test]
    fn insert_get_remove_roundtrip() {
        let mut buf = SeqRingBuffer::new(4);
        buf.insert(10, "a");
        buf.insert(11, "b");
        assert_eq!(buf.len(), 2);
        assert_eq!(buf.get(10), Some(&"a"));
        assert_eq!(buf.get(11), Some(&"b"));
        assert_eq!(buf.remove(10), Some("a"));
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.get(10), None);
        assert_eq!(buf.remove(10), None);
    }

    #[test]
    fn get_on_stale_slot_returns_none() {
        // capacity 4: seq 0 and seq 4 collide on the same slot.
        let mut buf = SeqRingBuffer::new(4);
        buf.insert(0, "old");
        assert!(buf.remove(0).is_some());
        // Slot is now empty, so a lookup for the never-inserted colliding
        // sequence number must not find the old value.
        assert_eq!(buf.get(4), None);
    }

    #[test]
    fn insert_reusing_a_slot_without_removal_overwrites() {
        // Exercises the same "at most `capacity` live entries" invariant
        // documented on insert() -- a fresh insert into an occupied slot
        // replaces it and len doesn't double-count.
        let mut buf = SeqRingBuffer::new(4);
        buf.insert(0, "old");
        buf.insert(4, "new");
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.get(4), Some(&"new"));
        assert_eq!(buf.get(0), None);
    }

    #[test]
    fn contains_reflects_occupancy() {
        let mut buf = SeqRingBuffer::new(4);
        assert!(!buf.contains(1));
        buf.insert(1, ());
        assert!(buf.contains(1));
        buf.remove(1);
        assert!(!buf.contains(1));
    }

    #[test]
    fn remove_less_than_removes_only_matching_and_bounds_len() {
        let mut buf = SeqRingBuffer::new(8);
        for seq in 0..5u32 {
            buf.insert(seq, seq);
        }
        let mut removed = buf.remove_less_than(3);
        removed.sort_unstable();
        assert_eq!(removed, vec![0, 1, 2]);
        assert_eq!(buf.len(), 2);
        assert!(buf.get(3).is_some());
        assert!(buf.get(4).is_some());
    }

    #[test]
    fn remove_less_than_handles_wraparound_boundary() {
        // 31-bit sequence space: 0x7FFF_FFFE, 0x7FFF_FFFF are "before" the
        // wrap; 0, 1 are "after" -- sequence_less_than must treat the whole
        // set as chronologically ordered despite the numeric wrap.
        let mut buf = SeqRingBuffer::new(8);
        buf.insert(0x7FFF_FFFE, "a");
        buf.insert(0x7FFF_FFFF, "b");
        buf.insert(0, "c");
        buf.insert(1, "d");

        // ACK claiming everything up to (not including) 1 is received:
        // 0x7FFF_FFFE, 0x7FFF_FFFF, and 0 should all be removed; 1 stays.
        let mut removed = buf.remove_less_than(1);
        removed.sort_unstable();
        assert_eq!(removed, vec![0, 0x7FFF_FFFE, 0x7FFF_FFFF]);
        assert_eq!(buf.len(), 1);
        assert_eq!(buf.get(1), Some(&"d"));
    }

    #[test]
    fn iter_yields_all_occupied_entries() {
        let mut buf = SeqRingBuffer::new(8);
        buf.insert(2, "a");
        buf.insert(5, "b");
        let mut seen: Vec<(u32, &&str)> = buf.iter().collect();
        seen.sort_unstable_by_key(|(seq, _)| *seq);
        assert_eq!(seen, vec![(2, &"a"), (5, &"b")]);
    }
}
