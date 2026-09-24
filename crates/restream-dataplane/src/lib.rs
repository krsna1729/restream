//! Small Linux-native dataplane primitives: mechanism, not policy.
//!
//! Production SRT does NOT pass through this crate: SRT ingress and egress run
//! on `srt_transport::compio::Owner` (see `docs/srt-compio-roadmap.md`). What
//! survives here is:
//!
//! * the legacy TCP/`io_uring` mechanisms (`tcp`, `files`), retained for
//!   dataplane tests; production RTMP egress now uses Compio TCP;
//! * reusable scheduler and media primitives (`ReadyQueue`, `DeadlineIndex`,
//!   `MediaArena`/`MediaRing`, `FeedCursor`, `TxPool`) and generic `io_uring`
//!   capability probing;
//! * `Dataplane`/`DataplaneHandle`, the synthetic proof harness that
//!   fixed-population benchmarks and allocation guards run against.
//!
//! `media/egress` owns production scheduling policy (stall classification,
//! drain semantics, feed overrun, reconnect behavior, runtime diagnostics). Do
//! NOT grow production scheduling policy here.

use std::cmp::Ordering as CmpOrdering;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use io_uring::{IoUring, opcode, types};

#[cfg(feature = "loom")]
use loom::sync::atomic::{AtomicBool, Ordering};
#[cfg(not(feature = "loom"))]
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_TAG_SLOTS: usize = 1 << 24;

pub mod capabilities;
pub mod files;
pub mod media;
pub mod tcp;
pub mod tx;

pub use capabilities::{UringCapabilities, UringCapabilityTier};
pub use files::{FixedFile, FixedFileTable};
pub use media::{CursorError, FeedCursor, MediaArena, MediaError, MediaRef, MediaRing};
pub use tx::{TxLease, TxPool, TxState};

// ---------------------------------------------------------------------------
// Operation identity
// ---------------------------------------------------------------------------

/// The operation kind encoded into an io_uring `user_data` value.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpKind {
    Accept = 1,
    Connect = 2,
    TcpRx = 3,
    TcpTx = 4,
    Timeout = 7,
    ControlWake = 8,
    PollCancel = 9,
    TimeoutCancel = 10,
    TcpTxCancel = 11,
}

impl OpKind {
    fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Accept,
            2 => Self::Connect,
            3 => Self::TcpRx,
            4 => Self::TcpTx,
            7 => Self::Timeout,
            8 => Self::ControlWake,
            9 => Self::PollCancel,
            10 => Self::TimeoutCancel,
            11 => Self::TcpTxCancel,
            _ => return None,
        })
    }
}

/// Generation-safe operation identity. It contains no Rust pointer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OpTag {
    pub kind: OpKind,
    pub slot: u32,
    pub generation: u32,
}

impl OpTag {
    pub fn new(kind: OpKind, slot: u32, generation: u32) -> Option<Self> {
        ((slot as usize) < MAX_TAG_SLOTS).then_some(Self {
            kind,
            slot,
            generation,
        })
    }

    /// Pack the tag for `io_uring_sqe::user_data`.
    pub fn encode(self) -> u64 {
        (u64::from(self.generation) << 32) | (u64::from(self.slot) << 8) | self.kind as u64
    }

    pub fn decode(value: u64) -> Option<Self> {
        let kind = OpKind::from_byte(value as u8)?;
        let slot = ((value >> 8) & 0x00ff_ffff) as u32;
        let generation = (value >> 32) as u32;
        Self::new(kind, slot, generation)
    }
}

// ---------------------------------------------------------------------------
// Bounded scheduling primitives
// ---------------------------------------------------------------------------

/// A fixed-capacity ready queue. A leaf can be queued at most once.
#[derive(Debug)]
pub struct ReadyQueue {
    entries: Box<[u32]>,
    queued: Box<[bool]>,
    head: usize,
    tail: usize,
    len: usize,
    max_depth: usize,
}

impl ReadyQueue {
    pub fn new(capacity: usize, leaf_capacity: usize) -> Result<Self, CapacityError> {
        if capacity == 0 || leaf_capacity == 0 {
            return Err(CapacityError::ReadyQueue {
                capacity,
                leaf_capacity,
            });
        }
        Ok(Self {
            entries: vec![0; capacity].into_boxed_slice(),
            queued: vec![false; leaf_capacity].into_boxed_slice(),
            head: 0,
            tail: 0,
            len: 0,
            max_depth: 0,
        })
    }

    pub fn enqueue(&mut self, slot: u32) -> bool {
        let slot_index = slot as usize;
        if slot_index >= self.queued.len() || self.queued[slot_index] {
            return false;
        }
        if self.len == self.entries.len() {
            return false;
        }
        debug_assert!(self.len < self.entries.len());
        self.entries[self.tail] = slot;
        self.tail = (self.tail + 1) % self.entries.len();
        self.len += 1;
        self.queued[slot_index] = true;
        self.max_depth = self.max_depth.max(self.len);
        true
    }

    pub fn pop(&mut self) -> Option<u32> {
        if self.len == 0 {
            return None;
        }
        let slot = self.entries[self.head];
        self.head = (self.head + 1) % self.entries.len();
        self.len -= 1;
        self.queued[slot as usize] = false;
        Some(slot)
    }

