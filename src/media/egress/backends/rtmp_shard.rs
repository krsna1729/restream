//! RTMP fabric shard backend. A shard-local Compio runtime owns connection
//! receives and writes; DNS resolution stays on a dedicated worker so a slow
//! resolver cannot stall the shard.
//!
//! Completion events are bounded and generation-tagged. The protocol engine
//! remains shard-scheduled, preserving feed fairness, drain and retry rules.

use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, ToSocketAddrs};
use std::os::fd::AsRawFd;
use std::os::unix::io::RawFd;
use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, SyncSender};
use std::time::{Duration, Instant};

use tokio_rustls::rustls::ClientConfig;

use crate::media::egress::backend::{CloseReason, EngineProgress, ProtocolEngine, Readiness};
use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::feed::EgressFeed;
use crate::media::egress::journal::RingFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::metrics::ShardMetrics;
use crate::media::egress::policy::{
    LeafLimits, LeafStallClass, WorkBudget, WorkBudgetConfig, classify_stall,
};
use crate::media::egress::scheduler::{LeafKey, VisitDecision};
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardCommandEffect, EgressShardConfig,
};
use crate::media::egress::visit::{EngineVisit, EngineVisitResult};
use crate::media::rtmp::parse_rtmp_url;

#[cfg(test)]
use super::compio_tcp::CompioTcpPoller;
use super::rtmp::{RtmpFabricEngine, RtmpPublishStartup};
use super::rtmp_connection::RtmpConnection;
#[cfg(test)]
use super::tcp::TcpConnectAttempt;
#[cfg(test)]
use super::tcp::TcpEgressPollError;
use super::tcp::TcpReadyLeaf;

use self::rtmp_shard_connect::{ConnectingRtmpConnect, PendingRtmpConnect};
pub(crate) use super::rtmp_shard_poller::RtmpReadinessPoller;

// ---------------------------------------------------------------------------
// Publish-startup source
// ---------------------------------------------------------------------------

/// Supplies the immutable [`RtmpPublishStartup`] snapshot for one output.
/// The application layer assembles this (querying `MediaEngine`, output
/// registries, and ring state — none of which a leaf visit may touch) before
/// adding the output; production writes it into the shared source before the
/// shard receives the `Add` command.
pub(crate) trait RtmpPublishStartupSource {
    fn take_startup(&mut self, output_id: &OutputId) -> Option<RtmpPublishStartup>;
}

/// Supplies an empty startup snapshot for backend tests that bypass the
/// application-layer startup handoff.
#[derive(Debug, Default)]
pub(crate) struct EmptyRtmpPublishStartupSource;

impl RtmpPublishStartupSource for EmptyRtmpPublishStartupSource {
    fn take_startup(&mut self, _output_id: &OutputId) -> Option<RtmpPublishStartup> {
        Some(RtmpPublishStartup::default())
    }
}

/// Real source backed by a shared map: the application layer assembles
/// `RtmpFabricStartup` (querying `MediaEngine`, output registries, and ring
/// state), converts it to `RtmpPublishStartup`, and calls
/// [`Self::set`] before dispatching `EgressCommand::Add` for that output —
/// the shard thread only ever reads, via `take_startup`, never queries
/// anything itself. One instance is shared (cloned) across every shard of a
/// fabric runtime, since any output can land on any shard.
#[derive(Debug, Clone, Default)]
pub(crate) struct SharedRtmpPublishStartupSource {
    pending: std::sync::Arc<std::sync::Mutex<HashMap<OutputId, RtmpPublishStartup>>>,
}

impl SharedRtmpPublishStartupSource {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set(&self, output_id: OutputId, startup: RtmpPublishStartup) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(output_id, startup);
    }

    pub(crate) fn remove(&self, output_id: &OutputId) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(output_id);
    }
}

impl RtmpPublishStartupSource for SharedRtmpPublishStartupSource {
    fn take_startup(&mut self, output_id: &OutputId) -> Option<RtmpPublishStartup> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .get(output_id)
            .cloned()
    }
}

// ---------------------------------------------------------------------------
// Resolve worker
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RtmpResolvedConnect {
    pub(crate) output_id: OutputId,
    pub(crate) generation: u64,
    /// `None` is the bounded failure completion: resolution failed, so the
    /// shard fails the matching pending connect instead of keeping it
    /// resident (and holding leaf capacity) forever.
    pub(crate) peer_addr: Option<SocketAddr>,
}

