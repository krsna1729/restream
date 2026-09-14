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
use std::collections::HashMap;
use std::hash::Hash;
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
            && self.key == other.key
            && self.generation == other.generation
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
        Reverse(self.fire_at)
            .cmp(&Reverse(other.fire_at))
            .then_with(|| self.key.cmp(&other.key))
            .then_with(|| self.generation.cmp(&other.generation))
    }
}

// ---------------------------------------------------------------------------
// TimerWheel
// ---------------------------------------------------------------------------

/// Generation-aware min-heap timer for egress shard leaves.
///
/// Replacing a key supersedes its previous deadline. Entries with a stale
/// generation are silently skipped when draining, without an O(n) scan.
#[derive(Debug)]
pub struct TimerWheel<K: Ord + Hash> {
    heap: BinaryHeap<TimerEntry<K>>,
    active: HashMap<K, TimerEntry<K>>,
}

impl<K: Ord + Hash + Clone> TimerWheel<K> {
    pub fn new() -> Self {
        Self::with_capacity(0)
    }

    /// Preallocate the expected number of live keys. Replacements reuse the
    /// active entry and stale heap entries are periodically rebuilt in place,
    /// so the heap stays bounded by the live-key count rather than the number
    /// of reschedules.
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            heap: BinaryHeap::with_capacity(capacity.saturating_mul(2).saturating_add(64)),
            active: HashMap::with_capacity(capacity),
        }
    }

    /// Schedule a wakeup for `key` at `fire_at`.
    ///
    /// Replaces any existing deadline for `key`.
    pub fn insert(&mut self, fire_at: Instant, key: K, generation: u64) {
        let entry = TimerEntry {
            fire_at,
            key: key.clone(),
            generation,
        };
        self.active.insert(key, entry.clone());
        self.heap.push(entry);
        self.rebuild_if_needed();
    }

    /// Returns the instant of the soonest pending timer, or `None` if empty.
    pub fn next_deadline(&mut self) -> Option<Instant> {
        self.discard_stale_heads();
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
        self.drain_expired_limited_into(now, usize::MAX, &mut valid_gen, &mut fired);
        fired
    }

    /// Drain at most `max_fired` current entries whose `fire_at <= now`.
    ///
    /// Stale entries are still discarded while searching for current timers.
    /// Current entries beyond the limit remain in the heap for a later pass.
    pub fn drain_expired_limited<F>(
        &mut self,
        now: Instant,
        max_fired: usize,
        mut valid_gen: F,
    ) -> Vec<(K, u64)>
    where
        F: FnMut(&K) -> Option<u64>,
    {
        let mut fired = Vec::new();
        self.drain_expired_limited_into(now, max_fired, &mut valid_gen, &mut fired);
        fired
    }

    /// Drain expired timers into caller-owned storage so a shard can reuse
    /// its result buffer on every loop iteration.
    pub fn drain_expired_limited_into<F>(
        &mut self,
        now: Instant,
        max_fired: usize,
        mut valid_gen: F,
        fired: &mut Vec<(K, u64)>,
    ) where
        F: FnMut(&K) -> Option<u64>,
    {
        fired.clear();
        while fired.len() < max_fired {
            let Some(entry) = self.heap.peek() else {
                break;
            };
            if entry.fire_at > now {
                break;
            }
            let entry = self.heap.pop().unwrap();
            let current = self.active.get(&entry.key).is_some_and(|active| {
                active.fire_at == entry.fire_at && active.generation == entry.generation
            });
            if !current {
                continue;
            }
            self.active.remove(&entry.key);
            // Accept only if the backend generation still matches.
            match valid_gen(&entry.key) {
                Some(current_gen) if current_gen == entry.generation => {
                    fired.push((entry.key, entry.generation));
                }
                _ => {
                    // Stale entry — silently discard.
                }
            }
        }
        self.rebuild_if_needed();
    }

    /// Remove all entries (e.g. during shard shutdown).
    pub fn clear(&mut self) {
        self.heap.clear();
        self.active.clear();
    }

    /// Number of pending timer entries (including stale ones not yet expired).
    pub fn len(&self) -> usize {
        self.active.len()
    }

    pub fn is_empty(&self) -> bool {
        self.active.is_empty()
    }

    fn discard_stale_heads(&mut self) {
        while let Some(entry) = self.heap.peek() {
            let current = self.active.get(&entry.key).is_some_and(|active| {
                active.fire_at == entry.fire_at && active.generation == entry.generation
            });
            if current {
                break;
            }
            self.heap.pop();
        }
    }

    fn rebuild_if_needed(&mut self) {
        let bound = self.active.len().saturating_mul(2).saturating_add(64);
        if self.heap.len() <= bound {
            return;
        }
        self.heap.clear();
        self.heap.extend(self.active.values().cloned());
    }
}

impl<K: Ord + Hash + Clone> Default for TimerWheel<K> {
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
        assert_eq!(wheel.len(), 1);

        // Current generation is 2; old entry should be skipped, new should fire.
        let fired = wheel.drain_expired(now + Duration::from_millis(25), |_k| Some(2));
        assert_eq!(fired.len(), 1);
        assert_eq!(fired[0].1, 2);
    }
}
