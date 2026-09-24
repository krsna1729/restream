//! SRT egress shard backend: common Restream feed/scheduler/lifecycle state
//! over the shard's Compio `Owner`s.
//!
//! Topology: one shard OS thread owns one Compio runtime and at most one
//! `srt_transport::compio::Owner` per address family (`owner_set`). A leaf is
//! product state only: its `OutputId`/generation, feed cursor, engine, and
//! the `SrtCaller` (family + `LogicalCallerId`) it sends through. There is no
//! per-output socket, task or Owner, and no Restream-owned caller table, TX
//! pool or UDP driver.
//!
//! Scheduling: a *ready batch* services each existing Owner once under a
//! finite budget, drains bounded Owner event queues, moves queued candidates
//! into the ready queue, and then visits those leaves one per `on_ready`
//! (the generic shard's readiness budget is the fairness boundary). Payload
//! submission (`send_shared`) only enqueues into protocol state; it never
//! services an Owner. Backpressured leaves park in `blocked` and are
//! re-examined at a bounded rate on Owner activity, never in a spin.

use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::time::{Duration, Instant};

use srt_transport::advanced::caller::{PoolOutcome, PoolRequestId};

use crate::media::egress::backend::Readiness;
use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::metrics::ShardMetrics;
use crate::media::egress::policy::{LeafLimits, WorkBudgetConfig};
use crate::media::egress::scheduler::{LeafKey, VisitDecision};
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardCommandEffect, EgressShardIdleWake,
};
use crate::media::egress::visit::EngineVisitResult;
use crate::media::srt::{AddressFamily, SrtFabricEgressConnectSpec, SrtSendBacklog};

pub(crate) mod owner_set;
pub(crate) mod resolve_runtime;
#[path = "srt_drain.rs"]
mod srt_drain;
#[path = "srt_events.rs"]
mod srt_events;
#[path = "srt_leaf.rs"]
mod srt_leaf;

pub(crate) use owner_set::{SrtCaller, SrtOwnerSettings, SrtOwners};
pub(crate) use srt_leaf::SrtFabricLeaf;

/// Blocked (backpressured or connecting) leaves re-examined per ready batch.
/// A fixed rotating window: the cost of a network wake is bounded by this, not
/// by how many leaves are parked.
const BLOCKED_RECHECK_PER_BATCH: usize = 16;

/// Combined application and transport pending state for one SRT leaf.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SrtLeafPressure {
    pub app_pending_bytes: usize,
    pub backlog: Option<SrtSendBacklog>,
}

impl SrtLeafPressure {
    /// Bytes charged against the leaf memory envelope: retained application
    /// message plus unacknowledged protocol sender-buffer bytes.
    pub(crate) fn pending_bytes(&self) -> u64 {
        self.app_pending_bytes as u64 + self.backlog.map_or(0, |backlog| backlog.bytes)
    }

    /// True when data is waiting anywhere on the send path. A leaf with a
    /// drained application queue but a saturated sender buffer is
    /// backpressured, not idle.
    pub(crate) fn is_backpressured(&self) -> bool {
        self.pending_bytes() > 0
    }
}