pub(crate) struct RtmpResolveCompletionQueue {
    receiver: Receiver<RtmpResolvedConnect>,
}

pub(crate) fn rtmp_resolve_completion_queue(
    capacity: usize,
) -> (SyncSender<RtmpResolvedConnect>, RtmpResolveCompletionQueue) {
    let (sender, receiver) = mpsc::sync_channel(capacity);
    (sender, RtmpResolveCompletionQueue { receiver })
}

impl RtmpResolveCompletionQueue {
    pub(crate) fn drain_resolved(&mut self, resolved: &mut Vec<RtmpResolvedConnect>) {
        while let Ok(completion) = self.receiver.try_recv() {
            resolved.push(completion);
        }
    }
}

fn push_bounded<T>(queue: &mut VecDeque<T>, value: T, capacity: usize) -> bool {
    debug_assert!(queue.len() <= capacity);
    if queue.len() == capacity {
        return false;
    }
    queue.push_back(value);
    true
}

fn merge_ready_flags(pending: &mut Readiness, event: TcpReadyLeaf) {
    pending.readable |= event.readable;
    pending.writable |= event.writable;
}

pub(crate) fn resolve_rtmp_peer_host(host: &str, port: u16) -> Option<SocketAddr> {
    if let Ok(addr) = host.parse::<std::net::IpAddr>() {
        return Some(SocketAddr::new(addr, port));
    }
    (host, port).to_socket_addrs().ok()?.next()
}

struct RtmpFabricLeaf {
    common: LeafCommon,
    engine: RtmpFabricEngine,
    transport: RtmpConnection,
    pending_readiness: Readiness,
    /// Last-progress fallback while a leaf has made no byte or protocol
    /// progress yet, such as during connect or handshake.
    observed_since: Instant,
    /// Handshake and session negotiation must reach publish acceptance by
    /// this instant. They queue no application bytes, so the pending-byte
    /// stall classifier alone would report a wedged startup as idle forever.
    startup_deadline: Instant,
    /// Set when this leaf has been asked to close (via `Remove`,
    /// `DrainShard`, or `Shutdown`) but still had queued application bytes
    /// at that moment. While `Some`, the leaf stays registered and visited
    /// normally so it can flush that backlog; it is force-closed once
    /// either `pending_application_bytes` reaches zero or this instant is
    /// more than the backend's drain timeout in the past — whichever comes
    /// first. `None` means "not closing" (the common case).
    draining_since: Option<Instant>,
    /// The reason to report once a draining leaf actually closes, recorded
    /// at the moment draining started so the real cause (removed vs.
    /// shutdown) survives to the eventual close call.
    draining_reason: Option<CloseReason>,
    /// `(tcp_bytes_sent, sampled_at)` from the previous quality sample,
    /// needed to compute `tcp_send_rate_mbps` as a two-sample delta —
    /// mirrors `rtmp/ingest.rs`'s `previous_tcp_bytes` for the receive side.
    previous_tcp_bytes: Option<(u64, Instant)>,
    /// Peer-acknowledged vs feed-published bytes, rated over one window.
    delivery: crate::media::egress::delivery::DeliveryTracker,
}

impl RtmpFabricLeaf {
    fn visit_ready(
        &mut self,
        generation: u64,
        readiness: Readiness,
        feed: &RingFeed,
        budget: WorkBudget,
    ) -> EngineVisitResult {
        let result = EngineVisit {
            generation,
            common: &mut self.common,
            engine: &mut self.engine,
            transport: &mut self.transport,
            readiness,
            feed,
            budget,
        }
        .run();
        self.transport.resume_receive();
        result
    }

    /// Classify send-path health from pending application bytes and time
    /// since the last byte or protocol progress. The shared stall classifier
    /// consumes the progress state updated by `EngineVisit::run`.
    fn observe_stall(&self, now: Instant) -> LeafStallClass {
        let last_progress = self
            .common
            .progress
            .last_byte_progress
            .into_iter()
            .chain(self.common.progress.last_protocol_progress)
            .max()
            .unwrap_or(self.observed_since);
        let age = now.saturating_duration_since(last_progress);
        classify_stall(
            self.common.pending_application_bytes as u64,
            age,
            // Lossless-TCP catch-up: a lagging RTMP leaf recovers by
            // reading flat-out, so the lag ceiling is SRT-only.
            0,
            &self.common.limits,
        )
    }

