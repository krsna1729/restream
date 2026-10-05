//! Egress scheduler: `ReadyQueue` and `ScheduleState`.
//!
//! Core invariant: each leaf appears in the ready queue at most once. The
//! queue itself owns that: it records which key each slot has queued, so a
//! push of a queued key is a no-op and no caller keeps a flag in step.

use std::collections::VecDeque;
use std::time::Instant;

use super::leaf_arena::LeafKey;

// ---------------------------------------------------------------------------
// ScheduleState — per-leaf
// ---------------------------------------------------------------------------

/// Scheduling metadata carried on every leaf's `LeafCommon`.
#[derive(Debug, Clone)]
pub struct ScheduleState {
    /// Accumulated byte deficit for deficit-round-robin scheduling.
    /// Reset after each successful service.
    pub deficit_bytes: usize,
    /// Instant of the most recent scheduler service visit.
    pub last_service_at: Option<Instant>,
    /// `true` iff the most recent `EngineProgress`'s `WaitCondition` was
    /// `Feed` or `FeedOrIo` — i.e. a feed wake should directly re-enqueue
    /// this leaf. Set unconditionally from every visit outcome in
    /// `apply_progress_to_common` (`visit.rs`); `false` for outcomes that
    /// don't carry a wait condition at all (`HandshakeComplete`,
    /// `FeedOverrun`, `PeerClosed`, `Failed`, `Yield`). Advisory: a stale
    /// value can never double-queue a leaf, because the ready queue
    /// refuses a key it holds.
    pub wants_feed_wake: bool,
    /// Whether this leaf currently has one entry in the shard's feed-waiting
    /// queue; it prevents repeated readiness visits from growing the parked
    /// queue without bound before the next feed wake.
    pub feed_wake_queued: bool,
}

impl ScheduleState {
    pub fn new() -> Self {
        Self {
            deficit_bytes: 0,
            last_service_at: None,
            wants_feed_wake: false,
            feed_wake_queued: false,
        }
    }

    pub fn mark_serviced(&mut self) {
        self.last_service_at = Some(Instant::now());
        self.deficit_bytes = 0;
    }
}

impl Default for ScheduleState {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// ReadyQueue
// ---------------------------------------------------------------------------

const DEFAULT_READY_CAPACITY: usize = 4096;

/// What `ReadyQueue::push` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Push {
    Queued,
    /// The key was already queued; it keeps its place.
    AlreadyQueued,
    /// The queue is at capacity; the key is not queued.
    Full,
}

impl Push {
    /// The key is in the queue after the push.
    pub fn is_queued(self) -> bool {
        !matches!(self, Self::Full)
    }
}

/// The shard's FIFO of leaves ready to make progress, holding each key at
/// most once. `member[slot]` is the key that slot has queued: a key from a
/// removed leaf (an older epoch) left in `order` is skipped by `pop` and
/// never blocks the slot's next occupant.
#[derive(Debug)]
pub struct ReadyQueue {
    order: VecDeque<LeafKey>,
    member: Vec<Option<LeafKey>>,
    capacity: usize,
}

impl ReadyQueue {
    pub fn new() -> Self {
        Self::with_capacity(DEFAULT_READY_CAPACITY)
    }

    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            order: VecDeque::with_capacity(capacity),
            member: vec![None; capacity],
            capacity,
        }
    }

    pub fn contains(&self, key: LeafKey) -> bool {
        self.member.get(key.slot()) == Some(&Some(key))
    }

    /// Queue `key` at the tail unless it is already queued. Push only keys
    /// of live leaves: a removed leaf's key pushed after its slot's next
    /// occupant would replace that occupant's entry (a lost wakeup).
    pub fn push(&mut self, key: LeafKey) -> Push {
        if self.contains(key) {
            return Push::AlreadyQueued;
        }
        if self.order.len() >= self.capacity {
            return Push::Full;
        }
        let slot = key.slot();
        if slot >= self.member.len() {
            self.member.resize(slot.saturating_add(1), None);
        }
        if let Some(entry) = self.member.get_mut(slot) {
            *entry = Some(key);
        }
        self.order.push_back(key);
        Push::Queued
    }

    /// Take the next queued key; it may be pushed again at once.
    pub fn pop(&mut self) -> Option<LeafKey> {
        while let Some(key) = self.order.pop_front() {
            if let Some(entry) = self.member.get_mut(key.slot())
                && *entry == Some(key)
            {
                *entry = None;
                return Some(key);
            }
        }
        None
    }

    /// The key `pop` would return next.
    pub fn front(&self) -> Option<LeafKey> {
        self.order.iter().copied().find(|key| self.contains(*key))
    }

    /// Drop `key` (its leaf was removed).
    pub fn remove(&mut self, key: LeafKey) {
        if self.contains(key) {
            if let Some(entry) = self.member.get_mut(key.slot()) {
                *entry = None;
            }
            self.order.retain(|queued| *queued != key);
        }
    }

    /// Queue entries, including any not yet skipped stale entry; for
    /// work-remaining checks, never zero while a key is queued.
    pub fn len(&self) -> usize {
        self.order.len()
    }

    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }

    /// Empty the queue (shard shutdown).
    pub fn clear(&mut self) {
        self.order.clear();
        self.member.iter_mut().for_each(|entry| *entry = None);
    }
}