pub(crate) fn apply_send_backlog(
    quality: &mut crate::media::snapshots::PublisherQuality,
    backlog: SrtSendBacklog,
) {
    quality.srt_send_buf_bytes = i32::try_from(backlog.bytes.min(i32::MAX as u64)).ok();
    quality.ms_send_buf = Some(f64::from(backlog.ms));
    quality.srt_flight_size_pkts = i32::try_from(backlog.packets.min(i32::MAX as u32)).ok();
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SrtResolvedConnect {
    pub(crate) output_id: OutputId,
    pub(crate) generation: u64,
    pub(crate) peer_addrs: Vec<SocketAddr>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SrtResolveRequest {
    pub(crate) output_id: OutputId,
    pub(crate) generation: u64,
    pub(crate) peer_hosts: Vec<String>,
}

impl SrtResolveRequest {
    pub(crate) fn new(output_id: OutputId, generation: u64, peer_hosts: Vec<String>) -> Self {
        Self {
            output_id,
            generation,
            peer_hosts,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SrtResolveWorkerError {
    EmptyPeerList,
    ResolveFailed { host: String },
    CompletionQueueFull,
    CompletionQueueClosed,
}

/// A leaf due a visit, carrying the generation it was scheduled under so a
/// slot reused by a replacement output can never be visited on its behalf.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SrtReadyLeaf {
    key: LeafKey,
    generation: u64,
}

#[derive(Debug)]
pub(crate) struct SrtResolveCompletionQueue {
    receiver: Receiver<SrtResolvedConnect>,
}

pub(crate) fn srt_resolve_completion_queue(
    capacity: usize,
) -> (SyncSender<SrtResolvedConnect>, SrtResolveCompletionQueue) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    (sender, SrtResolveCompletionQueue { receiver })
}

fn resolve_srt_peer_hosts(
    request: SrtResolveRequest,
    completion_sender: SyncSender<SrtResolvedConnect>,
) -> Result<(), SrtResolveWorkerError> {
    if request.peer_hosts.is_empty() {
        return Err(SrtResolveWorkerError::EmptyPeerList);
    }

    let mut peer_addrs = Vec::with_capacity(request.peer_hosts.len());
    for host in &request.peer_hosts {
        let addr = resolve_srt_peer_host(host)
            .ok_or_else(|| SrtResolveWorkerError::ResolveFailed { host: host.clone() })?;
        peer_addrs.push(addr);
    }

    completion_sender
        .try_send(SrtResolvedConnect {
            output_id: request.output_id,
            generation: request.generation,
            peer_addrs,
        })
        .map_err(|error| match error {
            TrySendError::Full(_) => SrtResolveWorkerError::CompletionQueueFull,
            TrySendError::Disconnected(_) => SrtResolveWorkerError::CompletionQueueClosed,
        })
}

fn resolve_srt_peer_host(host: &str) -> Option<SocketAddr> {
    host.parse::<SocketAddr>()
        .ok()
        .or_else(|| host.to_socket_addrs().ok()?.next())
}

impl SrtResolveCompletionQueue {
    pub(crate) fn drain_resolved(&mut self, resolved: &mut Vec<SrtResolvedConnect>) {
        loop {
            match self.receiver.try_recv() {
                Ok(completion) => resolved.push(completion),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }
}

/// Where a not-yet-live output is in its connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PendingStage {
    /// DNS has not completed.
    Resolving,
    /// Handed to the family Owner's caller pool and waiting for a permit.
    Queued {
        family: AddressFamily,
        request_id: PoolRequestId,
    },
}

struct PendingSrtConnect {
    common: LeafCommon,
    connect_spec: SrtFabricEgressConnectSpec,
    stage: PendingStage,
}

/// Correlation of a queued Owner request back to the output that made it. A
/// stale entry (output removed/replaced) is detected by generation and stage
/// when the Owner later admits it; the admission is then retired.
struct QueuedRequest {
    output_id: OutputId,
    generation: u64,
}

pub(crate) struct SrtShardBackend {
    resolve_completions: SrtResolveCompletionQueue,
    resolved_connects: Vec<SrtResolvedConnect>,
    feed: TsFeed,
    /// Per-visit limits and window remain valid throughout shard startup;
    /// each visit receives a fresh absolute deadline.
    budget_config: WorkBudgetConfig,
    /// The shard's Compio runtime and family Owners (`!Send`, thread-affine).
    owners: SrtOwners,
    leaves: Vec<Option<SrtFabricLeaf>>,
    free_leaf_keys: Vec<LeafKey>,
    output_sockets: HashMap<OutputId, LeafKey>,
    /// Exact attribution: transport identity -> live leaf. A logical caller
    /// id is never reused by an Owner, so an old id can never reach a
    /// replacement leaf that reuses the slot.
    callers: HashMap<SrtCaller, LeafKey>,
    queued_requests: HashMap<(AddressFamily, PoolRequestId), QueuedRequest>,
    ready: VecDeque<SrtReadyLeaf>,
    ready_candidates: VecDeque<LeafKey>,
    feed_waiting: VecDeque<LeafKey>,
    blocked: VecDeque<LeafKey>,
    stall_candidates: VecDeque<LeafKey>,
    pending_connects: HashMap<OutputId, PendingSrtConnect>,
    event_scratch: Vec<owner_set::SrtOwnerEvent>,
    last_stall_sweep: Option<Instant>,
    /// Leaves get `drain_timeout` minus the Owner-teardown reserve to flush,
    /// so shutdown stays inside the generic shard drain deadline.
    drain_timeout: Duration,
    owner_shutdown_reserve: Duration,
    resync_count: u64,
    budget_exhaustions: u64,
    queue_overflows: u64,
    /// Ready batches begun (one Owner service per family per batch).
    batches: u64,
    /// Leaves visited (one `on_ready` visits at most one).
    leaf_visits: u64,
    /// Owner events dropped because a family/leaf was already gone.
    stale_events: u64,
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, capacity: usize) -> bool {
    if queue.len() >= capacity {
        return false;
    }
    queue.push_back(value);
    true
}

impl SrtShardBackend {
    pub(crate) fn with_runtime_components(
        feed: TsFeed,
        budget: WorkBudgetConfig,
        resolve_completions: SrtResolveCompletionQueue,
        owners: SrtOwners,
    ) -> Self {
        let capacity = crate::media::egress::shard::EgressShardConfig::DEFAULT_LEAF_CAPACITY;
        let mut backend = Self {
            resolve_completions,
            resolved_connects: Vec::with_capacity(1024),
            feed,
            budget_config: budget,
            owners,
            leaves: Vec::new(),
            free_leaf_keys: Vec::new(),
            output_sockets: HashMap::new(),
            callers: HashMap::new(),
            queued_requests: HashMap::new(),
            ready: VecDeque::new(),
            ready_candidates: VecDeque::new(),
            feed_waiting: VecDeque::new(),
            blocked: VecDeque::new(),
            stall_candidates: VecDeque::new(),
            pending_connects: HashMap::new(),
            event_scratch: Vec::with_capacity(256),
            last_stall_sweep: None,
            drain_timeout: crate::media::egress::shard::EgressShardConfig::DEFAULT_DRAIN_TIMEOUT,
            owner_shutdown_reserve: owner_set::OWNER_SHUTDOWN_RESERVE,
            resync_count: 0,
            budget_exhaustions: 0,
            queue_overflows: 0,
            batches: 0,
            leaf_visits: 0,
            stale_events: 0,
        };
        backend.size_queues(capacity);
        backend
    }

    fn size_queues(&mut self, capacity: usize) {
        self.leaves = (0..capacity).map(|_| None).collect();
        self.free_leaf_keys = (0..capacity as u32)
            .rev()
            .map(|slot| LeafKey(slot as usize))
            .collect();
        self.ready = VecDeque::with_capacity(capacity);
        self.ready_candidates = VecDeque::with_capacity(capacity);
        self.feed_waiting = VecDeque::with_capacity(capacity);
        self.blocked = VecDeque::with_capacity(capacity);
        self.stall_candidates = VecDeque::with_capacity(capacity);
    }

    pub(crate) fn with_leaf_capacity(mut self, capacity: usize) -> Self {
        self.size_queues(capacity);
        self
    }

    /// Override the per-leaf drain deadline. Production threads the
    /// configured `EgressFabricConfig::drain_timeout_ms` through the shard
    /// factory; tests use it for fast, deterministic timing.
    pub(crate) fn with_drain_timeout(mut self, drain_timeout: Duration) -> Self {
        // Reserve part of the window for Owner teardown, never more than half.
        self.owner_shutdown_reserve = owner_set::OWNER_SHUTDOWN_RESERVE.min(drain_timeout / 2);
        self.drain_timeout = drain_timeout.saturating_sub(self.owner_shutdown_reserve);
        self
    }

    fn leaf_queue_capacity(&self) -> usize {
        self.leaves.len().max(1)
    }

    fn enqueue_ready_candidate(&mut self, key: LeafKey) -> bool {
        let capacity = self.leaf_queue_capacity();
        let admitted = push_bounded(&mut self.ready_candidates, key, capacity);
        if !admitted {
            self.queue_overflows = self.queue_overflows.saturating_add(1);
        }
        admitted
    }

    fn enqueue_feed_waiting(&mut self, key: LeafKey) -> bool {
        let capacity = self.leaf_queue_capacity();
        let admitted = push_bounded(&mut self.feed_waiting, key, capacity);
        if !admitted {
            self.queue_overflows = self.queue_overflows.saturating_add(1);
        }
        admitted
    }

    fn enqueue_blocked(&mut self, key: LeafKey) -> bool {
        let capacity = self.leaf_queue_capacity();
        let admitted = push_bounded(&mut self.blocked, key, capacity);
        if !admitted {
            self.queue_overflows = self.queue_overflows.saturating_add(1);
        }
        admitted
    }

    fn enqueue_stall_candidate(&mut self, key: LeafKey) -> bool {
        let capacity = self.leaf_queue_capacity();
        let admitted = push_bounded(&mut self.stall_candidates, key, capacity);
        if !admitted {
            self.queue_overflows = self.queue_overflows.saturating_add(1);
        }
        admitted
    }

    fn enqueue_ready_event(&mut self, event: SrtReadyLeaf) -> bool {
        let capacity = self.leaf_queue_capacity();
        let admitted = push_bounded(&mut self.ready, event, capacity);
        if !admitted {
            self.queue_overflows = self.queue_overflows.saturating_add(1);
        }
        admitted
    }

    fn allocate_leaf_key(&mut self) -> Option<LeafKey> {
        self.free_leaf_keys.pop()
    }

    /// Make a connected output live: give it a leaf slot, index its caller for
    /// exact event attribution, and schedule its first visit. A previous leaf
    /// for the same output (an `Update`) is closed.
    fn install_leaf(&mut self, common: LeafCommon, caller: SrtCaller) -> Option<LeafKey> {
        let progress_sink = common.progress_sink.clone();
        let Some(key) = self.allocate_leaf_key() else {
            progress_sink.mark_terminated_unexpectedly();
            self.owners.remove_now(&caller);
            return None;
        };
        let output_id = common.output_id.clone();
        self.leaves[key.0] = Some(SrtFabricLeaf::new(common, caller));
        self.callers.insert(caller, key);
        self.enqueue_ready_candidate(key);
        self.enqueue_stall_candidate(key);
        if let Some(previous) = self.output_sockets.insert(output_id, key) {
            self.remove_leaf(
                previous,
                crate::media::egress::backend::CloseReason::Removed,
            );
        }
        Some(key)
    }

    /// Fail an output that never became a leaf: nothing else will tell the
    /// application the attempt died.
    fn fail_pending(&mut self, output_id: &OutputId, generation: u64, why: &str) {
        let current = self
            .pending_connects
            .get(output_id)
            .is_some_and(|pending| pending.common.generation == generation);
        if !current {
            return;
        }
        if let Some(pending) = self.pending_connects.remove(output_id) {
            tracing::warn!(output_id = %output_id, reason = why, "srt egress connect failed");
            pending.common.progress_sink.mark_terminated_unexpectedly();
        }
    }

    /// Resolve-completion entry point: hand a resolved output to its family
    /// Owner. Direct or bonded, the Owner returns one `PoolOutcome`:
    /// `Admitted` makes the leaf now, `Queued` correlates the request id to
    /// this output/generation, `Full` fails the output.
    fn complete_pending_connect(
        &mut self,
        output_id: &OutputId,
        generation: u64,
        peers: &[SocketAddr],
    ) {
        let Some(pending) = self.pending_connects.get(output_id) else {
            return;
        };
        if pending.common.generation != generation || pending.stage != PendingStage::Resolving {
            return;
        }
        let request = match pending.connect_spec.connect_request(peers) {
            Ok(request) => request,
            Err(error) => {
                self.fail_pending(output_id, generation, &error);
                return;
            }
        };
        let family = request.family;
        match self.owners.connect(request) {
            Ok(PoolOutcome::Admitted(id)) => {
                if let Some(pending) = self.pending_connects.remove(output_id) {
                    self.install_leaf(pending.common, SrtCaller { family, id });
                }
            }
            Ok(PoolOutcome::Queued(request_id)) => {
                if let Some(pending) = self.pending_connects.get_mut(output_id) {
                    pending.stage = PendingStage::Queued { family, request_id };
                }
                self.queued_requests.insert(
                    (family, request_id),
                    QueuedRequest {
                        output_id: output_id.clone(),
                        generation,
                    },
                );
            }
            Ok(PoolOutcome::Full) => {
                self.fail_pending(output_id, generation, "caller pool and its queue are full");
            }
            Err(error) => self.fail_pending(output_id, generation, &error),
        }
    }

    fn remove_leaf_by_output(&mut self, output_id: &OutputId) -> bool {
        self.pending_connects.remove(output_id);
        let Some(key) = self.output_sockets.remove(output_id) else {
            return false;
        };
        self.remove_leaf(key, crate::media::egress::backend::CloseReason::Removed)
    }

    fn queue_pending_srt_connect(&mut self, spec: OutputSpec, target_url: &str) {
        let output_id = spec.id.clone();
        let already_admitted = self.output_sockets.contains_key(&output_id)
            || self.pending_connects.contains_key(&output_id);
        if !already_admitted
            && self
                .output_sockets
                .len()
                .saturating_add(self.pending_connects.len())
                >= self.leaves.len()
        {
            tracing::warn!(
                output_id = %output_id,
                "srt fabric leaf rejected: shard leaf capacity exhausted"
            );
            spec.progress.mark_terminated_unexpectedly();
            return;
        }
        let common = LeafCommon::new(
            spec.id,
            spec.generation,
            spec.feed,
            LeafLimits::from_policy(&spec.policy),
        )
        .with_progress_sink(spec.progress.clone());
        let connect_spec =
            SrtFabricEgressConnectSpec::from_url(target_url, spec.policy.connect_timeout);
        if connect_spec.peer_hosts().is_empty() {
            return;
        }
        // A replaced output's previous pending connect (and any queued Owner
        // request it made) is superseded; the stale request is retired when
        // the Owner later admits it (see `handle_owner_event`).
        self.pending_connects.insert(
            output_id,
            PendingSrtConnect {
                common,
                connect_spec,
                stage: PendingStage::Resolving,
            },
        );
    }

    fn remove_leaf(
        &mut self,
        key: LeafKey,
        reason: crate::media::egress::backend::CloseReason,
    ) -> bool {
        self.feed_waiting.retain(|queued| *queued != key);
        self.ready.retain(|event| event.key != key);
        self.ready_candidates.retain(|queued| *queued != key);
        self.blocked.retain(|queued| *queued != key);
        self.stall_candidates.retain(|queued| *queued != key);
        let Some(mut leaf) = self.leaves.get_mut(key.0).and_then(Option::take) else {
            return false;
        };
        let _ = reason;
        leaf.engine.clear();
        self.callers.remove(&leaf.caller);
        self.owners.begin_close(&leaf.caller);
        self.free_leaf_keys.push(key);
        true
    }

    /// Start one ready batch: service each existing Owner once, drain its
    /// bounded event queues, then move queued candidates into the ready
    /// queue. Never called per leaf or per fragment.
    fn begin_batch(&mut self) -> bool {
        self.batches = self.batches.saturating_add(1);
        let summary = self.owners.service();
        let mut scratch = std::mem::take(&mut self.event_scratch);
        let more_events = self.owners.drain_events(&mut scratch);
        for event in scratch.drain(..) {
            self.handle_owner_event(event);
        }
        self.event_scratch = scratch;
        for (index, faulted) in summary.newly_faulted.iter().enumerate() {
            if *faulted {
                let family = if index == 0 {
                    AddressFamily::V4
                } else {
                    AddressFamily::V6
                };
                self.fail_family(family);
            }
        }
        // Activity may have opened send windows: re-examine a bounded,
        // rotating slice of parked leaves.
        for _ in 0..BLOCKED_RECHECK_PER_BATCH {
            let Some(key) = self.blocked.pop_front() else {
                break;
            };
            if let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut) {
                leaf.blocked_queued = false;
                self.enqueue_ready_candidate(key);
            }
        }
        while let Some(key) = self.ready_candidates.pop_front() {
            let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut) else {
                continue;
            };
            if leaf.common.schedule.enqueued {
                continue;
            }
            leaf.common.schedule.enqueued = true;
            let generation = leaf.common.generation;
            if !self.enqueue_ready_event(SrtReadyLeaf { key, generation })
                && let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut)
            {
                leaf.common.schedule.enqueued = false;
            }
        }
        summary.work_remaining || more_events
    }

    fn requeue_after_visit(&mut self, key: LeafKey, decision: VisitDecision) {
        if matches!(decision, VisitDecision::Close) {
            return;
        }
        let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut) else {
            return;
        };
        if leaf.common.schedule.enqueued {
            return;
        }
        let feed_wake =
            leaf.common.schedule.wants_feed_wake && !leaf.common.schedule.feed_wake_queued;
        if feed_wake {
            leaf.common.schedule.feed_wake_queued = true;
            if !self.enqueue_feed_waiting(key)
                && let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut)
            {
                leaf.common.schedule.feed_wake_queued = false;
            }
        } else if matches!(decision, VisitDecision::Suspend) {
            // The send window is closed (or the caller is still connecting):
            // park. Owner activity re-examines parked leaves at a bounded
            // rate; nothing spins.
            if !leaf.blocked_queued {
                leaf.blocked_queued = true;
                if !self.enqueue_blocked(key)
                    && let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut)
                {
                    leaf.blocked_queued = false;
                }
            }
        } else {
            let _ = self.enqueue_ready_candidate(key);
        }
    }

    /// Visit the next ready leaf. Returns the output ID alongside the
    /// decision so the caller can remove a closed leaf. `OutputId` wraps a
    /// `String`, so it is only cloned on `VisitDecision::Close`.
    fn visit_one_ready_leaf(&mut self) -> Option<(Option<OutputId>, VisitDecision)> {
        let event = self.ready.pop_front()?;
        self.leaf_visits = self.leaf_visits.saturating_add(1);
        let budget = self.budget_config.new_visit();
        let feed = &self.feed;
        let now = self.owners.timestamp();
        let leaf = self.leaves.get_mut(event.key.0).and_then(Option::as_mut)?;
        let result = leaf.visit_ready(
            event.generation,
            Readiness {
                readable: false,
                writable: true,
            },
            feed,
            budget,
            &mut self.owners,
            now,
        );

        let decision = match result {
            EngineVisitResult::StaleGeneration => {
                leaf.common.schedule.enqueued = false;
                VisitDecision::Suspend
            }
            EngineVisitResult::Visited(outcome) => {
                if matches!(
                    &outcome.progress,
                    crate::media::egress::backend::EngineProgress::Yield
                ) {
                    self.budget_exhaustions = self.budget_exhaustions.saturating_add(1);
                }
                if matches!(
                    &outcome.progress,
                    crate::media::egress::backend::EngineProgress::FeedOverrun
                ) {
                    self.resync_count = self.resync_count.saturating_add(1);
                }
                outcome.decision
            }
        };

        // A draining leaf that has now flushed everything closes right here;
        // one stuck past its deadline force-closes so a peer that stops
        // reading mid-drain cannot hold it open forever.
        if let Some(draining_since) = leaf.draining_since {
            let backlog = self
                .owners
                .stats(&leaf.caller)
                .and_then(|stats| crate::media::srt::egress_stats::send_backlog(&stats));
            let flushed = !leaf.pressure(backlog).is_backpressured();
            let expired = draining_since.elapsed() >= self.drain_timeout;
            if flushed || expired {
                let reason = leaf
                    .draining_reason
                    .unwrap_or(crate::media::egress::backend::CloseReason::Removed);
                let output_id = leaf.common().output_id.clone();
                if let Some(key) = self.output_sockets.remove(&output_id) {
                    self.remove_leaf(key, reason);
                }
                return Some((None, VisitDecision::Suspend));
            }
        }

        let output_id =
            matches!(decision, VisitDecision::Close).then(|| leaf.common().output_id.clone());
        Some((output_id, decision))
    }

    /// Ready batches begun since construction.
    #[cfg(test)]
    pub(crate) fn batches(&self) -> u64 {
        self.batches
    }

    #[cfg(test)]
    pub(crate) fn leaf_visits(&self) -> u64 {
        self.leaf_visits
    }

    #[cfg(test)]
    pub(crate) fn stale_events(&self) -> u64 {
        self.stale_events
    }
}