    /// Sample sender-side TCP quality (RTT, retransmits, cwnd, pacing rate,
    /// congestion algorithm) for the once-per-second stall sweep — the same
    /// `TCP_INFO`/`SO_MEMINFO` mechanism and cadence legacy RTMP egress used
    /// for its own quality reporting, and the same conversion `rtmp/ingest.rs`
    /// already uses on the receive side. Returns `None` when `TCP_INFO` is
    /// unavailable (non-Linux, or the getsockopt call itself failed); the
    /// caller should leave the previously published quality in place then.
    fn sample_quality(
        &mut self,
        now: Instant,
        feed_published_bytes: u64,
    ) -> Option<crate::media::snapshots::PublisherQuality> {
        let stats =
            crate::media::tcp_stats::collect_tcp_stats_by_fd(self.transport.raw_fd()).ok()?;
        let delivery = stats
            .tcp_bytes_acked
            .map(|acked| self.delivery.sample(acked, feed_published_bytes, now));
        let send_rate = stats.tcp_bytes_sent.and_then(|bytes| {
            let rate = self.previous_tcp_bytes.and_then(|(previous, sampled_at)| {
                crate::media::tcp_stats::bytes_delta_rate_mbps(
                    bytes,
                    previous,
                    now.duration_since(sampled_at).as_secs_f64(),
                )
            });
            self.previous_tcp_bytes = Some((bytes, now));
            rate
        });
        let mut quality = stats.into_egress_quality(send_rate);
        if let Some(delivery) = delivery {
            quality.delivered_bps = delivery.delivered_bps;
            quality.offered_bps = delivery.offered_bps;
            quality.delivery_ratio = delivery.ratio;
        }
        Some(quality)
    }
}

/// The local visit a leaf needs after `progress`, or `None` when a real wake
/// source is already pending.
///
/// Completions are edge events: a leaf is only visited again if its visit left
/// it a wake source. Transmit completions exist only while bytes are in flight,
/// receive completions only for new peer bytes, and feed wakes only for
/// `Feed`/`FeedOrIo`. A visit may also write without reading, leaving bytes,
/// EOF or an error that an already-consumed receive completion staged in the
/// adapter. So a write-wanting leaf with nothing in flight, a read-wanting leaf
/// with staged receive state, a budget `Yield`, and a state transition each
/// get one bounded local visit at the ready-queue tail, round-robin with other
/// ready leaves. WouldBlock with bytes in flight waits for the real transmit
/// completion instead of spinning.
fn local_followup_visit(
    progress: &EngineProgress,
    transmit_in_flight: bool,
    buffered_receive: bool,
) -> Option<Readiness> {
    let wait_interest = match progress {
        EngineProgress::Needs(wait) | EngineProgress::Progress { wait, .. } => wait.io_interest(),
        EngineProgress::HandshakeComplete | EngineProgress::FeedOverrun | EngineProgress::Yield => {
            return Some(Readiness::BOTH);
        }
        EngineProgress::PeerClosed | EngineProgress::Failed(_) => return None,
    };
    let write = wait_interest.writable && !transmit_in_flight;
    let read = wait_interest.readable && buffered_receive;
    match (read, write) {
        (true, true) => Some(Readiness::BOTH),
        (true, false) => Some(Readiness::READABLE),
        (false, true) => Some(Readiness::WRITABLE),
        (false, false) => None,
    }
}

fn requeue_after_rtmp_visit(decision: VisitDecision) -> bool {
    matches!(decision, VisitDecision::Continue)
}

// ---------------------------------------------------------------------------
// Shard backend
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct RtmpLeafSocket {
    key: LeafKey,
    fd: RawFd,
}