// ---------------------------------------------------------------------------
// Scheduler helpers — shard-loop logic
// ---------------------------------------------------------------------------

/// Decision made by the scheduler for one leaf visit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VisitDecision {
    /// Leaf made progress; re-append to the tail for further work.
    Continue,
    /// Leaf is now blocked (transport would block, or budget exhausted with
    /// no more useful work). Remove it from the ready queue.
    Suspend,
    /// Leaf needs to be closed by the shard.
    Close,
}

impl Default for ReadyQueue {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    fn key(slot: u32, epoch: u32) -> LeafKey {
        LeafKey::for_test(slot, epoch)
    }

    #[test]
    fn a_queued_key_is_not_queued_twice() {
        let mut queue = ReadyQueue::with_capacity(4);
        assert_eq!(queue.push(key(0, 0)), Push::Queued);
        assert_eq!(queue.push(key(0, 0)), Push::AlreadyQueued);
        assert_eq!(queue.len(), 1);
        assert_eq!(queue.pop(), Some(key(0, 0)));
        assert_eq!(
            queue.push(key(0, 0)),
            Push::Queued,
            "popped keys may return"
        );
    }

    #[test]
    fn the_queue_has_a_hard_capacity() {
        let mut queue = ReadyQueue::with_capacity(1);
        assert_eq!(queue.push(key(0, 0)), Push::Queued);
        assert_eq!(queue.push(key(1, 0)), Push::Full);
        assert!(!queue.contains(key(1, 0)));
    }

    #[test]
    fn a_removed_leafs_entry_never_blocks_or_aliases_its_slots_next_leaf() {
        let mut queue = ReadyQueue::with_capacity(4);
        assert_eq!(queue.push(key(2, 0)), Push::Queued);
        // The slot is reused without a remove: the new key queues anyway,
        // and the old entry is skipped, not visited.
        assert_eq!(queue.push(key(2, 1)), Push::Queued);
        assert!(!queue.contains(key(2, 0)));
        assert_eq!(queue.pop(), Some(key(2, 1)));
        assert_eq!(queue.pop(), None);
    }

    #[test]
    fn remove_takes_a_key_out_of_order_and_membership() {
        let mut queue = ReadyQueue::with_capacity(4);
        for slot in 0..3 {
            queue.push(key(slot, 0));
        }
        queue.remove(key(1, 0));
        assert_eq!(queue.front(), Some(key(0, 0)));
        assert_eq!(queue.pop(), Some(key(0, 0)));
        assert_eq!(queue.pop(), Some(key(2, 0)));
        assert!(queue.is_empty());
    }

    #[test]
    fn schedule_state_serviced_resets_deficit() {
        let mut s = ScheduleState::new();
        s.deficit_bytes = 1000;
        s.mark_serviced();
        assert_eq!(s.deficit_bytes, 0);
        assert!(s.last_service_at.is_some());
    }

    #[derive(Debug, Clone, Copy)]
    enum Op {
        Push(u32, u32),
        Pop,
        Remove(u32, u32),
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(2000))]

        /// Against a model (FIFO of distinct live keys, at most one live key
        /// per slot): pops come out in push order, each key at most once,
        /// a stale key never comes out, and capacity holds.
        #[test]
        fn the_queue_matches_a_fifo_of_distinct_keys(
            ops in prop::collection::vec(
                prop_oneof![
                    (0u32..4, 0u32..3).prop_map(|(s, e)| Op::Push(s, e)),
                    Just(Op::Pop),
                    (0u32..4, 0u32..3).prop_map(|(s, e)| Op::Remove(s, e)),
                ],
                0..80,
            ),
        ) {
            let capacity = 6;
            let mut queue = ReadyQueue::with_capacity(capacity);
            let mut model: VecDeque<LeafKey> = VecDeque::new();
            let mut entries = 0usize;
            // Slot epochs only increase (a removed leaf's key never returns).
            let mut newest = [0u32; 4];
            for op in ops {
                match op {
                    Op::Push(slot, epoch) => {
                        if epoch < newest[slot as usize] {
                            continue;
                        }
                        newest[slot as usize] = epoch;
                        let k = key(slot, epoch);
                        let result = queue.push(k);
                        if model.contains(&k) {
                            prop_assert_eq!(result, Push::AlreadyQueued);
                        } else if entries >= capacity {
                            prop_assert_eq!(result, Push::Full);
                        } else {
                            prop_assert_eq!(result, Push::Queued);
                            // A newer key for the slot supersedes the old.
                            model.retain(|queued| queued.slot() != k.slot());
                            model.push_back(k);
                            entries += 1;
                        }
                    }
                    Op::Pop => {
                        let popped = queue.pop();
                        prop_assert_eq!(popped, model.pop_front());
                        entries = queue.len();
                    }
                    Op::Remove(slot, epoch) => {
                        let k = key(slot, epoch);
                        queue.remove(k);
                        model.retain(|queued| *queued != k);
                        entries = queue.len();
                    }
                }
                for k in &model {
                    prop_assert!(queue.contains(*k));
                }
                prop_assert!(queue.len() <= capacity);
            }
        }
    }
}