impl EgressShardBackend for SrtShardBackend {
    fn resync_count(&self) -> u64 {
        self.resync_count
    }

    fn budget_exhaustion_count(&self) -> u64 {
        self.budget_exhaustions
    }

    fn observe_metrics(&self, metrics: &mut ShardMetrics) {
        self.owners.observe(metrics);
        metrics.budget_exhaustions = self.budget_exhaustions;
        metrics.queue_overflows = self.queue_overflows;
    }

    fn on_command(
        &mut self,
        command: crate::media::egress::command::EgressCommand,
    ) -> EgressShardCommandEffect {
        match command {
            EgressCommand::Add(spec) | EgressCommand::Update(spec) => {
                if let ProtocolSpec::Srt { url } = spec.protocol.clone() {
                    self.queue_pending_srt_connect(spec, &url);
                }
            }
            EgressCommand::Remove(output_id) => {
                self.begin_graceful_close(
                    &output_id,
                    crate::media::egress::backend::CloseReason::Removed,
                );
            }
            EgressCommand::FeedWake => self.enqueue_feed_waiting_leaves(),
            // Both mean "every leaf here should close, gracefully" --
            // `DrainShard` for future shard-count reconfiguration (the shard
            // keeps running), `Shutdown` because the process is going down
            // (the shard loop stays alive for its drain window; see
            // `EgressShardRuntime::run`).
            EgressCommand::DrainShard(_) | EgressCommand::Shutdown => {
                let output_ids: Vec<OutputId> = self.output_sockets.keys().cloned().collect();
                let reason = if matches!(command, EgressCommand::Shutdown) {
                    crate::media::egress::backend::CloseReason::ShardShutdown
                } else {
                    crate::media::egress::backend::CloseReason::Removed
                };
                for output_id in output_ids {
                    self.begin_graceful_close(&output_id, reason);
                }
                self.pending_connects.clear();
            }
        }
        EgressShardCommandEffect::Continue
    }

