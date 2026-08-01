//! Egress scheduler: `ReadyQueue` and `ScheduleState`.
//!
//! Implements bounded round-robin (with optional deficit extension).
//! Core invariant: each leaf appears in the ready queue at most once.
//!
//! The `enqueued` flag is the authority; it changes only through the
//! `enqueue` / `dequeue_next` helpers so the invariant is verifiable.

use std::collections::VecDeque;
use std::time::Instant;

// ---------------------------------------------------------------------------
// LeafKey
// ---------------------------------------------------------------------------

/// Index into a shard's `Slab<Leaf<_>>`. Cheap to copy, never reallocated
/// for the same generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeafKey(pub usize);

// ---------------------------------------------------------------------------
// ScheduleState — per-leaf
// ---------------------------------------------------------------------------

/// Scheduling metadata carried on every leaf's `LeafCommon`.
#[derive(Debug, Clone)]
pub struct ScheduleState {
    /// `true` iff this leaf is currently in the shard's ready queue.
    /// Must be the only place that changes.
    pub enqueued: bool,
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
    /// `FeedOverrun`, `PeerClosed`, `Failed`, `Yield`).
    ///
    /// Advisory only, not authoritative like `enqueued`: a feed-wake
    /// direct-enqueue and a real poller-discovered enqueue both still
    /// check `!enqueued` before pushing, so this flag being stale between
    /// visits can never cause a double enqueue.
    pub wants_feed_wake: bool,
}

impl ScheduleState {
    pub fn new() -> Self {
        Self {
            enqueued: false,
            deficit_bytes: 0,
            last_service_at: None,
            wants_feed_wake: false,
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

/// The shard's scheduler ready queue.
///
/// Maintains the ordering of leaves that are ready to make progress and
/// enforces the one-entry-per-leaf invariant through the `enqueued` bit.
///
/// The caller is responsible for keeping `ScheduleState::enqueued` in sync:
/// call `set_enqueued(leaf_state, true)` before `push_back`, and
/// `set_enqueued(leaf_state, false)` after `dequeue_next`.
#[derive(Debug, Default)]
pub struct ReadyQueue {
    inner: VecDeque<LeafKey>,
}

impl ReadyQueue {
    pub fn new() -> Self {
        Self {
            inner: VecDeque::new(),
        }
    }

    /// Enqueue `key` at the tail. Caller must have set `enqueued = true`.
    ///
    /// This deliberately does not look up `enqueued` itself — the shard loop
    /// manages that bit. Separation keeps the hot path free of map lookups.
    pub fn push_back(&mut self, key: LeafKey) {
        self.inner.push_back(key);
    }

    /// Dequeue the next ready leaf key. Caller must set `enqueued = false`
    /// on the returned leaf.
    pub fn dequeue_next(&mut self) -> Option<LeafKey> {
        self.inner.pop_front()
    }

    /// Re-append a still-runnable leaf to the tail (after a partial visit).
    /// Caller must ensure `enqueued` remains `true`.
    pub fn push_back_runnable(&mut self, key: LeafKey) {
        self.inner.push_back(key);
    }

    /// Number of currently ready leaves.
    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Drain all keys (e.g. during shard shutdown). Caller is responsible for
    /// clearing `enqueued` on each drained leaf.
    pub fn drain(&mut self) -> impl Iterator<Item = LeafKey> + '_ {
        self.inner.drain(..)
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

/// Check whether a leaf's `ScheduleState` allows it to be enqueued.
///
/// Callers should call this before `push_back` to preserve the invariant.
pub fn can_enqueue(schedule: &ScheduleState) -> bool {
    !schedule.enqueued
}

/// Mark a leaf as enqueued and push it to the queue.
///
/// Returns `false` (and does not push) if the leaf was already enqueued.
pub fn try_enqueue(schedule: &mut ScheduleState, queue: &mut ReadyQueue, key: LeafKey) -> bool {
    if schedule.enqueued {
        return false;
    }
    schedule.enqueued = true;
    queue.push_back(key);
    true
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulate a minimal slab of leaves (just their ScheduleState).
    struct FakeSlab {
        states: Vec<ScheduleState>,
    }

    impl FakeSlab {
        fn new(n: usize) -> Self {
            Self {
                states: vec![ScheduleState::new(); n],
            }
        }
    }

    #[test]
    fn try_enqueue_deduplicates() {
        let mut slab = FakeSlab::new(3);
        let mut queue = ReadyQueue::new();

        // Enqueue leaf 0 once.
        assert!(try_enqueue(&mut slab.states[0], &mut queue, LeafKey(0)));
        // Second attempt returns false and does not double-enqueue.
        assert!(!try_enqueue(&mut slab.states[0], &mut queue, LeafKey(0)));

        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn round_robin_ordering() {
        let mut slab = FakeSlab::new(4);
        let mut queue = ReadyQueue::new();

        for i in 0..4 {
            try_enqueue(&mut slab.states[i], &mut queue, LeafKey(i));
        }

        // Should come out FIFO.
        for expected in 0..4usize {
            let key = queue.dequeue_next().unwrap();
            slab.states[key.0].enqueued = false;
            assert_eq!(key.0, expected);
        }
        assert!(queue.is_empty());
    }

    #[test]
    fn blocked_leaf_not_reenqueued() {
        let mut slab = FakeSlab::new(2);
        let mut queue = ReadyQueue::new();

        try_enqueue(&mut slab.states[0], &mut queue, LeafKey(0));
        try_enqueue(&mut slab.states[1], &mut queue, LeafKey(1));

        // Dequeue leaf 0 and decide to suspend it (transport blocked).
        let key = queue.dequeue_next().unwrap();
        assert_eq!(key.0, 0);
        slab.states[0].enqueued = false; // suspend: clear enqueued, do NOT re-push.

        // Leaf 1 is still in queue.
        assert_eq!(queue.len(), 1);
        let key = queue.dequeue_next().unwrap();
        assert_eq!(key.0, 1);
        // If leaf 1 has more work, re-append it.
        queue.push_back_runnable(key);
        assert_eq!(queue.len(), 1);
    }

    #[test]
    fn always_writable_leaf_rotates() {
        // An always-writable leaf must not stay at the head.
        let mut slab = FakeSlab::new(3);
        let mut queue = ReadyQueue::new();

        for i in 0..3 {
            try_enqueue(&mut slab.states[i], &mut queue, LeafKey(i));
        }

        // Service leaf 0, simulate it still has work → re-append at tail.
        let key0 = queue.dequeue_next().unwrap();
        assert_eq!(key0.0, 0);
        queue.push_back_runnable(key0); // still runnable, goes to tail.

        // Next service is leaf 1, not leaf 0 again.
        let key1 = queue.dequeue_next().unwrap();
        assert_eq!(key1.0, 1);
    }

    #[test]
    fn drain_clears_queue() {
        let mut slab = FakeSlab::new(5);
        let mut queue = ReadyQueue::new();
        for i in 0..5 {
            try_enqueue(&mut slab.states[i], &mut queue, LeafKey(i));
        }
        let drained: Vec<_> = queue.drain().collect();
        assert_eq!(drained.len(), 5);
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

    #[test]
    fn can_enqueue_reflects_flag() {
        let mut s = ScheduleState::new();
        assert!(can_enqueue(&s));
        s.enqueued = true;
        assert!(!can_enqueue(&s));
    }
}