    /// Remove one queued slot. Removal is control-plane work and bounded by
    /// the configured ready capacity; the hot path remains O(1).
    pub fn remove(&mut self, slot: u32) -> bool {
        let slot_index = slot as usize;
        if slot_index >= self.queued.len() || !self.queued[slot_index] {
            return false;
        }

        let mut read = self.head;
        let mut write = self.head;
        let mut removed = false;
        for _ in 0..self.len {
            let current = self.entries[read];
            read = (read + 1) % self.entries.len();
            if current == slot && !removed {
                removed = true;
                continue;
            }
            self.entries[write] = current;
            write = (write + 1) % self.entries.len();
        }
        if removed {
            self.len -= 1;
            self.tail = write;
            self.queued[slot_index] = false;
        }
        removed
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn max_depth(&self) -> usize {
        self.max_depth
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkBudget {
    pub max_units: u32,
    pub max_bytes: u32,
    pub deadline: Instant,
}

impl WorkBudget {
    pub fn new(max_units: u32, max_bytes: u32, window: Duration) -> Self {
        Self {
            max_units,
            max_bytes,
            deadline: Instant::now() + window,
        }
    }

    pub fn exhausted(self, units: u32, bytes: u32) -> bool {
        units >= self.max_units || bytes >= self.max_bytes || Instant::now() >= self.deadline
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DeadlineEntry {
    pub slot: u32,
    pub generation: u32,
    pub at: Instant,
}

/// One live deadline per leaf, backed by a preallocated indexed min-heap.
#[derive(Debug)]
pub struct DeadlineIndex {
    heap: Vec<DeadlineEntry>,
    positions: Box<[usize]>,
}

impl DeadlineIndex {
    pub fn new(leaf_capacity: usize) -> Self {
        Self {
            heap: Vec::with_capacity(leaf_capacity),
            positions: vec![usize::MAX; leaf_capacity].into_boxed_slice(),
        }
    }

    pub fn set(&mut self, entry: DeadlineEntry) -> bool {
        let slot = entry.slot as usize;
        if slot >= self.positions.len() {
            return false;
        }
        let position = self.positions[slot];
        if position == usize::MAX {
            if self.heap.len() == self.heap.capacity() {
                return false;
            }
            self.positions[slot] = self.heap.len();
            self.heap.push(entry);
            self.sift_up(self.heap.len() - 1);
        } else {
            self.heap[position] = entry;
            self.sift_up(position);
            self.sift_down(self.positions[slot]);
        }
        true
    }

    pub fn remove(&mut self, slot: u32) -> Option<DeadlineEntry> {
        let slot_index = slot as usize;
        let position = *self.positions.get(slot_index)?;
        if position == usize::MAX {
            return None;
        }
        Some(self.remove_at(position))
    }

    pub fn next(&self) -> Option<DeadlineEntry> {
        self.heap.first().copied()
    }

    pub fn len(&self) -> usize {
        self.heap.len()
    }

    pub fn pop_due(&mut self, now: Instant) -> Option<DeadlineEntry> {
        (self.next()?.at <= now).then(|| self.remove_at(0))
    }

    pub fn is_empty(&self) -> bool {
        self.heap.is_empty()
    }

    fn remove_at(&mut self, position: usize) -> DeadlineEntry {
        let removed = self.heap.swap_remove(position);
        self.positions[removed.slot as usize] = usize::MAX;
        if position < self.heap.len() {
            self.positions[self.heap[position].slot as usize] = position;
            self.sift_up(position);
            self.sift_down(self.positions[self.heap[position].slot as usize]);
        }
        removed
    }

    fn sift_up(&mut self, mut position: usize) {
        while position > 0 {
            let parent = (position - 1) / 2;
            if self.heap[parent] <= self.heap[position] {
                break;
            }
            self.swap_heap(parent, position);
            position = parent;
        }
    }

    fn sift_down(&mut self, mut position: usize) {
        loop {
            let left = position * 2 + 1;
            let right = left + 1;
            let mut smallest = position;
            if left < self.heap.len() && self.heap[left] < self.heap[smallest] {
                smallest = left;
            }
            if right < self.heap.len() && self.heap[right] < self.heap[smallest] {
                smallest = right;
            }
            if smallest == position {
                break;
            }
            self.swap_heap(position, smallest);
            position = smallest;
        }
    }

    fn swap_heap(&mut self, left: usize, right: usize) {
        self.heap.swap(left, right);
        self.positions[self.heap[left].slot as usize] = left;
        self.positions[self.heap[right].slot as usize] = right;
    }
}

impl PartialOrd for DeadlineEntry {
    fn partial_cmp(&self, other: &Self) -> Option<CmpOrdering> {
        Some(self.cmp(other))
    }
}

impl Ord for DeadlineEntry {
    fn cmp(&self, other: &Self) -> CmpOrdering {
        self.at
            .cmp(&other.at)
            .then_with(|| self.slot.cmp(&other.slot))
    }
}

// ---------------------------------------------------------------------------
// Fixed pools and metrics
// ---------------------------------------------------------------------------

/// Fixed-size byte storage. `acquire`/`release` do not allocate.
#[derive(Debug)]
pub struct BufferPool {
    slots: Box<[Box<[u8]>]>,
    leased: Box<[bool]>,
    free: Vec<u32>,
}

impl BufferPool {
    pub fn new(slot_count: usize, slot_size: usize) -> Result<Self, CapacityError> {
        if slot_count == 0 || slot_size == 0 {
            return Err(CapacityError::Pool {
                slots: slot_count,
                slot_size,
            });
        }
        let mut slots = Vec::with_capacity(slot_count);
        let mut free = Vec::with_capacity(slot_count);
        for index in 0..slot_count {
            slots.push(vec![0; slot_size].into_boxed_slice());
            free.push(index as u32);
        }
        Ok(Self {
            slots: slots.into_boxed_slice(),
            leased: vec![false; slot_count].into_boxed_slice(),
            free,
        })
    }

    pub fn acquire(&mut self) -> Option<u32> {
        let slot = self.free.pop()?;
        self.leased[slot as usize] = true;
        Some(slot)
    }

    pub fn release(&mut self, slot: u32) -> bool {
        let slot_index = slot as usize;
        if slot_index >= self.slots.len() || !self.leased[slot_index] {
            return false;
        }
        self.leased[slot_index] = false;
        self.free.push(slot);
        true
    }

    pub fn available(&self) -> usize {
        self.free.len()
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slots.is_empty()
    }

    pub fn slot(&self, slot: u32) -> Option<&[u8]> {
        self.slots.get(slot as usize).map(Box::as_ref)
    }

    pub fn slot_mut(&mut self, slot: u32) -> Option<&mut [u8]> {
        self.slots.get_mut(slot as usize).map(Box::as_mut)
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct ShardMetrics {
    pub rx_packets: u64,
    pub rx_bytes: u64,
    pub tx_packets: u64,
    pub tx_bytes: u64,
    pub cqes: u64,
    pub sqes: u64,
    pub ready_visits: u64,
    pub budget_exhaustions: u64,
    pub stale_completions: u64,
    pub cq_overflows: u64,
    pub rx_pool_empty: u64,
    pub tx_pool_empty: u64,
    pub send_zc_attempts: u64,
    pub send_zc_fallbacks: u64,
    pub feed_overruns: u64,
    pub timers_processed: u64,
    pub loop_iterations: u64,
    pub max_ready_depth: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CapacityError {
    ReadyQueue {
        capacity: usize,
        leaf_capacity: usize,
    },
    Pool {
        slots: usize,
        slot_size: usize,
    },
    TooManyLeaves(usize),
    InvalidRingEntries(u32),
}

// ---------------------------------------------------------------------------
// Wake coalescing primitive
// ---------------------------------------------------------------------------

/// Dirty-bit primitive for one `(feed, shard)` wake stream.
pub struct WakeGate(AtomicBool);

impl WakeGate {
    pub fn new() -> Self {
        Self(AtomicBool::new(false))
    }

    /// Returns `true` only for the transition from clean to dirty.
    pub fn notify(&self) -> bool {
        !self.0.swap(true, Ordering::AcqRel)
    }

    pub fn clear(&self) {
        self.0.store(false, Ordering::Release);
    }

    pub fn is_dirty(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

impl Default for WakeGate {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Owner-thread synthetic shard
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy)]
pub struct ShardConfig {
    pub ring_entries: u32,
    pub max_leaves: usize,
    pub ready_capacity: usize,
    pub rx_slots: usize,
    pub tx_slots: usize,
    pub buffer_size: usize,
    pub mailbox_capacity: usize,
    pub leaf_budget: WorkBudgetConfig,
}

#[derive(Debug, Clone, Copy)]
pub struct WorkBudgetConfig {
    pub max_units: u32,
    pub max_bytes: u32,
    pub window: Duration,
}

impl Default for ShardConfig {
    fn default() -> Self {
        Self {
            ring_entries: 256,
            max_leaves: 4096,
            ready_capacity: 4096,
            rx_slots: 256,
            tx_slots: 256,
            buffer_size: 64 * 1024,
            mailbox_capacity: 1024,
            leaf_budget: WorkBudgetConfig {
                max_units: 32,
                max_bytes: 256 * 1024,
                window: Duration::from_micros(500),
            },
        }
    }
}

impl ShardConfig {
    fn validate(self) -> Result<Self, CapacityError> {
        if !self.ring_entries.is_power_of_two() || self.ring_entries < 8 {
            return Err(CapacityError::InvalidRingEntries(self.ring_entries));
        }
        if self.max_leaves == 0 || self.max_leaves >= MAX_TAG_SLOTS {
            return Err(CapacityError::TooManyLeaves(self.max_leaves));
        }
        ReadyQueue::new(self.ready_capacity, self.max_leaves)?;
        BufferPool::new(self.rx_slots, self.buffer_size)?;
        BufferPool::new(self.tx_slots, self.buffer_size)?;
        if self.mailbox_capacity == 0 {
            return Err(CapacityError::Pool {
                slots: self.mailbox_capacity,
                slot_size: 1,
            });
        }
        Ok(self)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkSnapshot {
    pub id: u64,
    pub generation: u32,
    pub visits: u64,
}

/// Control-plane identity for a dataplane-owned output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputRuntimeSpec {
    pub id: u64,
    pub generation: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardSnapshot {
    pub active_leaves: usize,
    pub ready_leaves: usize,
    pub deadline_count: usize,
    pub rx_available: usize,
    pub tx_available: usize,
    /// Jain fairness index for service visits, scaled by 1,000.
    pub jain_fairness_milli: u16,
    pub metrics: ShardMetrics,
    pub sinks: Vec<SinkSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    MailboxFull,
    Closed,
    Wake(io::ErrorKind),
    Shard(CapacityError),
    DeadlineCapacity,
    StaleGeneration,
}

struct SinkLeaf {
    id: u64,
    generation: u32,
    pending: bool,
    visits: u64,
}

struct LeafSlab {
    leaves: Box<[Option<SinkLeaf>]>,
    generations: Box<[u32]>,
    free: Vec<u32>,
}

impl LeafSlab {
    fn new(capacity: usize) -> Self {
        let mut free = Vec::with_capacity(capacity);
        for slot in (0..capacity as u32).rev() {
            free.push(slot);
        }
        Self {
            leaves: (0..capacity)
                .map(|_| None)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            generations: vec![0; capacity].into_boxed_slice(),
            free,
        }
    }

    fn add(&mut self, spec: OutputRuntimeSpec) -> Result<(u32, u32), CapacityError> {
        if self.leaves.iter().flatten().any(|leaf| leaf.id == spec.id) {
            return Err(CapacityError::TooManyLeaves(self.leaves.len()));
        }
        let slot = self
            .free
            .pop()
            .ok_or(CapacityError::TooManyLeaves(self.leaves.len()))?;
        let generation = self.generations[slot as usize];
        self.leaves[slot as usize] = Some(SinkLeaf {
            id: spec.id,
            generation: spec.generation.max(generation),
            pending: false,
            visits: 0,
        });
        let generation = self.leaves[slot as usize]
            .as_ref()
            .expect("new leaf remains live")
            .generation;
        Ok((slot, generation))
    }

    fn validate(&self, slot: u32, generation: u32) -> Result<usize, CommandError> {
        let slot_index = slot as usize;
        self.leaves
            .get(slot_index)
            .and_then(Option::as_ref)
            .filter(|leaf| leaf.generation == generation)
            .map(|_| slot_index)
            .ok_or(CommandError::StaleGeneration)
    }

    fn update(
        &mut self,
        slot: u32,
        expected_generation: u32,
        generation: u32,
    ) -> Result<(), CommandError> {
        let slot_index = self.validate(slot, expected_generation)?;
        if generation <= expected_generation {
            return Err(CommandError::StaleGeneration);
        }
        self.leaves[slot_index]
            .as_mut()
            .expect("validated leaf remains live")
            .generation = generation;
        Ok(())
    }

    fn remove_slot(&mut self, slot: u32) -> bool {
        let slot = slot as usize;
        if self.leaves.get(slot).and_then(Option::as_ref).is_none() {
            return false;
        }
        let generation = self.leaves[slot]
            .as_ref()
            .map_or(self.generations[slot], |leaf| leaf.generation);
        self.leaves[slot] = None;
        self.generations[slot] = generation.wrapping_add(1);
        self.free.push(slot as u32);
        true
    }
}

struct ShardState {
    leaves: LeafSlab,
    ready: ReadyQueue,
    deadlines: DeadlineIndex,
    rx: BufferPool,
    tx: TxPool,
    metrics: ShardMetrics,
    budget: WorkBudgetConfig,
}

impl ShardState {
    fn new(config: ShardConfig) -> Self {
        Self {
            leaves: LeafSlab::new(config.max_leaves),
            ready: ReadyQueue::new(config.ready_capacity, config.max_leaves).unwrap(),
            deadlines: DeadlineIndex::new(config.max_leaves),
            rx: BufferPool::new(config.rx_slots, config.buffer_size).unwrap(),
            tx: TxPool::new(config.tx_slots, config.buffer_size).unwrap(),
            metrics: ShardMetrics::default(),
            budget: config.leaf_budget,
        }
    }

    fn wake(&mut self, slot: u32, generation: u32) -> Result<bool, CommandError> {
        let slot_index = self.leaves.validate(slot, generation)?;
        if self.leaves.leaves[slot as usize]
            .as_ref()
            .is_some_and(|leaf| leaf.pending)
        {
            return Ok(true);
        }
        if !self.ready.enqueue(slot) {
            return Ok(false);
        }
        self.leaves.leaves[slot_index]
            .as_mut()
            .expect("ready slot remains live")
            .pending = true;
        Ok(true)
    }

    fn remove(&mut self, slot: u32, generation: u32) -> Result<bool, CommandError> {
        self.leaves.validate(slot, generation)?;
        self.ready.remove(slot);
        self.deadlines.remove(slot);
        Ok(self.leaves.remove_slot(slot))
    }

    fn remove_if_generation(&mut self, slot: u32, generation: u32) -> Result<bool, CommandError> {
        self.leaves.validate(slot, generation)?;
        self.ready.remove(slot);
        self.deadlines.remove(slot);
        Ok(self.leaves.remove_slot(slot))
    }

    fn update(
        &mut self,
        slot: u32,
        expected_generation: u32,
        generation: u32,
    ) -> Result<(), CommandError> {
        self.leaves.update(slot, expected_generation, generation)?;
        self.ready.remove(slot);
        self.deadlines.remove(slot);
        self.leaves.leaves[slot as usize]
            .as_mut()
            .expect("updated slot remains live")
            .pending = false;
        Ok(())
    }

    fn set_deadline(
        &mut self,
        slot: u32,
        generation: u32,
        at: Instant,
    ) -> Result<(), CommandError> {
        self.leaves.validate(slot, generation)?;
        if self.deadlines.set(DeadlineEntry {
            slot,
            generation,
            at,
        }) {
            Ok(())
        } else {
            Err(CommandError::DeadlineCapacity)
        }
    }

    fn process_due_deadlines(&mut self, now: Instant) {
        while let Some(entry) = self.deadlines.pop_due(now) {
            if self.wake(entry.slot, entry.generation).unwrap_or(false) {
                self.metrics.timers_processed = self.metrics.timers_processed.saturating_add(1);
            } else {
                let _ = self.deadlines.set(entry);
                break;
            }
        }
    }

    fn service_ready(&mut self, loop_deadline: Instant) {
        let mut units = 0;
        let mut bytes = 0;
        let budget = WorkBudget::new(
            self.budget.max_units,
            self.budget.max_bytes,
            self.budget.window,
        );
        while Instant::now() < loop_deadline {
            let Some(slot) = self.ready.pop() else {
                break;
            };
            if budget.exhausted(units, bytes) {
                self.metrics.budget_exhaustions += 1;
                let _ = self.ready.enqueue(slot);
                break;
            }
            let Some(leaf) = self.leaves.leaves[slot as usize].as_mut() else {
                continue;
            };
            leaf.pending = false;
            leaf.visits += 1;
            self.metrics.ready_visits += 1;
            if let Some(tx_lease) = self.tx.acquire() {
                let _ = self.tx.submit(tx_lease);
                let _ = self.tx.complete(tx_lease);
                let _ = self.tx.release(tx_lease);
                self.metrics.tx_packets += 1;
                self.metrics.tx_bytes += 1;
            } else {
                self.metrics.tx_pool_empty += 1;
            }
            units += 1;
            bytes += 1;
        }
        self.metrics.max_ready_depth = self
            .metrics
            .max_ready_depth
            .max(self.ready.max_depth() as u64);
    }

    fn snapshot(&self) -> ShardSnapshot {
        let visits: Vec<u64> = self
            .leaves
            .leaves
            .iter()
            .flatten()
            .map(|leaf| leaf.visits)
            .collect();
        let jain_fairness_milli = jain_fairness_milli(&visits);
        ShardSnapshot {
            active_leaves: self.leaves.leaves.iter().flatten().count(),
            ready_leaves: self.ready.len(),
            deadline_count: self.deadlines.len(),
            rx_available: self.rx.available(),
            tx_available: self.tx.available(),
            jain_fairness_milli,
            metrics: self.metrics,
            sinks: self
                .leaves
                .leaves
                .iter()
                .flatten()
                .map(|leaf| SinkSnapshot {
                    id: leaf.id,
                    generation: leaf.generation,
                    visits: leaf.visits,
                })
                .collect(),
        }
    }
}

pub fn jain_fairness_milli(visits: &[u64]) -> u16 {
    let count = visits.len() as u128;
    if count <= 1 {
        return 1_000;
    }
    let sum = visits.iter().map(|&value| u128::from(value)).sum::<u128>();
    if sum == 0 {
        return 1_000;
    }
    let sum_squared = visits
        .iter()
        .map(|&value| {
            let value = u128::from(value);
            value.saturating_mul(value)
        })
        .sum::<u128>();
    if sum_squared == 0 {
        return 1_000;
    }
    sum.saturating_mul(sum)
        .saturating_mul(1_000)
        .checked_div(count.saturating_mul(sum_squared))
        .unwrap_or(0)
        .min(1_000) as u16
}

enum Command {
    Add {
        id: u64,
        generation: u32,
        reply: SyncSender<Result<(u32, u32), CommandError>>,
    },
    Update {
        slot: u32,
        expected_generation: u32,
        generation: u32,
        reply: SyncSender<Result<(), CommandError>>,
    },
    SetDeadline {
        slot: u32,
        generation: u32,
        at: Instant,
        reply: SyncSender<Result<(), CommandError>>,
    },
    Remove {
        slot: u32,
        generation: u32,
        reply: SyncSender<Result<bool, CommandError>>,
    },
    RemoveIfGeneration {
        slot: u32,
        generation: u32,
        reply: SyncSender<Result<bool, CommandError>>,
    },
    Wake {
        slot: u32,
        generation: u32,
        reply: SyncSender<Result<bool, CommandError>>,
    },
    Snapshot {
        reply: SyncSender<ShardSnapshot>,
    },
    Shutdown {
        reply: SyncSender<ShardMetrics>,
    },
}

/// Handle for one native owner-thread shard.
pub struct Dataplane {
    commands: SyncSender<Command>,
    wake_fd: Arc<OwnedFd>,
    join: Option<JoinHandle<io::Result<ShardMetrics>>>,
}

/// Stable placement handle for one output owned by a dataplane shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputHandle {
    pub shard: usize,
    pub slot: u32,
    pub generation: u32,
}

/// Cold-path snapshot of all native owner shards.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DataplaneSnapshot {
    pub shards: Box<[ShardSnapshot]>,
}

/// A fixed set of single-owner native shards.
pub struct DataplaneHandle {
    shards: Box<[Dataplane]>,
}

impl DataplaneHandle {
    /// Start exactly `shard_count` owner threads. Placement is stable for an
    /// output's lifetime: the output id chooses its shard once and is never
    /// migrated by this handle.
    pub fn spawn(config: ShardConfig, shard_count: usize) -> io::Result<Self> {
        if shard_count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "dataplane requires at least one shard",
            ));
        }
        let mut shards = Vec::with_capacity(shard_count);
        for shard_index in 0..shard_count {
            shards.push(Dataplane::spawn_named(config, shard_index)?);
        }
        Ok(Self {
            shards: shards.into_boxed_slice(),
        })
    }

    pub fn shard_count(&self) -> usize {
        self.shards.len()
    }

    pub fn add_output(&self, id: u64) -> Result<OutputHandle, CommandError> {
        let shard = self.shard_for(id);
        let (slot, generation) = self.shards[shard].add_sink_with_generation(id, 0)?;
        Ok(OutputHandle {
            shard,
            slot,
            generation,
        })
    }

    pub fn add_output_spec(&self, spec: OutputRuntimeSpec) -> Result<OutputHandle, CommandError> {
        let shard = self.shard_for(spec.id);
        let (slot, generation) =
            self.shards[shard].add_sink_with_generation(spec.id, spec.generation)?;
        Ok(OutputHandle {
            shard,
            slot,
            generation,
        })
    }

    pub fn update_output(
        &self,
        handle: OutputHandle,
        generation: u32,
    ) -> Result<OutputHandle, CommandError> {
        self.shards
            .get(handle.shard)
            .ok_or(CommandError::StaleGeneration)?
            .update_sink(handle, generation)?;
        Ok(OutputHandle {
            generation,
            ..handle
        })
    }

    pub fn set_deadline(&self, handle: OutputHandle, at: Instant) -> Result<(), CommandError> {
        self.shards
            .get(handle.shard)
            .ok_or(CommandError::StaleGeneration)?
            .set_deadline(handle, at)
    }

    pub fn remove_output_if_generation(&self, handle: OutputHandle) -> Result<bool, CommandError> {
        self.shards
            .get(handle.shard)
            .ok_or(CommandError::StaleGeneration)?
            .remove_sink_if_generation(handle)
    }

    pub fn remove_output(&self, handle: OutputHandle) -> Result<bool, CommandError> {
        self.shards
            .get(handle.shard)
            .ok_or(CommandError::StaleGeneration)?
            .remove_sink(handle)
    }

    pub fn wake_output(&self, handle: OutputHandle) -> Result<bool, CommandError> {
        self.shards
            .get(handle.shard)
            .ok_or(CommandError::StaleGeneration)?
            .wake_sink(handle)
    }

    pub fn snapshot(&self) -> Result<DataplaneSnapshot, CommandError> {
        self.shards
            .iter()
            .map(Dataplane::snapshot)
            .collect::<Result<Vec<_>, _>>()
            .map(|snapshots| DataplaneSnapshot {
                shards: snapshots.into_boxed_slice(),
            })
    }

    pub fn shutdown(self) -> io::Result<Box<[ShardMetrics]>> {
        let mut metrics = Vec::with_capacity(self.shards.len());
        let mut first_error = None;
        for shard in self.shards {
            match shard.shutdown() {
                Ok(value) => metrics.push(value),
                Err(error) => {
                    first_error.get_or_insert(error);
                }
            }
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(metrics.into_boxed_slice()),
        }
    }

    fn shard_for(&self, id: u64) -> usize {
        (id as usize) % self.shards.len()
    }
}

impl Dataplane {
    pub fn spawn(config: ShardConfig) -> io::Result<Self> {
        Self::spawn_named(config, 0)
    }

    fn spawn_named(config: ShardConfig, shard_index: usize) -> io::Result<Self> {
        let config = config
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        let wake_fd = Arc::new(new_eventfd()?);
        let (commands, mailbox) = mpsc::sync_channel(config.mailbox_capacity);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let thread_wake_fd = Arc::clone(&wake_fd);
        let join = thread::Builder::new()
            .name(format!("restream-dataplane-{shard_index}"))
            .spawn(move || run_shard(config, mailbox, thread_wake_fd, startup_tx))?;
        match startup_rx.recv() {
            Ok(Ok(())) => Ok(Self {
                commands,
                wake_fd,
                join: Some(join),
            }),
            Ok(Err(error)) => {
                let _ = join.join();
                Err(error)
            }
            Err(_) => {
                let _ = join.join();
                Err(io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "dataplane startup failed",
                ))
            }
        }
    }

    pub fn add_sink(&self, id: u64) -> Result<(u32, u32), CommandError> {
        self.add_sink_with_generation(id, 0)
    }

    pub fn add_sink_with_generation(
        &self,
        id: u64,
        generation: u32,
    ) -> Result<(u32, u32), CommandError> {
        let (reply, result) = reply_channel();
        self.enqueue(Command::Add {
            id,
            generation,
            reply,
        })?;
        result.recv().map_err(|_| CommandError::Closed)?
    }

    pub fn update_sink(&self, handle: OutputHandle, generation: u32) -> Result<(), CommandError> {
        let (reply, result) = reply_channel();
        self.enqueue(Command::Update {
            slot: handle.slot,
            expected_generation: handle.generation,
            generation,
            reply,
        })?;
        result.recv().map_err(|_| CommandError::Closed)?
    }

    pub fn set_deadline(&self, handle: OutputHandle, at: Instant) -> Result<(), CommandError> {
        let (reply, result) = reply_channel();
        self.enqueue(Command::SetDeadline {
            slot: handle.slot,
            generation: handle.generation,
            at,
            reply,
        })?;
        result.recv().map_err(|_| CommandError::Closed)?
    }

    pub fn remove_sink(&self, handle: OutputHandle) -> Result<bool, CommandError> {
        let (reply, result) = reply_channel();
        self.enqueue(Command::Remove {
            slot: handle.slot,
            generation: handle.generation,
            reply,
        })?;
        result.recv().map_err(|_| CommandError::Closed)?
    }

    pub fn remove_sink_if_generation(&self, handle: OutputHandle) -> Result<bool, CommandError> {
        let (reply, result) = reply_channel();
        self.enqueue(Command::RemoveIfGeneration {
            slot: handle.slot,
            generation: handle.generation,
            reply,
        })?;
        result.recv().map_err(|_| CommandError::Closed)?
    }

    pub fn wake_sink(&self, handle: OutputHandle) -> Result<bool, CommandError> {
        let (reply, result) = reply_channel();
        self.enqueue(Command::Wake {
            slot: handle.slot,
            generation: handle.generation,
            reply,
        })?;
        result.recv().map_err(|_| CommandError::Closed)?
    }

    pub fn snapshot(&self) -> Result<ShardSnapshot, CommandError> {
        let (reply, result) = reply_channel();
        self.enqueue(Command::Snapshot { reply })?;
        result.recv().map_err(|_| CommandError::Closed)
    }

    pub fn shutdown(mut self) -> io::Result<ShardMetrics> {
        let (reply, result) = reply_channel();
        self.enqueue_blocking(Command::Shutdown { reply })?;
        let metrics = result
            .recv()
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "dataplane stopped"))?;
        self.join_thread()
            .and_then(|result| result.map(|_| metrics))
    }

    fn enqueue(&self, command: Command) -> Result<(), CommandError> {
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => CommandError::MailboxFull,
                TrySendError::Disconnected(_) => CommandError::Closed,
            })?;
        signal_eventfd(&self.wake_fd).map_err(|error| CommandError::Wake(error.kind()))
    }

    fn enqueue_blocking(&self, command: Command) -> io::Result<()> {
        self.commands
            .send(command)
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "dataplane stopped"))?;
        signal_eventfd(&self.wake_fd)
    }

    fn join_thread(&mut self) -> io::Result<io::Result<ShardMetrics>> {
        self.join
            .take()
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "dataplane already joined"))
            .map(|join| {
                join.join()
                    .unwrap_or_else(|_| Err(io::Error::other("dataplane panicked")))
            })
    }
}