    /// Park inside the shard's Compio runtime when an Owner exists, so
    /// commands, Owner network/completion activity and Owner protocol
    /// deadlines are all awaited by the SAME runtime; before the first Owner
    /// there is no I/O to wait on and the default command-channel wait is
    /// exact.
    fn wait_idle(
        &mut self,
        commands: &flume::Receiver<EgressCommand>,
        max_wait: Duration,
    ) -> EgressShardIdleWake {
        if self.owners.has_owner() {
            return self.owners.wait_idle(commands, max_wait);
        }
        match commands.recv_timeout(max_wait) {
            Ok(command) => EgressShardIdleWake::Command(command),
            Err(flume::RecvTimeoutError::Timeout) => EgressShardIdleWake::Timeout,
            Err(flume::RecvTimeoutError::Disconnected) => EgressShardIdleWake::Disconnected,
        }
    }

    /// One `on_ready` = (at most) one ready batch + one leaf visit. A batch
    /// only begins when the ready queue is empty, so a batch's leaves are all
    /// visited under ONE Owner service, and parked leaves never cause a
    /// follow-up: the effect asks for another visit only while there is
    /// queued ready work or the Owner reported bounded work remaining.
    fn on_ready(&mut self) -> EgressShardCommandEffect {
        let mut owner_work_remaining = false;
        if self.ready.is_empty() {
            owner_work_remaining = self.begin_batch();
        }

        let ready_key = self.ready.front().map(|event| event.key);
        let outcome = self.visit_one_ready_leaf();
        if let Some((Some(output_id), VisitDecision::Close)) = &outcome {
            // `Close` only comes from `PeerClosed`/`Failed`; an explicit
            // `Remove` never reaches here, so every close seen here is
            // unexpected from the application's point of view.
            if let Some(key) = self.output_sockets.get(output_id)
                && let Some(leaf) = self.leaves.get(key.0).and_then(Option::as_ref)
            {
                leaf.common.progress_sink.mark_terminated_unexpectedly();
            }
            self.remove_leaf_by_output(output_id);
        }

        if let Some(key) = ready_key
            && let Some((_, decision)) = outcome.as_ref()
        {
            self.requeue_after_visit(key, *decision);
        }

        if !self.ready.is_empty() || !self.ready_candidates.is_empty() || owner_work_remaining {
            EgressShardCommandEffect::ScheduleReady { count: 1 }
        } else {
            EgressShardCommandEffect::Continue
        }
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        let mut resolved = std::mem::take(&mut self.resolved_connects);
        resolved.clear();
        self.resolve_completions.drain_resolved(&mut resolved);
        let before = self.output_sockets.len() + self.queued_requests.len();
        for completion in resolved.drain(..) {
            if completion.peer_addrs.is_empty() {
                // The bounded failure completion from the resolver worker.
                self.fail_pending(
                    &completion.output_id,
                    completion.generation,
                    "SRT peer resolution failed",
                );
                continue;
            }
            self.complete_pending_connect(
                &completion.output_id,
                completion.generation,
                &completion.peer_addrs,
            );
        }
        self.resolved_connects = resolved;
        self.sweep_stalled_leaves(Instant::now());
        let changed = self.output_sockets.len() + self.queued_requests.len() != before;
        if changed || !self.ready_candidates.is_empty() {
            EgressShardCommandEffect::ScheduleReady { count: 1 }
        } else {
            EgressShardCommandEffect::Continue
        }
    }

    fn on_shutdown(&mut self) {
        let keys: Vec<LeafKey> = self.output_sockets.drain().map(|(_, key)| key).collect();
        for key in keys {
            self.remove_leaf(
                key,
                crate::media::egress::backend::CloseReason::ShardShutdown,
            );
        }
        // Canonical Owner teardown, bounded by the reserve carved out of the
        // drain window. Removes any lingering closing callers first.
        let _ = self.owners.shutdown(self.owner_shutdown_reserve);
    }
}

#[cfg(test)]
mod tests;