pub(crate) struct RtmpShardBackend<P, S = EmptyRtmpPublishStartupSource>
where
    P: RtmpReadinessPoller,
    S: RtmpPublishStartupSource,
{
    poller: P,
    resolve_completions: RtmpResolveCompletionQueue,
    resolved_connects: Vec<RtmpResolvedConnect>,
    startup_source: S,
    feed: RingFeed,
    /// Per-visit limits and window remain valid throughout shard startup;
    /// each visit receives a fresh absolute deadline.
    budget_config: WorkBudgetConfig,
    chunk_size: u32,
    rtmps_client_config: Arc<ClientConfig>,
    leaves: Vec<Option<RtmpFabricLeaf>>,
    free_leaf_keys: Vec<LeafKey>,
    output_sockets: HashMap<OutputId, RtmpLeafSocket>,
    ready: VecDeque<TcpReadyLeaf>,
    /// Visits left before completions are reaped again even though `ready`
    /// is non-empty: one ready-queue round. Local follow-up visits requeue
    /// without I/O, so polling only on an empty queue could starve the very
    /// completion a requeued leaf is waiting for.
    visits_until_poll: usize,
    feed_waiting: VecDeque<LeafKey>,
    stall_candidates: VecDeque<LeafKey>,
    queue_capacity: usize,
    poll_buffer: Vec<TcpReadyLeaf>,
    pending_connects: HashMap<OutputId, PendingRtmpConnect>,
    connecting: HashMap<LeafKey, ConnectingRtmpConnect>,
    connecting_by_output: HashMap<OutputId, LeafKey>,
    last_stall_sweep: Option<Instant>,
    /// Bound on how long a leaf may stay in `draining_since` before it is
    /// force-closed regardless of remaining `pending_application_bytes`.
    /// Defaults to `EgressShardConfig::DEFAULT_DRAIN_TIMEOUT`; tests use
    /// `with_drain_timeout` for fast, deterministic timing.
    drain_timeout: Duration,
    /// Total `EngineProgress::FeedOverrun` resynchronizations observed across
    /// every leaf this backend has ever visited. Read by
    /// `EgressShardRuntime::record_iteration` into `ShardMetrics::feed_resyncs`
    /// for the repeated-resync alert (`derive_alerts`, `src/alerts.rs`).
    resync_count: u64,
    budget_exhaustions: u64,
    /// Media units and bytes the engine handed to its transports.
    tx_units: u64,
    tx_bytes: u64,
    queue_overflows: u64,
}