impl Drop for Dataplane {
    fn drop(&mut self) {
        if self.join.is_none() {
            return;
        }
        let _ = self.enqueue_blocking(Command::Shutdown {
            reply: reply_channel().0,
        });
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

fn reply_channel<T>() -> (SyncSender<T>, Receiver<T>) {
    mpsc::sync_channel(1)
}

fn new_eventfd() -> io::Result<OwnedFd> {
    let fd = unsafe { libc::eventfd(0, libc::EFD_CLOEXEC | libc::EFD_NONBLOCK) };
    if fd < 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(unsafe { OwnedFd::from_raw_fd(fd) })
    }
}

fn signal_eventfd(fd: &OwnedFd) -> io::Result<()> {
    let value = 1_u64;
    let result = unsafe {
        libc::write(
            fd.as_raw_fd(),
            (&value as *const u64).cast::<libc::c_void>(),
            std::mem::size_of::<u64>(),
        )
    };
    if result == std::mem::size_of::<u64>() as isize
        || (result < 0 && io::Error::last_os_error().raw_os_error() == Some(libc::EAGAIN))
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn drain_eventfd(fd: &OwnedFd) {
    let mut value = 0_u64;
    loop {
        let result = unsafe {
            libc::read(
                fd.as_raw_fd(),
                (&mut value as *mut u64).cast::<libc::c_void>(),
                std::mem::size_of::<u64>(),
            )
        };
        if result != std::mem::size_of::<u64>() as isize {
            break;
        }
    }
}

#[derive(Debug, Clone, Copy)]
struct DeadlineTimer {
    at: Instant,
    generation: u32,
    cancel_pending: bool,
}

fn run_shard(
    config: ShardConfig,
    mailbox: Receiver<Command>,
    wake_fd: Arc<OwnedFd>,
    startup: SyncSender<Result<(), io::Error>>,
) -> io::Result<ShardMetrics> {
    let mut ring = build_ring(config.ring_entries)?;
    let mut files = FixedFileTable::new(1)?;
    files.register(&ring.submitter())?;
    let wake_file = files.install(&ring.submitter(), wake_fd.as_raw_fd())?;

    let mut state = ShardState::new(config);
    arm_control_poll(&mut ring, wake_file.index)?;
    ring.submit()?;
    let _ = startup.send(Ok(()));
    let mut poll_in_flight = true;
    let mut deadline_timer = None;
    let mut timer_generation = 0_u32;
    let result = loop {
        state.metrics.loop_iterations += 1;
        let mut stopping = false;
        {
            let cq = ring.completion();
            state.metrics.cq_overflows = u64::from(cq.overflow());
            for completion in cq {
                state.metrics.cqes += 1;
                let tag = OpTag::decode(completion.user_data());
                match tag {
                    Some(OpTag {
                        kind: OpKind::ControlWake,
                        ..
                    }) => {
                        poll_in_flight = false;
                        drain_eventfd(&wake_fd);
                    }
                    Some(OpTag {
                        kind: OpKind::Timeout,
                        generation,
                        ..
                    }) if deadline_timer
                        .is_some_and(|timer: DeadlineTimer| timer.generation == generation) =>
                    {
                        deadline_timer = None;
                    }
                    Some(OpTag {
                        kind: OpKind::TimeoutCancel,
                        generation,
                        ..
                    }) if deadline_timer
                        .is_some_and(|timer: DeadlineTimer| timer.generation == generation) =>
                    {
                        deadline_timer = None;
                    }
                    Some(tag) if tag.generation != 0 => {
                        state.metrics.stale_completions += 1;
                    }
                    _ => {}
                }
            }
        }

        loop {
            match mailbox.try_recv() {
                Ok(Command::Add {
                    id,
                    generation,
                    reply,
                }) => {
                    let result = state
                        .leaves
                        .add(OutputRuntimeSpec { id, generation })
                        .map_err(CommandError::Shard);
                    let _ = reply.send(result);
                }
                Ok(Command::Update {
                    slot,
                    expected_generation,
                    generation,
                    reply,
                }) => {
                    let _ = reply.send(state.update(slot, expected_generation, generation));
                }
                Ok(Command::SetDeadline {
                    slot,
                    generation,
                    at,
                    reply,
                }) => {
                    let _ = reply.send(state.set_deadline(slot, generation, at));
                }
                Ok(Command::Remove {
                    slot,
                    generation,
                    reply,
                }) => {
                    let removed = state.remove(slot, generation);
                    let _ = reply.send(removed);
                }
                Ok(Command::RemoveIfGeneration {
                    slot,
                    generation,
                    reply,
                }) => {
                    let removed = state.remove_if_generation(slot, generation);
                    let _ = reply.send(removed);
                }
                Ok(Command::Wake {
                    slot,
                    generation,
                    reply,
                }) => {
                    let woken = state.wake(slot, generation);
                    let _ = reply.send(woken);
                }
                Ok(Command::Snapshot { reply }) => {
                    let _ = reply.send(state.snapshot());
                }
                Ok(Command::Shutdown { reply }) => {
                    let _ = reply.send(state.metrics);
                    stopping = true;
                    break;
                }
                Err(TryRecvError::Empty | TryRecvError::Disconnected) => break,
            }
        }
        if stopping {
            break Ok(state.metrics);
        }

        state.service_ready(Instant::now() + config.leaf_budget.window);
        state.process_due_deadlines(Instant::now());
        state.service_ready(Instant::now() + config.leaf_budget.window);
        sync_deadline_timeout(
            &mut ring,
            state.deadlines.next().map(|entry| entry.at),
            &mut deadline_timer,
            &mut timer_generation,
        )?;
        if !poll_in_flight {
            arm_control_poll(&mut ring, wake_file.index)?;
            poll_in_flight = true;
        }
        state.metrics.sqes += ring.submit_and_wait(1)? as u64;
    };

    let mut shutdown_needs_wait = false;
    if let Some(timer) = deadline_timer.take() {
        if !timer.cancel_pending {
            cancel_deadline_timeout(&mut ring, timer.generation)?;
        }
        shutdown_needs_wait = true;
    }
    if poll_in_flight {
        cancel_control_poll(&mut ring)?;
        shutdown_needs_wait = true;
    }
    if shutdown_needs_wait {
        ring.submit_and_wait(1)?;
        for _ in ring.completion() {}
    }
    let _ = files.remove(&ring.submitter(), wake_file);
    let _ = ring.submitter().unregister_files();
    result
}

fn build_ring(entries: u32) -> io::Result<IoUring> {
    let mut builder = IoUring::builder();
    builder
        .setup_single_issuer()
        .setup_defer_taskrun()
        .setup_coop_taskrun()
        .setup_taskrun_flag()
        .setup_cqsize(entries.saturating_mul(2));
    match builder.build(entries) {
        Ok(ring) => Ok(ring),
        Err(_) => IoUring::new(entries),
    }
}

fn arm_control_poll(ring: &mut IoUring, file_index: u32) -> io::Result<()> {
    let entry = opcode::PollAdd::new(types::Fixed(file_index), libc::POLLIN as _)
        .build()
        .user_data(OpTag::new(OpKind::ControlWake, 0, 0).unwrap().encode());
    unsafe {
        ring.submission()
            .push(&entry)
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full"))?;
    }
    Ok(())
}

fn cancel_control_poll(ring: &mut IoUring) -> io::Result<()> {
    let target = OpTag::new(OpKind::ControlWake, 0, 0).unwrap().encode();
    let entry = opcode::PollRemove::new(target)
        .build()
        .user_data(OpTag::new(OpKind::PollCancel, 0, 0).unwrap().encode());
    unsafe {
        ring.submission()
            .push(&entry)
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full"))?;
    }
    Ok(())
}

fn sync_deadline_timeout(
    ring: &mut IoUring,
    next_at: Option<Instant>,
    timer: &mut Option<DeadlineTimer>,
    next_generation: &mut u32,
) -> io::Result<()> {
    match (*timer, next_at) {
        (None, None) => {}
        (None, Some(at)) => {
            *next_generation = next_generation.wrapping_add(1).max(1);
            arm_deadline_timeout(ring, at, *next_generation)?;
            *timer = Some(DeadlineTimer {
                at,
                generation: *next_generation,
                cancel_pending: false,
            });
        }
        (Some(active), _) if active.cancel_pending => {}
        (Some(active), Some(at)) if active.at == at => {}
        (Some(active), _) => {
            cancel_deadline_timeout(ring, active.generation)?;
            *timer = Some(DeadlineTimer {
                cancel_pending: true,
                ..active
            });
        }
    }
    Ok(())
}

fn arm_deadline_timeout(ring: &mut IoUring, at: Instant, generation: u32) -> io::Result<()> {
    let timespec = types::Timespec::from(at.saturating_duration_since(Instant::now()));
    let entry = opcode::Timeout::new(&timespec).build().user_data(
        OpTag::new(OpKind::Timeout, 0, generation)
            .expect("deadline timer generation is a valid tag")
            .encode(),
    );
    unsafe {
        ring.submission()
            .push(&entry)
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full"))?;
    }
    Ok(())
}

fn cancel_deadline_timeout(ring: &mut IoUring, generation: u32) -> io::Result<()> {
    let target = OpTag::new(OpKind::Timeout, 0, generation)
        .expect("deadline timer generation is a valid tag")
        .encode();
    let entry = opcode::TimeoutRemove::new(target).build().user_data(
        OpTag::new(OpKind::TimeoutCancel, 0, generation)
            .expect("deadline timer generation is a valid tag")
            .encode(),
    );
    unsafe {
        ring.submission()
            .push(&entry)
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full"))?;
    }
    Ok(())
}

impl std::fmt::Display for CapacityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CapacityError {}

impl std::fmt::Display for CommandError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}

impl std::error::Error for CommandError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn operation_tags_round_trip_and_reject_invalid_kind() {
        let tag = OpTag::new(OpKind::TcpTx, 0x00ab_cdef, u32::MAX).unwrap();
        assert_eq!(OpTag::decode(tag.encode()), Some(tag));
        assert!(OpTag::decode(0x0d).is_none());
        assert!(OpTag::new(OpKind::TcpTx, MAX_TAG_SLOTS as u32, 0).is_none());
    }

