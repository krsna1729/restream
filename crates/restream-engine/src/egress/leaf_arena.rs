//! Shard leaf storage. A [`LeafKey`] names a slot *and* one occupancy of it
//! (the slot's epoch). Removing a leaf bumps its slot's epoch, so every key,
//! queued entry, readiness event or I/O completion that still names the old
//! leaf stops resolving, even after the slot is reused by another output.
//! This is the one stale check for slot-addressed events; backends do not
//! compare generations per handler for them.
//!
//! An epoch wraps after 2^32 reuses of one slot; a key held across that many
//! reuses could alias. Queued entries and in-flight I/O hold a key for one
//! pass or one operation; owner maps (`output_sockets`, `callers`) hold it
//! for the leaf's life and drop it on removal, so no key survives that long.

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct LeafKey {
    slot: u32,
    epoch: u32,
}

impl LeafKey {
    /// A key for tests that address slots directly (scheduler, poller). It
    /// resolves in an arena only while that slot's epoch matches.
    #[cfg(any(test, kani))]
    pub(crate) const fn for_test(slot: u32, epoch: u32) -> Self {
        Self { slot, epoch }
    }

    /// The slot index (dense per shard): the ready queue's membership index.
    pub(crate) const fn slot(self) -> usize {
        self.slot as usize
    }
}

#[derive(Debug)]
struct Slot<T> {
    epoch: u32,
    value: Option<T>,
}

/// Fixed-capacity slots for one shard's leaves.
#[derive(Debug)]
pub struct LeafArena<T> {
    slots: Vec<Slot<T>>,
    /// Free slot indices; popped from the end, so slot 0 is used first.
    free: Vec<u32>,
}

impl<T> LeafArena<T> {
    pub fn with_capacity(capacity: usize) -> Self {
        let capacity = u32::try_from(capacity).unwrap_or(u32::MAX);
        Self {
            slots: (0..capacity)
                .map(|_| Slot {
                    epoch: 0,
                    value: None,
                })
                .collect(),
            free: (0..capacity).rev().collect(),
        }
    }

    pub fn capacity(&self) -> usize {
        self.slots.len()
    }

    pub fn is_full(&self) -> bool {
        self.free.is_empty()
    }

