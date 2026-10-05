//! Shard leaf storage. A [`LeafKey`] names a slot *and* one occupancy of it
//! (the slot's epoch). Removing a leaf bumps its slot's epoch, so every key,
//! queued entry, readiness event or I/O completion that still names the old
//! leaf stops resolving, even after the slot is reused by another output.
//! This is the one stale check for slot-addressed events; backends do not
//! compare generations per handler for them.
//!
//! An epoch wraps after 2^32 reuses of one slot; a key held across that many
//! reuses could alias. Keys live for one queue pass or one in-flight I/O
//! operation, far below that.

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct LeafKey {
    slot: u32,
    epoch: u32,
}

impl LeafKey {
    /// A key for tests that address slots directly (scheduler, poller). It
    /// resolves in an arena only while that slot's epoch matches.
    #[cfg(test)]
    pub(crate) const fn for_test(slot: u32, epoch: u32) -> Self {
        Self { slot, epoch }
    }

    /// The slot index, for test fakes that keep their own slab.
    #[cfg(test)]
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

    /// Free `key`'s slot, reserved or filled, returning the leaf if there
    /// was one; `key` and every copy of it stop resolving. A stale key frees
    /// nothing.
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

    /// Remove every leaf (shard shutdown), as [`Self::remove`] would.
    pub fn drain(&mut self) -> impl Iterator<Item = T> + '_ {
        let free = &mut self.free;
        self.slots
            .iter_mut()
            .zip(0u32..)
            .filter_map(move |(entry, slot)| {
                let value = entry.value.take()?;
                entry.epoch = entry.epoch.wrapping_add(1);
                free.push(slot);
                Some(value)
            })
    }
}

#[cfg(test)]
#[path = "leaf_arena_tests.rs"]
mod tests;
