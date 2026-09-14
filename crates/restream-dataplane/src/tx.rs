//! Bounded TX storage with explicit completion ownership.

use crate::BufferPool;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxState {
    Filling,
    Submitted,
    WaitingZcNotification,
    Retired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TxLease {
    pub slot: u32,
    pub generation: u32,
}

#[derive(Debug)]
pub struct TxPool {
    buffers: BufferPool,
    generations: Box<[u32]>,
    states: Box<[Option<TxState>]>,
}

impl TxPool {
    pub fn new(slot_count: usize, slot_size: usize) -> Result<Self, crate::CapacityError> {
        Ok(Self {
            buffers: BufferPool::new(slot_count, slot_size)?,
            generations: vec![0; slot_count].into_boxed_slice(),
            states: vec![None; slot_count].into_boxed_slice(),
        })
    }

    pub fn acquire(&mut self) -> Option<TxLease> {
        let slot = self.buffers.acquire()?;
        let lease = TxLease {
            slot,
            generation: self.generations[slot as usize],
        };
        self.states[slot as usize] = Some(TxState::Filling);
        Some(lease)
    }

    pub fn slot_mut(&mut self, lease: TxLease) -> Option<&mut [u8]> {
        self.valid(lease, TxState::Filling)
            .then(|| self.buffers.slot_mut(lease.slot))
            .flatten()
    }

    pub fn slot(&self, lease: TxLease) -> Option<&[u8]> {
        matches!(
            self.state(lease),
            Some(TxState::Filling | TxState::Submitted | TxState::WaitingZcNotification)
        )
        .then(|| self.buffers.slot(lease.slot))
        .flatten()
    }

    pub fn submit(&mut self, lease: TxLease) -> bool {
        self.transition(lease, TxState::Filling, TxState::Submitted)
    }

    pub fn abort(&mut self, lease: TxLease) -> bool {
        if !self.valid(lease, TxState::Filling) {
            return false;
        }
        self.states[lease.slot as usize] = Some(TxState::Retired);
        self.release(lease)
    }

    pub fn await_zc_notification(&mut self, lease: TxLease) -> bool {
        self.transition(lease, TxState::Submitted, TxState::WaitingZcNotification)
    }

    pub fn complete(&mut self, lease: TxLease) -> bool {
        let state = self.state(lease);
        if !matches!(
            state,
            Some(TxState::Submitted | TxState::WaitingZcNotification)
        ) {
            return false;
        }
        self.states[lease.slot as usize] = Some(TxState::Retired);
        true
    }

    pub fn release(&mut self, lease: TxLease) -> bool {
        if !self.valid(lease, TxState::Retired) {
            return false;
        }
        self.states[lease.slot as usize] = None;
        self.generations[lease.slot as usize] = lease.generation.wrapping_add(1);
        self.buffers.release(lease.slot)
    }

    pub fn state(&self, lease: TxLease) -> Option<TxState> {
        (self.generations.get(lease.slot as usize) == Some(&lease.generation))
            .then(|| self.states.get(lease.slot as usize).copied().flatten())
            .flatten()
    }

    pub fn available(&self) -> usize {
        self.buffers.available()
    }

    fn valid(&self, lease: TxLease, expected: TxState) -> bool {
        self.state(lease) == Some(expected)
    }

    fn transition(&mut self, lease: TxLease, from: TxState, to: TxState) -> bool {
        if !self.valid(lease, from) {
            return false;
        }
        self.states[lease.slot as usize] = Some(to);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zero_copy_completion_keeps_storage_until_notification() {
        let mut pool = TxPool::new(1, 32).unwrap();
        let lease = pool.acquire().unwrap();
        pool.slot_mut(lease).unwrap()[..3].copy_from_slice(b"tx!");
        assert_eq!(pool.slot(lease).unwrap()[..3], *b"tx!");
        assert!(pool.submit(lease));
        assert!(pool.await_zc_notification(lease));
        assert_eq!(pool.available(), 0);
        assert!(pool.complete(lease));
        assert!(pool.release(lease));
        assert_eq!(pool.available(), 1);
        assert!(!pool.release(lease));
    }
}