impl<P, S> RtmpShardBackend<P, S>
where
    P: RtmpReadinessPoller,
    S: RtmpPublishStartupSource,
{
    pub(crate) fn with_runtime_components(
        poller: P,
        feed: RingFeed,
        budget: WorkBudgetConfig,
        chunk_size: u32,
        rtmps_client_config: Arc<ClientConfig>,
        resolve_completions: RtmpResolveCompletionQueue,
        startup_source: S,
    ) -> Self {
        let ready_capacity = poller.ready_capacity();
        Self {
            poller,
            resolve_completions,
            resolved_connects: Vec::with_capacity(1024),
            startup_source,
            feed,
            budget_config: budget,
            chunk_size,
            rtmps_client_config,
            leaves: (0..EgressShardConfig::DEFAULT_LEAF_CAPACITY)
                .map(|_| None)
                .collect(),
            free_leaf_keys: (0..EgressShardConfig::DEFAULT_LEAF_CAPACITY as u32)
                .rev()
                .map(|slot| LeafKey(slot as usize))
                .collect(),
            output_sockets: HashMap::new(),
            ready: VecDeque::with_capacity(ready_capacity),
            visits_until_poll: 0,
            feed_waiting: VecDeque::with_capacity(ready_capacity),
            stall_candidates: VecDeque::with_capacity(ready_capacity),
            queue_capacity: EgressShardConfig::DEFAULT_LEAF_CAPACITY,
            poll_buffer: Vec::with_capacity(ready_capacity),
            pending_connects: HashMap::new(),
            connecting: HashMap::new(),
            connecting_by_output: HashMap::new(),
            last_stall_sweep: None,
            drain_timeout: crate::media::egress::shard::EgressShardConfig::DEFAULT_DRAIN_TIMEOUT,
            resync_count: 0,
            budget_exhaustions: 0,
            tx_units: 0,
            tx_bytes: 0,
            queue_overflows: 0,
        }
    }

    pub(crate) fn with_leaf_capacity(mut self, capacity: usize) -> Self {
        self.leaves = (0..capacity).map(|_| None).collect();
        self.free_leaf_keys = (0..capacity as u32)
            .rev()
            .map(|slot| LeafKey(slot as usize))
            .collect();
        self.queue_capacity = capacity;
        self.ready = VecDeque::with_capacity(capacity);
        self.feed_waiting = VecDeque::with_capacity(capacity);
        self.stall_candidates = VecDeque::with_capacity(capacity);
        self
    }

    fn enqueue_ready(&mut self, event: TcpReadyLeaf) -> bool {
        let queue_capacity = self.queue_capacity;
        let ready = &mut self.ready;
        let queue_overflows = &mut self.queue_overflows;
        let Some(leaf) = self.leaves.get_mut(event.key.0).and_then(Option::as_mut) else {
            return false;
        };
        if leaf.common.generation != event.generation {
            return false;
        }
        merge_ready_flags(&mut leaf.pending_readiness, event);
        if leaf.common.schedule.enqueued {
            return true;
        }
        leaf.common.schedule.enqueued = true;
        let admitted = push_bounded(ready, event, queue_capacity);
        if !admitted {
            leaf.common.schedule.enqueued = false;
            *queue_overflows = queue_overflows.saturating_add(1);
        }
        admitted
    }

    fn enqueue_stall_candidate(&mut self, key: LeafKey) -> bool {
        let admitted = push_bounded(&mut self.stall_candidates, key, self.queue_capacity);
        if !admitted {
            self.queue_overflows = self.queue_overflows.saturating_add(1);
        }
        admitted
    }

    /// Override the per-leaf drain deadline. Production threads the
    /// configured `EgressFabricConfig::drain_timeout_ms` through here (see
    /// `resolving_rtmp_shard_backend`); tests use it for fast, deterministic
    /// timing instead of the constructor's multi-second default.
    pub(crate) fn with_drain_timeout(mut self, drain_timeout: Duration) -> Self {
        self.drain_timeout = drain_timeout;
        self
    }

    fn remove_leaf_socket(&mut self, socket_ref: RtmpLeafSocket, reason: CloseReason) -> bool {
        let _ = self.poller.remove(socket_ref.fd);
        self.feed_waiting.retain(|key| *key != socket_ref.key);
        self.stall_candidates.retain(|key| *key != socket_ref.key);
        self.ready.retain(|event| event.key != socket_ref.key);
        self.poll_buffer.retain(|event| event.key != socket_ref.key);
        let Some(leaf) = self.leaves.get_mut(socket_ref.key.0).and_then(Option::take) else {
            return false;
        };
        let mut leaf = leaf;
        leaf.engine.close(&mut leaf.transport, reason);
        self.free_leaf_keys.push(socket_ref.key);
        true
    }

    fn allocate_leaf_key(&mut self) -> Option<LeafKey> {
        self.free_leaf_keys.pop()
    }

    /// Minimum interval between stall sweeps — no per-leaf FFI probe to
    /// throttle here (unlike SRT's native bstats call), but there is no
    /// reason to walk every leaf on every media tick either.
    const STALL_SWEEP_INTERVAL: Duration = Duration::from_secs(1);

    /// Directly enqueue leaves parked on `Feed`/`FeedOrIo` when the shared
    /// feed publishes more media. The queue is populated at visit time, so a
    /// feed wake does not scan every output on the shard.
    ///
    /// Feed wakes go directly to the deduplicated ready queue rather than
    /// manufacturing a socket-readiness event.
    ///
    /// Safe against the regression an earlier direct-enqueue attempt hit
    /// (documented in this method's prior history): that attempt pushed a
    /// synthetic ready entry unconditionally, which starved
    /// handshake/negotiation leaves — those always want *real* I/O, and a
    /// synthetic no-readiness event kept winning the race to be visited
    /// before `poll_ready()` ever ran. This can't happen here because
    /// handshake/negotiation sub-state-machines only ever report
    /// `WaitCondition::Io(_)` (see `RtmpFabricEngine::advance`'s
    /// `Handshaking`/`Negotiating` arms), never `Feed`/`FeedOrIo`, so
    /// `wants_feed_wake` is structurally `false` for them and this loop
    /// never touches them — they remain discoverable only via real
    /// `poll_ready()`, exactly as before.
    fn enqueue_feed_waiting_leaves(&mut self) {
        while let Some(key) = self.feed_waiting.pop_front() {
            let event = self
                .leaves
                .get_mut(key.0)
                .and_then(Option::as_mut)
                .and_then(|leaf| {
                    leaf.common.schedule.feed_wake_queued = false;
                    if !leaf.common.schedule.wants_feed_wake || leaf.common.schedule.enqueued {
                        None
                    } else {
                        Some(TcpReadyLeaf {
                            fd: leaf.transport.raw_fd(),
                            key,
                            generation: leaf.common.generation,
                            readable: false,
                            writable: false,
                        })
                    }
                });
            if let Some(event) = event {
                self.enqueue_ready(event);
            }
        }
    }

    fn poll_ready(&mut self) {
        if self.poller.poll_leaves(0, &mut self.poll_buffer).is_err() {
            return;
        }
        let mut poll_buffer = std::mem::take(&mut self.poll_buffer);
        for event in poll_buffer.drain(..) {
            if self.connecting.contains_key(&event.key) {
                if self.finish_connecting(event)
                    && self
                        .leaves
                        .get(event.key.0)
                        .and_then(Option::as_ref)
                        .is_some()
                {
                    self.enqueue_ready(event);
                }
            } else {
                self.enqueue_ready(event);
            }
        }
        self.poll_buffer = poll_buffer;
    }

    /// Visit one completion-ready leaf. The scheduler keeps protocol work
    /// bounded and isolates a blocked socket from healthy neighbors.
    ///
    /// `OutputId` wraps a `String`, so cloning it is a heap allocation; the
    /// caller only ever uses it on `VisitDecision::Close` (to remove the
    /// leaf), so it's only cloned then — every other visit (the overwhelming
    /// majority in steady state) pays nothing for it.
    fn visit_one_ready_leaf(&mut self) -> Option<(Option<OutputId>, VisitDecision)> {
        let event = self.ready.pop_front()?;
        let budget = self.budget_config.new_visit();
        let feed = &self.feed;
        let leaf = self.leaves.get_mut(event.key.0).and_then(Option::as_mut)?;
        let readiness = std::mem::take(&mut leaf.pending_readiness);
        let result = leaf.visit_ready(event.generation, readiness, feed, budget);
        let (progress, decision) = match result {
            EngineVisitResult::StaleGeneration => return Some((None, VisitDecision::Suspend)),
            EngineVisitResult::Visited(outcome) => {
                if let EngineProgress::Progress { bytes, units, .. } = &outcome.progress {
                    self.tx_bytes = self.tx_bytes.saturating_add(*bytes as u64);
                    self.tx_units = self.tx_units.saturating_add(*units as u64);
                }
                if matches!(&outcome.progress, EngineProgress::Yield) {
                    self.budget_exhaustions = self.budget_exhaustions.saturating_add(1);
                }
                (outcome.progress, outcome.decision)
            }
        };
        let followup = local_followup_visit(
            &progress,
            leaf.transport.pending_transport_write_bytes() > 0,
            leaf.transport.has_buffered_receive(),
        );
        if matches!(
            progress,
            crate::media::egress::backend::EngineProgress::FeedOverrun
        ) {
            self.resync_count = self.resync_count.saturating_add(1);
        }
        leaf.common.pending_application_bytes = leaf
            .engine
            .pending_application_bytes()
            .saturating_add(leaf.transport.rustls_pending_bytes_estimate())
            .saturating_add(leaf.transport.pending_transport_write_bytes());
        // Progress can still end on an empty feed. Keep that leaf parked for
        // the next publication even though this visit returns `Continue`.
        let feed_waiting = leaf.common.schedule.wants_feed_wake
            && !leaf.common.schedule.enqueued
            && !leaf.common.schedule.feed_wake_queued;
        if feed_waiting {
            let admitted = push_bounded(&mut self.feed_waiting, event.key, self.queue_capacity);
            if !admitted {
                self.queue_overflows = self.queue_overflows.saturating_add(1);
            }
            leaf.common.schedule.feed_wake_queued = admitted;
        }

        // A draining leaf (see `begin_graceful_close`) that has now flushed
        // everything it had queued closes right here — no need to wait for
        // the next `sweep_draining_leaves` tick. One still stuck past its
        // deadline force-closes the same way, so a peer that stops reading
        // mid-drain can't hang this leaf open forever.
        if let Some(draining_since) = leaf.draining_since {
            let flushed = leaf.common.pending_application_bytes == 0;
            let expired = draining_since.elapsed() >= self.drain_timeout;
            if flushed || expired {
                let reason = leaf.draining_reason.unwrap_or(CloseReason::Removed);
                let output_id = leaf.common.output_id.clone();
                if let Some(socket_ref) = self.output_sockets.remove(&output_id) {
                    self.remove_leaf_socket(socket_ref, reason);
                }
                return Some((None, VisitDecision::Suspend));
            }
        }

        if matches!(decision, VisitDecision::Close) {
            return Some((Some(leaf.common.output_id.clone()), decision));
        }

        if let Some(followup_readiness) = followup {
            leaf.pending_readiness = followup_readiness;
            let admitted = push_bounded(
                &mut self.ready,
                TcpReadyLeaf {
                    fd: event.fd,
                    key: event.key,
                    generation: event.generation,
                    readable: followup_readiness.readable,
                    writable: followup_readiness.writable,
                },
                self.queue_capacity,
            );
            if !admitted {
                self.queue_overflows = self.queue_overflows.saturating_add(1);
            }
            leaf.common.schedule.enqueued = admitted;
        }

        Some((None, decision))
    }
}