    #[test]
    fn ready_queue_deduplicates_without_population_scan() {
        let mut queue = ReadyQueue::new(4, 4).unwrap();
        assert!(queue.enqueue(2));
        assert!(!queue.enqueue(2));
        assert!(queue.enqueue(0));
        assert_eq!(queue.pop(), Some(2));
        assert!(queue.enqueue(2));
        assert_eq!(queue.pop(), Some(0));
        assert_eq!(queue.pop(), Some(2));
        assert!(queue.is_empty());
    }

    #[test]
    fn ready_queue_removes_a_pending_slot_before_reuse() {
        let mut queue = ReadyQueue::new(3, 3).unwrap();
        assert!(queue.enqueue(0));
        assert!(queue.enqueue(1));
        assert!(queue.remove(0));
        assert!(!queue.remove(0));
        assert!(queue.enqueue(0));
        assert_eq!(queue.pop(), Some(1));
        assert_eq!(queue.pop(), Some(0));
        assert!(queue.is_empty());
    }

    #[test]
    fn removed_pending_leaf_does_not_wake_a_reused_slot() {
        let mut state = ShardState::new(ShardConfig {
            max_leaves: 1,
            ready_capacity: 1,
            ..ShardConfig::default()
        });
        let first = state
            .leaves
            .add(OutputRuntimeSpec {
                id: 7,
                generation: 0,
            })
            .unwrap();
        assert!(state.wake(first.0, first.1).unwrap());
        assert!(state.remove(first.0, first.1).unwrap());
        let second = state
            .leaves
            .add(OutputRuntimeSpec {
                id: 8,
                generation: 0,
            })
            .unwrap();
        state.service_ready(Instant::now() + Duration::from_millis(1));
        assert_eq!(state.leaves.leaves[0].as_ref().unwrap().visits, 0);
        assert_eq!(
            state.wake(first.0, first.1),
            Err(CommandError::StaleGeneration)
        );
        assert!(state.wake(second.0, second.1).unwrap());
        state.service_ready(Instant::now() + Duration::from_millis(1));
        assert_eq!(state.leaves.leaves[0].as_ref().unwrap().visits, 1);
    }

