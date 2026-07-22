//! Minimal timer structure for the egress fabric shard.
//!
//! Provides a heap-based timer wheel that supports per-entry generation tags
//! so stale entries (from superseded leaf generations) are silently ignored
//! on expiry.
//!
//! Phase 3 will integrate this with the real shard OS-thread loop. Phase 1
//! exposes the structure and tests its correctness in isolation.

use std::cmp::Reverse;
use std::collections::BinaryHeap;
use std::time::Instant;

// ---------------------------------------------------------------------------
// TimerEntry
// ---------------------------------------------------------------------------

/// A scheduled timer entry.
#[derive(Debug, Clone)]
struct TimerEntry<K> {
    /// Absolute time this entry fires.
    fire_at: Instant,
    /// The leaf key to wake.
    key: K,
    /// Generation tag. If the leaf's current generation differs when the
    /// timer fires, the entry is silently dropped.
    generation: u64,
}

// BinaryHeap is a max-heap; we want min-heap (earliest fires first).
impl<K: Ord> PartialEq for TimerEntry<K> {
    fn eq(&self, other: &Self) -> bool {
        self.fire_at == other.fire_at
    }
}
impl<K: Ord> Eq for TimerEntry<K> {}
impl<K: Ord> PartialOrd for TimerEntry<K> {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl<K: Ord> Ord for TimerEntry<K> {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        // Reverse so earliest deadline is at the top.
        Reverse(self.fire_at).cmp(&Reverse(other.fire_at))
    }
}

// ---------------------------------------------------------------------------
// TimerWheel
// ---------------------------------------------------------------------------

/// Generation-aware min-heap timer for egress shard leaves.
///
/// Entries with a stale `generation` are silently skipped when draining. This
/// prevents cancelled or updated leaves from causing spurious wakeups without
/// requiring an O(n) scan to remove them.
#[derive(Debug, Default)]
pub struct TimerWheel<K: Ord> {
    heap: BinaryHeap<TimerEntry<K>>,
}

impl<K: Ord + Clone> TimerWheel<K> {
    pub fn new() -> Self {
        Self {
            heap: BinaryHeap::new(),
        }
    }

    /// Schedule a wakeup for `key` at `fire_at`.
    ///
    /// Multiple entries for the same key are allowed; the caller must use
    /// `generation` to distinguish which entry is still valid.
    pub fn insert(&mut self, fire_at: Instant, key: K, generation: u64) {
        self.heap.push(TimerEntry {
            fire_at,
            key,
            generation,
        });
    }

    /// Returns the instant of the soonest pending timer, or `None` if empty.
    pub fn next_deadline(&self) -> Option<Instant> {
        self.heap.peek().map(|e| e.fire_at)
    }

    /// Drain all entries whose `fire_at <= now` and whose generation matches
    /// `valid_gen(key)`. Stale entries are consumed without being returned.
    ///
    /// `valid_gen` is a closure that maps a key to its current valid
    /// generation (e.g. `|k| slab[k].generation`).
    pub fn drain_expired<F>(&mut self, now: Instant, mut valid_gen: F) -> Vec<(K, u64)>
    where
        F: FnMut(&K) -> Option<u64>,
    {
        let mut fired = Vec::new();
        while let Some(entry) = self.heap.peek() {
            if entry.fire_at > now {
                break;
            }
            let entry = self.heap.pop().unwrap();
            // Accept only if the generation still matches.
            match valid_gen(&entry.key) {
                Some(current_gen) if current_gen == entry.generation => {
                    fired.push((entry.key, entry.generation));
                }
                _ => {
                    // Stale entry — silently discard.
                }
            }
        }
        fired
    }

    /// Remove all entries (e.g. during shard shutdown).
    pub fn clear(&mut self) {
        self.heap.clear();
    }

    /// Number of pending timer entries (including stale ones not yet expired).
    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn fires_in_order() {
        let now = Instant::now();
        let mut wheel = TimerWheel::<u32>::new();
        wheel.insert(now + Duration::from_millis(200), 2, 2);
        wheel.insert(now + Duration::from_millis(100), 1, 1);
        wheel.insert(now + Duration::from_millis(300), 3, 3);

        // None should fire yet (now is before all deadlines).
        let fired = wheel.drain_expired(now, |k| Some(*k as u64));
        assert!(fired.is_empty());

        // After 150ms, only entry with key=1 should fire.
        let fired = wheel.drain_expired(now + Duration::from_millis(150), |k| Some(*k as u64));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].0, 1);

        // After 250ms, entry with key=2 fires.
        let fired = wheel.drain_expired(now + Duration::from_millis(250), |k| Some(*k as u64));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].0, 2);

        // After 350ms, entry with key=3 fires.
        let fired = wheel.drain_expired(now + Duration::from_millis(350), |k| Some(*k as u64));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].0, 3);
    }

    #[test]
    fn stale_generation_skipped() {
        let now = Instant::now();
        let mut wheel = TimerWheel::<u32>::new();

        // Schedule with generation 1; then "update" to generation 2.
        wheel.insert(now, 0, 1);

        // The slab reports generation 2 for this key — entry is stale.
        let fired = wheel.drain_expired(now + Duration::from_millis(1), |_k| Some(2));
        assert!(fired.is_empty(), "stale entry should not fire");
    }

    #[test]
    fn missing_key_skipped() {
        let now = Instant::now();
        let mut wheel = TimerWheel::<u32>::new();
        wheel.insert(now, 0, 1);

        // Key no longer exists in slab (valid_gen returns None).
        let fired = wheel.drain_expired(now + Duration::from_millis(1), |_k| None);
        assert!(fired.is_empty(), "removed key should not fire");
    }

    #[test]
    fn next_deadline_updates_after_drain() {
        let now = Instant::now();
        let mut wheel = TimerWheel::<u32>::new();
        wheel.insert(now + Duration::from_millis(10), 1, 1);
        wheel.insert(now + Duration::from_millis(50), 2, 1);

        assert!(wheel.next_deadline().is_some());

        // Drain the first.
        wheel.drain_expired(now + Duration::from_millis(20), |k| Some(*k as u64));

        // Remaining deadline is for key 2.
        let next = wheel.next_deadline().unwrap();
        assert!(next > now + Duration::from_millis(20));
    }

    #[test]
    fn clear_empties_wheel() {
        let now = Instant::now();
        let mut wheel = TimerWheel::<u32>::new();
        for i in 0..10 {
            wheel.insert(now + Duration::from_millis(i), i as u32, 1);
        }
        wheel.clear();
        assert!(wheel.is_empty());
    }

    #[test]
    fn multiple_entries_same_key_different_generations() {
        let now = Instant::now();
        let mut wheel = TimerWheel::<u32>::new();

        // Old timer (generation 1) and new timer (generation 2) for same key.
        wheel.insert(now + Duration::from_millis(10), 0, 1);
        wheel.insert(now + Duration::from_millis(20), 0, 2);

        // Current generation is 2; old entry should be skipped, new should fire.
        let fired = wheel.drain_expired(now + Duration::from_millis(25), |_k| Some(2));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].1, 2);
    }
}