impl<P, S> EgressShardBackend for RtmpShardBackend<P, S>
where
    P: RtmpReadinessPoller + 'static,
    S: RtmpPublishStartupSource + Send + 'static,
{
    fn resync_count(&self) -> u64 {
        self.resync_count
    }

    fn budget_exhaustion_count(&self) -> u64 {
        self.budget_exhaustions
    }

    fn observe_metrics(&self, metrics: &mut ShardMetrics) {
        // RTMP observes media units and bytes handed to its transports and
        // the completion events it consumes; io_uring SQ/CQ counters are not
        // visible through Compio and stay unset.
        let (completions, stale_completions) = self.poller.completion_counts();
        metrics.tx_packets = self.tx_units;
        metrics.tx_bytes = self.tx_bytes;
        metrics.cqes = completions;
        metrics.stale_completions = stale_completions;
        metrics.budget_exhaustions = self.budget_exhaustions;
        metrics.queue_overflows = self.queue_overflows;
    }

    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match command {
            EgressCommand::Add(spec) | EgressCommand::Update(spec) => {
                if let ProtocolSpec::Rtmp { url, .. } = spec.protocol.clone() {
                    self.queue_pending_rtmp_connect(spec, &url);
                }
            }
            EgressCommand::Remove(output_id) => {
                self.begin_graceful_close(&output_id, CloseReason::Removed);
            }
            EgressCommand::FeedWake => self.enqueue_feed_waiting_leaves(),
            // Both mean "every leaf here should close, gracefully" —
            // `DrainShard` for future shard-count reconfiguration (the
            // shard itself keeps running afterward), `Shutdown` because the
            // whole process is going down (the shard-runtime layer keeps
            // this shard's loop alive long enough to let leaves flush; see
            // `EgressShardRuntime::run`'s drain window in `shard.rs`).
            EgressCommand::DrainShard(_) | EgressCommand::Shutdown => {
                let output_ids: Vec<OutputId> = self.output_sockets.keys().cloned().collect();
                let reason = if matches!(command, EgressCommand::Shutdown) {
                    CloseReason::ShardShutdown
                } else {
                    CloseReason::Removed
                };
                for output_id in output_ids {
                    self.begin_graceful_close(&output_id, reason);
                }
            }
        }
        EgressShardCommandEffect::Continue
    }

    /// Visit one ready leaf, then decide whether to ask for another
    /// `on_ready` pass immediately.
    ///
    /// `poll_ready()` can enqueue several ready leaves from one poll; if
    /// the leaf visited *this* call suspends (needs more I/O readiness) or
    /// closes, that alone must not stop the shard from draining the rest
    /// of an already-nonempty `self.ready` queue — those leaves were
    /// already reported ready and would otherwise sit stranded until some
    /// unrelated future command or feed wake happened to touch this shard
    /// again. Requeuing whenever `self.ready` is still nonempty (in
    /// addition to the existing "this leaf wants to continue" case) fixes
    /// that: a blocked leaf never blocks its already-ready neighbors.
    fn on_ready(&mut self) -> EgressShardCommandEffect {
        if self.ready.is_empty() || self.visits_until_poll == 0 {
            self.poll_ready();
            self.visits_until_poll = self.ready.len().max(1);
        }
        self.visits_until_poll -= 1;

        let outcome = self.visit_one_ready_leaf();
        if let Some((Some(output_id), VisitDecision::Close)) = &outcome {
            // `VisitDecision::Close` here means either
            // `EngineProgress::PeerClosed`/`Failed` (see `visit.rs`) or a
            // failed poller re-registration inside `visit_one_ready_leaf`
            // — an explicit `EgressCommand::Remove` never reaches this
            // path — so every close observed here is unexpected from the
            // application's point of view.
            if let Some(socket_ref) = self.output_sockets.get(output_id)
                && let Some(leaf) = self.leaves.get(socket_ref.key.0).and_then(Option::as_ref)
            {
                leaf.common.progress_sink.mark_terminated_unexpectedly();
            }
            self.remove_leaf_by_output(output_id);
        }

        let leaf_wants_more =
            matches!(&outcome, Some((_, decision)) if requeue_after_rtmp_visit(*decision));
        if leaf_wants_more || !self.ready.is_empty() {
            EgressShardCommandEffect::ScheduleReady { count: 1 }
        } else {
            EgressShardCommandEffect::Continue
        }
    }

    fn wait_idle(
        &mut self,
        commands: &flume::Receiver<EgressCommand>,
        max_wait: Duration,
    ) -> crate::media::egress::shard::EgressShardIdleWake {
        if let Some(wake) = self.poller.wait_idle(commands, max_wait) {
            return wake;
        }
        match commands.recv_timeout(max_wait) {
            Ok(command) => crate::media::egress::shard::EgressShardIdleWake::Command(command),
            Err(flume::RecvTimeoutError::Timeout) => {
                crate::media::egress::shard::EgressShardIdleWake::Timeout
            }
            Err(flume::RecvTimeoutError::Disconnected) => {
                crate::media::egress::shard::EgressShardIdleWake::Disconnected
            }
        }
    }
    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        let mut resolved = std::mem::take(&mut self.resolved_connects);
        resolved.clear();
        self.resolve_completions.drain_resolved(&mut resolved);
        let mut connected_any = false;
        for completion in resolved.drain(..) {
            let Some(peer_addr) = completion.peer_addr else {
                self.fail_pending_connect(&completion.output_id, completion.generation);
                continue;
            };
            connected_any |= self.complete_pending_connect(
                &completion.output_id,
                completion.generation,
                peer_addr,
            );
        }
        self.resolved_connects = resolved;
        self.sweep_connecting_leaves(Instant::now());
        self.sweep_stalled_leaves(Instant::now());
        if connected_any {
            EgressShardCommandEffect::ScheduleReady { count: 1 }
        } else {
            EgressShardCommandEffect::Continue
        }
    }

    fn on_shutdown(&mut self) {
        let sockets: Vec<_> = self
            .output_sockets
            .drain()
            .map(|(_, socket_ref)| socket_ref)
            .collect();
        for socket_ref in sockets {
            let _ = self.poller.remove(socket_ref.fd);
            if let Some(leaf) = self.leaves.get_mut(socket_ref.key.0).and_then(Option::take) {
                let mut leaf = leaf;
                leaf.engine.close(
                    &mut leaf.transport,
                    crate::media::egress::backend::CloseReason::ShardShutdown,
                );
            }
        }
        let connecting = std::mem::take(&mut self.connecting);
        self.connecting_by_output.clear();
        for (_, connecting) in connecting {
            let _ = self.poller.remove(connecting.stream.as_raw_fd());
        }
    }
}

impl<P> RtmpShardBackend<P, EmptyRtmpPublishStartupSource>
where
    P: RtmpReadinessPoller,
{
    // Production always constructs via `with_runtime_components` directly
    // (see rtmp_shard_resolve_runtime.rs); this convenience constructor is
    // only used by tests.
    #[cfg(test)]
    pub(crate) fn new(
        poller: P,
        feed: RingFeed,
        budget: WorkBudgetConfig,
        chunk_size: u32,
    ) -> Self {
        let (_sender, queue) = rtmp_resolve_completion_queue(1);
        Self::with_runtime_components(
            poller,
            feed,
            budget,
            chunk_size,
            crate::media::rtmp::rustls_client_config(),
            queue,
            EmptyRtmpPublishStartupSource,
        )
    }
}

#[path = "rtmp_shard_connect.rs"]
mod rtmp_shard_connect;

#[path = "rtmp_shard_drain.rs"]
mod rtmp_shard_drain;

#[cfg(test)]
#[path = "rtmp_shard_tests.rs"]
mod tests;