    #[test]
    fn wake_failure_does_not_poison_a_leaf_when_ready_is_full() {
        let mut state = ShardState::new(ShardConfig {
            max_leaves: 3,
            ready_capacity: 2,
            ..ShardConfig::default()
        });
        let handles = (1..=3)
            .map(|id| {
                state
                    .leaves
                    .add(OutputRuntimeSpec { id, generation: 0 })
                    .unwrap()
            })
            .collect::<Vec<_>>();
        assert!(state.wake(handles[0].0, handles[0].1).unwrap());
        assert!(state.wake(handles[1].0, handles[1].1).unwrap());
        assert!(!state.wake(handles[2].0, handles[2].1).unwrap());

        // Free one queue slot without servicing the second leaf. A retry for
        // the third leaf must now be admitted.
        assert!(state.remove(handles[0].0, handles[0].1).unwrap());
        assert!(state.wake(handles[2].0, handles[2].1).unwrap());
    }

    #[test]
    fn update_rejects_stale_generation_and_clears_pending_work() {
        let mut state = ShardState::new(ShardConfig {
            max_leaves: 1,
            ready_capacity: 1,
            ..ShardConfig::default()
        });
        let handle = state
            .leaves
            .add(OutputRuntimeSpec {
                id: 7,
                generation: 4,
            })
            .unwrap();
        assert!(state.wake(handle.0, handle.1).unwrap());
        assert_eq!(
            state.update(handle.0, handle.1, 3),
            Err(CommandError::StaleGeneration)
        );
        assert_eq!(state.ready.len(), 1);
        assert!(state.update(handle.0, handle.1, 5).is_ok());
        assert_eq!(state.ready.len(), 0);
        assert_eq!(state.leaves.leaves[0].as_ref().unwrap().generation, 5);
    }