    pub fn len(&self) -> usize {
        self.slots.len().saturating_sub(self.free.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Store the value `make` builds for its new key; `None` when full.
    pub fn insert_with(&mut self, make: impl FnOnce(LeafKey) -> T) -> Option<LeafKey> {
        let key = self.reserve()?;
        let entry = self.slots.get_mut(key.slot as usize)?;
        entry.value = Some(make(key));
        Some(key)
    }

    /// Take a slot for a leaf that does not exist yet (a connect in
    /// flight). It resolves to nothing until [`Self::fill`]; [`Self::remove`]
    /// frees it either way.
    pub fn reserve(&mut self) -> Option<LeafKey> {
        let slot = self.free.pop()?;
        let epoch = self.slots.get(slot as usize)?.epoch;
        Some(LeafKey { slot, epoch })
    }

    /// Put the leaf into its reserved slot. Refused (value handed back) when
    /// `key` is stale or the slot is already filled.
    pub fn fill(&mut self, key: LeafKey, value: T) -> Result<(), T> {
        match self
            .slots
            .get_mut(key.slot as usize)
            .filter(|entry| entry.epoch == key.epoch && entry.value.is_none())
        {
            Some(entry) => {
                entry.value = Some(value);
                Ok(())
            }
            None => Err(value),
        }
    }

    pub fn get(&self, key: LeafKey) -> Option<&T> {
        self.slots
            .get(key.slot as usize)
            .filter(|entry| entry.epoch == key.epoch)
            .and_then(|entry| entry.value.as_ref())
    }

    pub fn get_mut(&mut self, key: LeafKey) -> Option<&mut T> {
        self.slots
            .get_mut(key.slot as usize)
            .filter(|entry| entry.epoch == key.epoch)
            .and_then(|entry| entry.value.as_mut())
    }

    /// Free a slot taken by [`Self::reserve`] and never filled (a connect
    /// that failed). A filled slot must go through [`Self::remove`], which
    /// hands the leaf back to be closed.
    pub fn release_reserved(&mut self, key: LeafKey) {
        let leaf = self.remove(key);
        debug_assert!(leaf.is_none(), "release_reserved on a filled slot");
        drop(leaf);
    }

    /// Free `key`'s slot, reserved or filled, returning the leaf if there
    /// was one; `key` and every copy of it stop resolving. A stale key frees
    /// nothing.
    #[must_use = "a removed leaf must be closed"]
    pub fn remove(&mut self, key: LeafKey) -> Option<T> {
        let entry = self
            .slots
            .get_mut(key.slot as usize)
            .filter(|entry| entry.epoch == key.epoch)?;
        let value = entry.value.take();
        entry.epoch = entry.epoch.wrapping_add(1);
        self.free.push(key.slot);
        value
    }

    /// Every live leaf with its key.
    pub fn iter(&self) -> impl Iterator<Item = (LeafKey, &T)> {
        self.slots.iter().zip(0u32..).filter_map(|(entry, slot)| {
            let key = LeafKey {
                slot,
                epoch: entry.epoch,
            };
            entry.value.as_ref().map(|value| (key, value))
        })
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (LeafKey, &mut T)> {
        self.slots
            .iter_mut()
            .zip(0u32..)
            .filter_map(|(entry, slot)| {
                let key = LeafKey {
                    slot,
                    epoch: entry.epoch,
                };
                entry.value.as_mut().map(|value| (key, value))
            })
    }

    /// Free every slot, filled or reserved (shard shutdown), as
    /// [`Self::remove`] would, yielding the leaves.
    pub fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        let in_use: Vec<bool> = {
            let mut in_use = vec![true; self.slots.len()];
            for &slot in &self.free {
                if let Some(flag) = in_use.get_mut(slot as usize) {
                    *flag = false;
                }
            }
            in_use
        };
        let free = &mut self.free;
        self.slots
            .iter_mut()
            .zip(in_use)
            .zip(0u32..)
            .filter_map(move |((entry, in_use), slot)| {
                if !in_use {
                    return None;
                }
                entry.epoch = entry.epoch.wrapping_add(1);
                free.push(slot);
                entry.value.take()
            })
    }
}

#[cfg(test)]
#[path = "leaf_arena_tests.rs"]
mod tests;

/// Rung 4 (docs/assurance-roadmap.md): for every bounded sequence of
/// inserts, reserves, fills and removals with fresh and stale keys, a key
/// resolves exactly while its slot holds that key's leaf (a removed key
/// never resolves again) and the arena holds at most its capacity.
#[cfg(kani)]
mod kani_proofs {
    use super::*;

    const CAPACITY: usize = 2;
    const STEPS: usize = 4;

    /// One key the arena handed out, as the caller sees it.
    #[derive(Clone, Copy)]
    struct Issued {
        key: LeafKey,
        value: Option<u8>,
        live: bool,
    }

    #[kani::proof]
    #[kani::unwind(6)]
    #[kani::solver(kissat)]
    fn a_key_resolves_exactly_while_its_leaf_is_live() {
        let mut arena = LeafArena::<u8>::with_capacity(CAPACITY);
        let mut issued = [None::<Issued>; STEPS];
        let mut count = 0;
        let mut live = 0;
        for _ in 0..STEPS {
            let operation: u8 = kani::any();
            kani::assume(operation < 4);
            if operation < 2 {
                let value: u8 = kani::any();
                let key = if operation == 0 {
                    arena.insert_with(|_| value)
                } else {
                    arena.reserve()
                };
                assert_eq!(key.is_some(), live < CAPACITY);
                if let Some(key) = key {
                    kani::cover!(
                        issued[..count]
                            .iter()
                            .flatten()
                            .any(|entry| entry.key.slot == key.slot),
                        "a slot is reused at a new epoch"
                    );
                    let value = (operation == 0).then_some(value);
                    issued[count] = Some(Issued {
                        key,
                        value,
                        live: true,
                    });
                    count += 1;
                    live += 1;
                }
            } else if count > 0 {
                let index: usize = kani::any();
                kani::assume(index < count);
                let Some(target) = issued[index] else {
                    unreachable!()
                };
                if operation == 2 {
                    kani::cover!(!target.live, "a stale key's fill is refused");
                    let value: u8 = kani::any();
                    let filled = arena.fill(target.key, value).is_ok();
                    assert_eq!(filled, target.live && target.value.is_none());
                    if filled {
                        issued[index] = Some(Issued {
                            value: Some(value),
                            ..target
                        });
                    }
                } else {
                    kani::cover!(!target.live, "a stale key's remove frees nothing");
                    let removed = arena.remove(target.key);
                    assert_eq!(removed, if target.live { target.value } else { None });
                    if target.live {
                        live -= 1;
                    }
                    issued[index] = Some(Issued {
                        live: false,
                        ..target
                    });
                }
            }
            assert_eq!(arena.len(), live);
        }
        for entry in issued.iter().flatten() {
            let expected = if entry.live {
                entry.value.as_ref()
            } else {
                None
            };
            assert_eq!(arena.get(entry.key), expected);
        }
    }
}
