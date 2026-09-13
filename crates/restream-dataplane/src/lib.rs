//! Small Linux-native dataplane primitives.
//!
//! The first slice deliberately owns no product protocol. It proves the
//! execution model: one owner thread, one `io_uring`, bounded control input,
//! generation-safe operation tags, fixed ready/deadline storage, and fixed
//! RX/TX pools. Protocol leaves can be added without changing that ownership
//! contract.

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

pub mod media;
pub mod tcp;
pub mod udp;

pub use media::{CursorError, FeedCursor, MediaArena, MediaError, MediaRef, MediaRing};

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
    UdpRx = 5,
    UdpTx = 6,
    Timeout = 7,
    ControlWake = 8,
    PollCancel = 9,
}

impl OpKind {
    fn from_byte(value: u8) -> Option<Self> {
        Some(match value {
            1 => Self::Accept,
            2 => Self::Connect,
            3 => Self::TcpRx,
            4 => Self::TcpTx,
            5 => Self::UdpRx,
            6 => Self::UdpTx,
            7 => Self::Timeout,
            8 => Self::ControlWake,
            9 => Self::PollCancel,
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
        if capacity == 0 || leaf_capacity == 0 || capacity < leaf_capacity {
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
        debug_assert!(self.len < self.entries.len());
        if self.len == self.entries.len() {
            return false;
        }
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

    pub fn pop_due(&mut self, now: Instant) -> Option<DeadlineEntry> {
        (self.next()?.at <= now).then(|| self.remove_at(0))
    }

    pub fn len(&self) -> usize {
        self.heap.len()
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
    pub rx_pool_empty: u64,
    pub tx_pool_empty: u64,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardSnapshot {
    pub active_leaves: usize,
    pub ready_leaves: usize,
    pub metrics: ShardMetrics,
    pub sinks: Vec<SinkSnapshot>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandError {
    MailboxFull,
    Closed,
    Wake(io::ErrorKind),
    Shard(CapacityError),
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

    fn add(&mut self, id: u64) -> Result<u32, CapacityError> {
        if self.leaves.iter().flatten().any(|leaf| leaf.id == id) {
            return Err(CapacityError::TooManyLeaves(self.leaves.len()));
        }
        let slot = self
            .free
            .pop()
            .ok_or(CapacityError::TooManyLeaves(self.leaves.len()))?;
        let generation = self.generations[slot as usize];
        self.leaves[slot as usize] = Some(SinkLeaf {
            id,
            generation,
            pending: false,
            visits: 0,
        });
        Ok(slot)
    }

    fn remove(&mut self, id: u64) -> bool {
        let Some((slot, _)) = self
            .leaves
            .iter()
            .enumerate()
            .find(|(_, leaf)| leaf.as_ref().is_some_and(|leaf| leaf.id == id))
        else {
            return false;
        };
        self.leaves[slot] = None;
        self.generations[slot] = self.generations[slot].wrapping_add(1);
        self.free.push(slot as u32);
        true
    }

    fn find_mut(&mut self, id: u64) -> Option<(u32, &mut SinkLeaf)> {
        self.leaves.iter_mut().enumerate().find_map(|(slot, leaf)| {
            leaf.as_mut()
                .filter(|leaf| leaf.id == id)
                .map(|leaf| (slot as u32, leaf))
        })
    }
}

struct ShardState {
    leaves: LeafSlab,
    ready: ReadyQueue,
    deadlines: DeadlineIndex,
    _rx: BufferPool,
    tx: BufferPool,
    metrics: ShardMetrics,
    budget: WorkBudgetConfig,
}

impl ShardState {
    fn new(config: ShardConfig) -> Self {
        Self {
            leaves: LeafSlab::new(config.max_leaves),
            ready: ReadyQueue::new(config.ready_capacity, config.max_leaves).unwrap(),
            deadlines: DeadlineIndex::new(config.max_leaves),
            _rx: BufferPool::new(config.rx_slots, config.buffer_size).unwrap(),
            tx: BufferPool::new(config.tx_slots, config.buffer_size).unwrap(),
            metrics: ShardMetrics::default(),
            budget: config.leaf_budget,
        }
    }

    fn wake(&mut self, id: u64) -> bool {
        let Some((slot, leaf)) = self.leaves.find_mut(id) else {
            return false;
        };
        if leaf.pending {
            return true;
        }
        leaf.pending = true;
        self.ready.enqueue(slot)
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
            if let Some(tx_slot) = self.tx.acquire() {
                let _ = self.tx.release(tx_slot);
                self.metrics.tx_packets += 1;
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
        ShardSnapshot {
            active_leaves: self.leaves.leaves.iter().flatten().count(),
            ready_leaves: self.ready.len(),
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

enum Command {
    Add {
        id: u64,
        reply: SyncSender<Result<(), CommandError>>,
    },
    Remove {
        id: u64,
        reply: SyncSender<bool>,
    },
    Wake {
        id: u64,
        reply: SyncSender<bool>,
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

impl Dataplane {
    pub fn spawn(config: ShardConfig) -> io::Result<Self> {
        let config = config
            .validate()
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, format!("{error:?}")))?;
        let wake_fd = Arc::new(new_eventfd()?);
        let (commands, mailbox) = mpsc::sync_channel(config.mailbox_capacity);
        let (startup_tx, startup_rx) = mpsc::sync_channel(1);
        let thread_wake_fd = Arc::clone(&wake_fd);
        let join = thread::Builder::new()
            .name("restream-dataplane-0".to_owned())
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

    pub fn add_sink(&self, id: u64) -> Result<(), CommandError> {
        let (reply, result) = reply_channel();
        self.enqueue(Command::Add { id, reply })?;
        result.recv().map_err(|_| CommandError::Closed)?
    }

    pub fn remove_sink(&self, id: u64) -> Result<bool, CommandError> {
        let (reply, result) = reply_channel();
        self.enqueue(Command::Remove { id, reply })?;
        result.recv().map_err(|_| CommandError::Closed)
    }

    pub fn wake_sink(&self, id: u64) -> Result<bool, CommandError> {
        let (reply, result) = reply_channel();
        self.enqueue(Command::Wake { id, reply })?;
        result.recv().map_err(|_| CommandError::Closed)
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
        signal_eventfd(&self.wake_fd).map_err(|error| CommandError::Wake(error.kind()))?;
        self.commands
            .try_send(command)
            .map_err(|error| match error {
                TrySendError::Full(_) => CommandError::MailboxFull,
                TrySendError::Disconnected(_) => CommandError::Closed,
            })
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
    if result == std::mem::size_of::<u64>() as isize {
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

fn run_shard(
    config: ShardConfig,
    mailbox: Receiver<Command>,
    wake_fd: Arc<OwnedFd>,
    startup: SyncSender<Result<(), io::Error>>,
) -> io::Result<ShardMetrics> {
    let mut ring = build_ring(config.ring_entries)?;
    ring.submitter().register_files(&[wake_fd.as_raw_fd()])?;

    let mut state = ShardState::new(config);
    arm_control_poll(&mut ring)?;
    ring.submit()?;
    let _ = startup.send(Ok(()));
    let mut poll_in_flight = true;
    let result = loop {
        state.metrics.loop_iterations += 1;
        let mut stopping = false;
        {
            let cq = ring.completion();
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
                    Some(tag) if tag.generation != 0 => {
                        state.metrics.stale_completions += 1;
                    }
                    _ => {}
                }
            }
        }

        loop {
            match mailbox.try_recv() {
                Ok(Command::Add { id, reply }) => {
                    let result = state
                        .leaves
                        .add(id)
                        .map(|_| ())
                        .map_err(CommandError::Shard);
                    let _ = reply.send(result);
                }
                Ok(Command::Remove { id, reply }) => {
                    let removed = state.leaves.remove(id);
                    let _ = reply.send(removed);
                }
                Ok(Command::Wake { id, reply }) => {
                    let woken = state.wake(id);
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

        while state.deadlines.pop_due(Instant::now()).is_some() {}
        state.service_ready(Instant::now() + config.leaf_budget.window);
        if !poll_in_flight {
            arm_control_poll(&mut ring)?;
            poll_in_flight = true;
        }
        state.metrics.sqes += ring.submit_and_wait(1)? as u64;
    };

    let _ = ring.submitter().unregister_files();
    result
}

fn build_ring(entries: u32) -> io::Result<IoUring> {
    let mut builder = IoUring::builder();
    builder
        .setup_single_issuer()
        .setup_defer_taskrun()
        .setup_coop_taskrun()
        .setup_taskrun_flag();
    match builder.build(entries) {
        Ok(ring) => Ok(ring),
        Err(_) => IoUring::new(entries),
    }
}

fn arm_control_poll(ring: &mut IoUring) -> io::Result<()> {
    let entry = opcode::PollAdd::new(types::Fixed(0), libc::POLLIN as _)
        .build()
        .user_data(OpTag::new(OpKind::ControlWake, 0, 0).unwrap().encode());
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
        assert!(OpTag::decode(0x0a).is_none());
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
                ready_capacity: 1,
                ..ShardConfig::default()
            }
            .validate()
            .is_err()
        );
    }
}