    #[test]
    fn deadline_index_updates_and_expires_in_order() {
        let now = Instant::now();
        let mut deadlines = DeadlineIndex::new(3);
        assert!(deadlines.set(DeadlineEntry {
            slot: 0,
            generation: 1,
            at: now + Duration::from_millis(20),
        }));
        assert!(deadlines.set(DeadlineEntry {
            slot: 1,
            generation: 1,
            at: now + Duration::from_millis(10),
        }));
        assert!(deadlines.set(DeadlineEntry {
            slot: 0,
            generation: 2,
            at: now,
        }));
        assert_eq!(deadlines.pop_due(now).unwrap().slot, 0);
        assert_eq!(deadlines.next().unwrap().slot, 1);
        assert_eq!(deadlines.remove(1).unwrap().generation, 1);
        assert_eq!(deadlines.len(), 0);
    }

    #[test]
    fn pools_exhaust_and_recover_without_growing() {
        let mut pool = BufferPool::new(2, 16).unwrap();
        let first = pool.acquire().unwrap();
        let second = pool.acquire().unwrap();
        assert!(pool.acquire().is_none());
        assert!(pool.release(first));
        assert!(!pool.release(first));
        assert!(pool.release(second));
        assert_eq!(pool.available(), 2);
    }

    #[cfg(not(feature = "loom"))]
    #[test]
    fn wake_gate_coalesces_notifications() {
        let gate = WakeGate::new();
        assert!(gate.notify());
        assert!(!gate.notify());
        gate.clear();
        assert!(gate.notify());
    }

    #[cfg(feature = "loom")]
    #[test]
    fn wake_gate_coalesces_notifications_in_model() {
        loom::model(|| {
            let gate = WakeGate::new();
            assert!(gate.notify());
            assert!(!gate.notify());
            gate.clear();
            assert!(gate.notify());
        });
    }

    #[test]
    fn shard_config_rejects_unbounded_or_invalid_shapes() {
        assert!(
            ShardConfig {
                ring_entries: 3,
                ..ShardConfig::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            ShardConfig {
                ready_capacity: 0,
                ..ShardConfig::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn shard_snapshot_reports_bounded_pool_and_schedule_depths() {
        let state = ShardState::new(ShardConfig::default());
        let snapshot = state.snapshot();
        assert_eq!(snapshot.active_leaves, 0);
        assert_eq!(snapshot.ready_leaves, 0);
        assert_eq!(snapshot.deadline_count, 0);
        assert_eq!(snapshot.rx_available, 256);
        assert_eq!(snapshot.tx_available, 256);
        assert_eq!(snapshot.jain_fairness_milli, 1_000);
        assert_eq!(snapshot.metrics.cq_overflows, 0);
    }

    #[test]
    fn jain_fairness_reports_service_imbalance() {
        assert_eq!(jain_fairness_milli(&[10, 10, 10]), 1_000);
        assert_eq!(jain_fairness_milli(&[2, 1]), 900);
        assert_eq!(jain_fairness_milli(&[0, 0]), 1_000);
    }
}
