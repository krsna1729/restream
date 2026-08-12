use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use crate::media::egress::backend::{ProtocolEngine, Readiness};
use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::{LeafLimits, LeafStallClass, WorkBudget, classify_stall};
use crate::media::egress::scheduler::{LeafKey, VisitDecision};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::egress::visit::{EngineVisit, EngineVisitResult};
use crate::media::snapshots::PublisherQuality;
use crate::media::srt::{
    NativeSendBacklog, SRTSOCKET, SrtEgressEngine, SrtEgressInterest, SrtEgressPollError,
    SrtEgressSendMode, SrtFabricEgressConnectConfig, SrtFabricEgressConnectSpec, SrtFabricPoller,
    SrtMessageSender, SrtReadyLeaf, SrtSenderCounterSnapshot, claim_srt_egress_muxer_port,
    connect_fabric_srt_egress_socket, srt_fabric_message_sender, srt_sender_quality_from_stats,
};

/// Combined application and native pending state for one SRT fabric leaf.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct SrtLeafPressure {
    pub app_pending_bytes: usize,
    pub native_backlog: Option<NativeSendBacklog>,
}

impl SrtLeafPressure {
    /// Bytes charged against the leaf memory envelope: retained application
    /// message plus unacknowledged native sender-buffer bytes.
    pub(crate) fn pending_bytes(&self) -> u64 {
        self.app_pending_bytes as u64 + self.native_backlog.map_or(0, |backlog| backlog.bytes)
    }

    /// True when data is waiting anywhere on the send path.  A leaf with a
    /// drained application queue but a saturated native buffer is
    /// backpressured, not idle.
    pub(crate) fn is_backpressured(&self) -> bool {
        self.pending_bytes() > 0
    }
}

mod add_error;
pub(crate) mod muxer_ports;
pub(crate) mod resolve_runtime;
mod socket_config;

pub(crate) use add_error::SrtBackendAddError;
pub(crate) use socket_config::{NativeSrtSocketConfigurator, SrtSocketConfigurator};

type NativeSrtLeaf = SrtFabricLeaf<Box<dyn SrtMessageSender + Send>>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SrtBackendConnectError {
    Connect(String),
    Add(SrtBackendAddError),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SrtPendingConnectError {
    Missing,
    Stale,
    Connect(SrtBackendConnectError),
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

pub(crate) struct SrtFabricLeaf<T>
where
    T: SrtMessageSender,
{
    common: LeafCommon,
    engine: SrtEgressEngine<T>,
    transport: T,
    /// Native backlog observed at the previous stall check; a decline means
    /// the peer acknowledged data (native progress) even without new sends.
    last_native_backlog_bytes: u64,
    /// `pktSndDropTotal` observed at the previous stall check. A backlog
    /// decline is only counted as progress when this counter did not
    /// advance: TLPKTDROP/TSBPD-deadline discards also shrink the buffer
    /// head, and a drop-riddled leaf must not extend its no-progress
    /// deadline forever ("0 sent / 3M dropped").
    last_packets_sent_drop: u64,
    /// Anchor for stall aging before any progress has been recorded.
    observed_since: Instant,
    /// Set when this leaf has been asked to close (via `Remove`,
    /// `DrainShard`, or `Shutdown`) but still had queued send-path bytes at
    /// that moment — mirrors `RtmpFabricLeaf::draining_since`
    /// (`rtmp_shard.rs`) exactly. While `Some`, the leaf stays registered
    /// and visited normally so it can flush that backlog (application
    /// message plus native libsrt sender buffer, see
    /// `SrtLeafPressure::pending_bytes`); it is force-closed once either
    /// `pending_bytes()` reaches zero or this instant is more than the
    /// backend's drain timeout in the past.
    draining_since: Option<Instant>,
    /// The reason to report once a draining leaf actually closes, recorded
    /// at the moment draining started so the real cause survives to the
    /// eventual close call.
    draining_reason: Option<crate::media::egress::backend::CloseReason>,
    /// Sender-side counters from the previous quality sample, needed to
    /// compute per-second rates (loss/drop/retrans) on the next one.
    quality_snapshot: Option<SrtSenderCounterSnapshot>,
    /// Effective `SRTO_SNDBUF` this leaf connected with (PREBIND — read
    /// once, reported on every quality sample without a per-tick FFI call).
    configured_sndbuf_bytes: Option<i32>,
}

impl<T> SrtFabricLeaf<T>
where
    T: SrtMessageSender,
{
    pub(crate) fn new(common: LeafCommon, transport: T) -> Self {
        Self {
            common,
            engine: SrtEgressEngine::default(),
            transport,
            last_native_backlog_bytes: 0,
            last_packets_sent_drop: 0,
            observed_since: Instant::now(),
            draining_since: None,
            draining_reason: None,
            quality_snapshot: None,
            configured_sndbuf_bytes: None,
        }
    }

    /// Sets the value read back from libsrt post-connect; see field doc.
    pub(crate) fn with_configured_sndbuf(mut self, bytes: Option<i32>) -> Self {
        self.configured_sndbuf_bytes = bytes;
        self
    }

    pub(crate) fn common(&self) -> &LeafCommon {
        &self.common
    }

    #[cfg(test)]
    pub(crate) fn pending_message_bytes(&self) -> usize {
        self.engine.pending_message_bytes()
    }

    #[cfg(test)]
    pub(crate) fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    /// Combined backpressure state per the architecture's native-buffer
    /// accounting rule: a leaf is backpressured when either the engine
    /// retains an application message or the native libsrt sender buffer
    /// holds unacknowledged data.  Removing the application queue while
    /// permitting unlimited native buffering does not count as progress.
    pub(crate) fn pressure(&mut self) -> SrtLeafPressure {
        SrtLeafPressure {
            app_pending_bytes: self.engine.pending_message_bytes(),
            native_backlog: self.transport.native_send_backlog(),
        }
    }

    /// Classify the send path from combined application and native pending
    /// state. A declining native backlog counts as protocol progress only
    /// when the peer actually drained data — i.e. `packets_sent_drop` did
    /// not advance (TLPKTDROP/TSBPD-deadline discards also shrink the
    /// buffer head, and a drop-riddled leaf must not extend its
    /// no-progress deadline forever); a leaf whose read cursor lags more
    /// than `max_feed_lag_units` behind the head is stalled regardless.
    /// `packets_sent_drop` is the transport's drop total for this sweep
    /// (`None` keeps the last sampled value, so the stall sweep's second
    /// per-leaf call costs no extra FFI); `lag_units` is how far the read
    /// cursor is behind the live edge (0 when unprimed or for lossless
    /// protocols).
    pub(crate) fn observe_stall(
        &mut self,
        now: Instant,
        packets_sent_drop: Option<u64>,
        lag_units: u64,
    ) -> LeafStallClass {
        let pressure = self.pressure();
        let native_bytes = pressure.native_backlog.map_or(0, |backlog| backlog.bytes);
        let drops = packets_sent_drop.unwrap_or(self.last_packets_sent_drop);
        if native_bytes < self.last_native_backlog_bytes && drops <= self.last_packets_sent_drop {
            self.common.progress.last_protocol_progress = Some(now);
        }
        self.last_native_backlog_bytes = native_bytes;
        if let Some(drops) = packets_sent_drop {
            self.last_packets_sent_drop = drops;
        }

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
            pressure.pending_bytes(),
            age,
            lag_units,
            &self.common.limits,
        )
    }

    /// Sample sender-side connection quality (RTT, loss, retransmits,
    /// bandwidth) from libsrt, for the once-per-second stall sweep — the
    /// same `srt_bistats` mechanism and cadence legacy SRT egress used for
    /// its own quality reporting. Returns `None` when the transport has no
    /// native stats to offer (fakes, closed sockets); the caller should
    /// leave the previously published quality in place in that case.
    pub(crate) fn sample_quality(&mut self, now: Instant) -> Option<PublisherQuality> {
        let stats = self.transport.sender_quality_stats()?;
        let (mut quality, snapshot) =
            srt_sender_quality_from_stats(&stats, self.quality_snapshot, now);
        self.quality_snapshot = Some(snapshot);
        quality.srt_sndbuf_configured_bytes = self.configured_sndbuf_bytes;
        Some(quality)
    }

    pub(crate) fn visit_ready(
        &mut self,
        generation: u64,
        readiness: Readiness,
        feed: &TsFeed,
        budget: WorkBudget,
    ) -> EngineVisitResult {
        EngineVisit {
            generation,
            common: &mut self.common,
            engine: &mut self.engine,
            transport: &mut self.transport,
            readiness,
            feed,
            budget,
        }
        .run()
    }
}

#[cfg(test)]
pub(crate) fn requeue_after_srt_visit(decision: VisitDecision) -> bool {
    matches!(decision, VisitDecision::Continue)
}

pub(crate) fn srt_fabric_leaf_from_socket(common: LeafCommon, socket: SRTSOCKET) -> NativeSrtLeaf {
    let sndbuf = Some(crate::media::srt::srt_get_configured_sndbuf(socket));
    SrtFabricLeaf::new(common, srt_fabric_message_sender(socket)).with_configured_sndbuf(sndbuf)
}

pub(crate) trait SrtReadinessPoller {
    fn register_leaf(
        &mut self,
        socket: SRTSOCKET,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError>;

    fn remove(&mut self, socket: SRTSOCKET) -> Result<(), SrtEgressPollError>;

    fn poll_leaves(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError>;
}

pub(crate) trait SrtSocketConnector {
    fn connect(&mut self, config: SrtFabricEgressConnectConfig<'_>) -> Result<SRTSOCKET, String>;
}

pub(crate) trait SrtResolveCompletionSource {
    fn drain_resolved(&mut self, resolved: &mut Vec<SrtResolvedConnect>);
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

pub(crate) fn spawn_srt_resolve_worker(
    request: SrtResolveRequest,
    completion_sender: SyncSender<SrtResolvedConnect>,
) -> JoinHandle<Result<(), SrtResolveWorkerError>> {
    // Fast path: if the peer is already a raw IP address, resolve it
    // synchronously without spawning a thread. Most SRT egress destinations
    // (local sinks, peered restream instances) use IP addresses, so this
    // avoids 1,200 thread creations at scale — each thread costs ~2 MB of
    // virtual address space and a clone() syscall.
    if request.peer_hosts.len() == 1
        && request.peer_hosts[0].parse::<SocketAddr>().is_ok()
    {
        let result = resolve_srt_peer_hosts(request, completion_sender);
        return thread::spawn(move || result);
    }
    // Hostname resolution needs blocking I/O.
    thread::spawn(move || resolve_srt_peer_hosts(request, completion_sender))
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

impl SrtResolveCompletionSource for SrtResolveCompletionQueue {
    fn drain_resolved(&mut self, resolved: &mut Vec<SrtResolvedConnect>) {
        loop {
            match self.receiver.try_recv() {
                Ok(completion) => resolved.push(completion),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => break,
            }
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct NativeSrtSocketConnector;

impl SrtSocketConnector for NativeSrtSocketConnector {
    fn connect(&mut self, config: SrtFabricEgressConnectConfig<'_>) -> Result<SRTSOCKET, String> {
        connect_fabric_srt_egress_socket(config)
    }
}

#[derive(Debug, Default)]
pub(crate) struct NoopSrtResolveCompletionSource;

impl SrtResolveCompletionSource for NoopSrtResolveCompletionSource {
    fn drain_resolved(&mut self, _resolved: &mut Vec<SrtResolvedConnect>) {}
}

impl SrtReadinessPoller for SrtFabricPoller {
    fn register_leaf(
        &mut self,
        socket: SRTSOCKET,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        self.register_leaf(socket, key, generation, interest)
    }

    fn remove(&mut self, socket: SRTSOCKET) -> Result<(), SrtEgressPollError> {
        self.remove(socket)
    }

    fn poll_leaves(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        self.poll_leaves(timeout_ms, ready)
    }
}

pub(crate) struct SrtShardBackend<
    P,
    C = NativeSrtSocketConfigurator,
    K = NativeSrtSocketConnector,
    R = NoopSrtResolveCompletionSource,
> where
    P: SrtReadinessPoller,
    C: SrtSocketConfigurator,
    K: SrtSocketConnector,
    R: SrtResolveCompletionSource,
{
    poller: P,
    socket_configurator: C,
    socket_connector: K,
    resolve_completions: R,
    feed: TsFeed,
    /// Per-visit limits. `WorkBudget::deadline` is an absolute `Instant`
    /// computed at construction time — storing one `WorkBudget` and reusing
    /// it for every visit (as this backend used to) makes `is_exhausted()`
    /// permanently `true` once that one deadline passes, silently stopping
    /// every leaf on this shard from reading or sending anything ever
    /// again (found and fixed for `RtmpShardBackend`; this is the same bug
    /// in the SRT shard — see `docs/egress-implementation.md` Phase 5
    /// status). A fresh `WorkBudget` is constructed from these fields for
    /// every visit instead (see `visit_one_ready_leaf`).
    budget_max_units: usize,
    budget_max_bytes: usize,
    budget_window: Duration,
    leaves: Vec<Option<NativeSrtLeaf>>,
    output_sockets: HashMap<OutputId, SrtLeafSocket>,
    ready: VecDeque<SrtReadyLeaf>,
    poll_buffer: Vec<SrtReadyLeaf>,
    pending_connects: HashMap<OutputId, PendingSrtConnect>,
    last_stall_sweep: Option<Instant>,
    /// This shard's local-UDP-port state for libsrt egress-multiplexer
    /// reuse, handed out per shard by
    /// `SrtEgressMuxerPorts::shard` (`muxer_ports.rs`) so every leaf on this
    /// shard shares one libsrt multiplexer — and therefore one `CSndQueue`
    /// worker thread — while other shards get their own. Defaults to a
    /// fresh, backend-local mutex with reuse disabled so every existing
    /// constructor (tests included) keeps building unconfigured sockets
    /// unless `with_srt_egress_muxer_port_reuse` opts in explicitly.
    srt_egress_muxer_port: Arc<Mutex<Option<u16>>>,
    reuse_local_srt_egress_port: bool,
    /// Bound on how long a leaf may stay in `draining_since` before it is
    /// force-closed regardless of remaining pending send-path bytes.
    /// Mirrors `RtmpShardBackend::drain_timeout` exactly. Defaults to
    /// `EgressShardConfig::DEFAULT_DRAIN_TIMEOUT`; tests use
    /// `with_drain_timeout` for fast, deterministic timing.
    drain_timeout: Duration,
    /// Total `EngineProgress::FeedOverrun` resynchronizations observed
    /// across every leaf this backend has ever visited. Mirrors
    /// `RtmpShardBackend::resync_count` exactly.
    resync_count: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SrtLeafSocket {
    key: LeafKey,
    socket: SRTSOCKET,
}

struct PendingSrtConnect {
    common: LeafCommon,
    connect_spec: SrtFabricEgressConnectSpec,
}

// Production always constructs via `with_runtime_components` directly (see
// resolve_runtime.rs); these convenience constructors are only used by tests.
#[cfg(test)]
impl<P>
    SrtShardBackend<
        P,
        NativeSrtSocketConfigurator,
        NativeSrtSocketConnector,
        NoopSrtResolveCompletionSource,
    >
where
    P: SrtReadinessPoller,
{
    pub(crate) fn new(poller: P, feed: TsFeed, budget: WorkBudget) -> Self {
        Self::with_socket_configurator(poller, feed, budget, NativeSrtSocketConfigurator)
    }
}

#[cfg(test)]
impl<P, C> SrtShardBackend<P, C, NativeSrtSocketConnector, NoopSrtResolveCompletionSource>
where
    P: SrtReadinessPoller,
    C: SrtSocketConfigurator,
{
    pub(crate) fn with_socket_configurator(
        poller: P,
        feed: TsFeed,
        budget: WorkBudget,
        socket_configurator: C,
    ) -> Self {
        Self::with_runtime_components(
            poller,
            feed,
            budget,
            socket_configurator,
            NativeSrtSocketConnector,
            NoopSrtResolveCompletionSource,
        )
    }
}

impl<P, C, K, R> SrtShardBackend<P, C, K, R>
where
    P: SrtReadinessPoller,
    C: SrtSocketConfigurator,
    K: SrtSocketConnector,
    R: SrtResolveCompletionSource,
{
    pub(crate) fn with_runtime_components(
        poller: P,
        feed: TsFeed,
        budget: WorkBudget,
        socket_configurator: C,
        socket_connector: K,
        resolve_completions: R,
    ) -> Self {
        let budget_window = budget.deadline.saturating_duration_since(Instant::now());
        Self {
            poller,
            socket_configurator,
            socket_connector,
            resolve_completions,
            feed,
            budget_max_units: budget.max_units,
            budget_max_bytes: budget.max_bytes,
            budget_window,
            leaves: Vec::new(),
            output_sockets: HashMap::new(),
            ready: VecDeque::new(),
            poll_buffer: Vec::new(),
            pending_connects: HashMap::new(),
            last_stall_sweep: None,
            srt_egress_muxer_port: Arc::new(Mutex::new(None)),
            reuse_local_srt_egress_port: false,
            drain_timeout: crate::media::egress::shard::EgressShardConfig::DEFAULT_DRAIN_TIMEOUT,
            resync_count: 0,
        }
    }

    /// Override the per-leaf drain deadline. Production threads the
    /// configured `EgressFabricConfig::drain_timeout_ms` through the shard
    /// factory; tests use it for fast, deterministic timing instead of the
    /// constructor's multi-second default.
    pub(crate) fn with_drain_timeout(mut self, drain_timeout: Duration) -> Self {
        self.drain_timeout = drain_timeout;
        self
    }

    /// Opts this backend's outbound SRT connects into the shared local-port
    /// reuse `state` (see the field doc on `srt_egress_muxer_port`). Kept as
    /// a separate builder step rather than a `with_runtime_components`
    /// parameter so every existing constructor and test call site is
    /// unaffected; only production wiring
    /// (`resolving_srt_shard_backend_with_configurator`) calls this.
    pub(crate) fn with_srt_egress_muxer_port_reuse(
        mut self,
        state: Arc<Mutex<Option<u16>>>,
        enabled: bool,
    ) -> Self {
        self.srt_egress_muxer_port = state;
        self.reuse_local_srt_egress_port = enabled;
        self
    }

    /// The exact reuse state this backend will claim from, so shard-wiring
    /// tests can prove distinct shards were handed distinct per-shard state
    /// (`Arc::ptr_eq`) without a second construction path.
    #[cfg(test)]
    pub(crate) fn srt_egress_muxer_port_state(&self) -> &Arc<Mutex<Option<u16>>> {
        &self.srt_egress_muxer_port
    }

    pub(crate) fn add_connected_socket(
        &mut self,
        common: LeafCommon,
        socket: SRTSOCKET,
    ) -> Result<LeafKey, SrtBackendAddError> {
        self.socket_configurator
            .configure_connected(socket, SrtEgressSendMode::FabricNonblocking)?;

        let key = LeafKey(self.leaves.len());
        self.poller
            .register_leaf(socket, key, common.generation, SrtEgressInterest::WRITE)
            .map_err(SrtBackendAddError::Poller)?;
        let output_id = common.output_id.clone();
        let leaf = srt_fabric_leaf_from_socket(common, socket);
        self.leaves.push(Some(leaf));
        if let Some(previous) = self
            .output_sockets
            .insert(output_id, SrtLeafSocket { key, socket })
        {
            self.remove_leaf_socket(
                previous,
                crate::media::egress::backend::CloseReason::Removed,
            );
        }
        Ok(key)
    }

    /// Resolve-completion entry point: turn a queued `PendingSrtConnect`
    /// into a live leaf using this backend's own `socket_connector`.
    ///
    /// Tests drive this same function rather than a parallel injectable
    /// variant — `SrtShardBackend` is already generic over its connector
    /// (`K: SrtSocketConnector`), so a fake is supplied by constructing the
    /// backend with `with_runtime_components`. Keeping one path means the
    /// progress-sink bookkeeping below stays covered instead of being
    /// silently skipped by a test-only sibling.
    fn complete_pending_connect(
        &mut self,
        output_id: &OutputId,
        generation: u64,
        peer_addrs: &[std::net::SocketAddr],
    ) -> Result<LeafKey, SrtPendingConnectError> {
        let Some(pending) = self.pending_connects.remove(output_id) else {
            return Err(SrtPendingConnectError::Missing);
        };
        if pending.common.generation != generation {
            self.pending_connects.insert(output_id.clone(), pending);
            return Err(SrtPendingConnectError::Stale);
        }
        // A connect failure here means the application never sees a leaf at
        // all — nothing else will tell it the attempt died, so mark it the
        // same way an established leaf's unexpected close does (see
        // `EgressProgressSink::terminated_unexpectedly`).
        let progress_sink = pending.common.progress_sink.clone();
        let muxer_port_state = self.srt_egress_muxer_port.clone();
        let muxer_port_claim = self
            .reuse_local_srt_egress_port
            .then(|| claim_srt_egress_muxer_port(&muxer_port_state));
        let config = pending
            .connect_spec
            .connect_config(peer_addrs, muxer_port_claim);
        let socket = self.socket_connector.connect(config).map_err(|error| {
            tracing::warn!(
                output_id = %output_id,
                error = %error,
                "srt fabric leaf connect failed"
            );
            progress_sink.mark_terminated_unexpectedly();
            SrtPendingConnectError::Connect(SrtBackendConnectError::Connect(error))
        })?;
        self.add_connected_socket(pending.common, socket)
            .map_err(|error| {
                progress_sink.mark_terminated_unexpectedly();
                SrtPendingConnectError::Connect(SrtBackendConnectError::Add(error))
            })
    }

    #[cfg(test)]
    pub(crate) fn add_leaf(
        &mut self,
        socket: SRTSOCKET,
        leaf: NativeSrtLeaf,
    ) -> Result<LeafKey, SrtEgressPollError> {
        let key = LeafKey(self.leaves.len());
        let output_id = leaf.common.output_id.clone();
        self.poller.register_leaf(
            socket,
            key,
            leaf.common.generation,
            SrtEgressInterest::WRITE,
        )?;
        self.leaves.push(Some(leaf));
        if let Some(previous) = self
            .output_sockets
            .insert(output_id, SrtLeafSocket { key, socket })
        {
            self.remove_leaf_socket(
                previous,
                crate::media::egress::backend::CloseReason::Removed,
            );
        }
        Ok(key)
    }

    fn remove_leaf_by_output(&mut self, output_id: &OutputId) -> bool {
        self.pending_connects.remove(output_id);
        let Some(socket_ref) = self.output_sockets.remove(output_id) else {
            return false;
        };
        self.remove_leaf_socket(
            socket_ref,
            crate::media::egress::backend::CloseReason::Removed,
        )
    }

    fn queue_pending_srt_connect(&mut self, spec: OutputSpec, target_url: &str) {
        let output_id = spec.id.clone();
        let common = LeafCommon::new(
            spec.id,
            spec.generation,
            spec.feed,
            LeafLimits::from_policy(&spec.policy),
        )
        .with_progress_sink(spec.progress.clone());
        let connect_spec = SrtFabricEgressConnectSpec::from_url(
            target_url,
            duration_millis_u64(spec.policy.connect_timeout),
        );
        if connect_spec.peer_hosts().is_empty() {
            return;
        }
        self.pending_connects.insert(
            output_id,
            PendingSrtConnect {
                common,
                connect_spec,
            },
        );
    }

    #[cfg(test)]
    fn pending_connect(&self, output_id: &OutputId) -> Option<&PendingSrtConnect> {
        self.pending_connects.get(output_id)
    }

    fn remove_leaf_socket(
        &mut self,
        socket_ref: SrtLeafSocket,
        reason: crate::media::egress::backend::CloseReason,
    ) -> bool {
        let _ = self.poller.remove(socket_ref.socket);
        let Some(leaf) = self.leaves.get_mut(socket_ref.key.0).and_then(Option::take) else {
            return false;
        };
        let mut leaf = leaf;
        leaf.engine.close(&mut leaf.transport, reason);
        true
    }

    fn poll_ready(&mut self) {
        if self.poller.poll_leaves(0, &mut self.poll_buffer).is_err() {
            return;
        }

        let events: Vec<_> = self.poll_buffer.drain(..).collect();
        for event in events {
            let Some(leaf) = self.leaf_mut(event.key) else {
                continue;
            };
            if leaf.common.schedule.enqueued {
                continue;
            }
            leaf.common.schedule.enqueued = true;
            self.ready.push_back(event);
        }
    }

    fn leaf_mut(&mut self, key: LeafKey) -> Option<&mut NativeSrtLeaf> {
        self.leaves.get_mut(key.0).and_then(Option::as_mut)
    }

    /// Visit the next ready leaf.  Returns the output ID alongside the
    /// decision so the caller can remove a closed leaf: closing is otherwise
    /// silently dropped, leaking a connected-but-dead socket and stalling
    /// the output forever (PeerClosed/Failed after the shared FeedOverrun
    /// path now resynchronizes in place instead of closing).
    ///
    /// `OutputId` wraps a `String`, so cloning it is a heap allocation; the
    /// caller only ever uses it on `VisitDecision::Close` (to remove the
    /// leaf), so it's only cloned then — every other visit (the overwhelming
    /// majority in steady state) pays nothing for it.
    fn visit_one_ready_leaf(&mut self) -> Option<(Option<OutputId>, VisitDecision)> {
        let event = self.ready.pop_front()?;
        let budget = WorkBudget::new(
            self.budget_max_units,
            self.budget_max_bytes,
            self.budget_window,
        );
        let feed = &self.feed;
        let leaf = self.leaves.get_mut(event.key.0).and_then(Option::as_mut)?;
        let result = leaf.visit_ready(
            event.generation,
            Readiness {
                readable: false,
                writable: event.writable,
            },
            feed,
            budget,
        );

        let decision = match result {
            EngineVisitResult::StaleGeneration => VisitDecision::Suspend,
            EngineVisitResult::Visited(outcome) => {
                if matches!(
                    outcome.progress,
                    crate::media::egress::backend::EngineProgress::FeedOverrun
                ) {
                    self.resync_count = self.resync_count.saturating_add(1);
                }
                outcome.decision
            }
        };

        // A draining leaf (see `begin_graceful_close`) that has now flushed
        // everything it had queued closes right here — no need to wait for
        // the next `sweep_draining_leaves` tick. One still stuck past its
        // deadline force-closes the same way, so a peer that stops reading
        // mid-drain can't hang this leaf open forever. Mirrors
        // `RtmpShardBackend::visit_one_ready_leaf` exactly.
        if let Some(draining_since) = leaf.draining_since {
            let flushed = !leaf.pressure().is_backpressured();
            let expired = draining_since.elapsed() >= self.drain_timeout;
            if flushed || expired {
                let reason = leaf
                    .draining_reason
                    .unwrap_or(crate::media::egress::backend::CloseReason::Removed);
                let output_id = leaf.common().output_id.clone();
                if let Some(socket_ref) = self.output_sockets.remove(&output_id) {
                    self.remove_leaf_socket(socket_ref, reason);
                }
                return Some((None, VisitDecision::Suspend));
            }
        }

        let output_id =
            matches!(decision, VisitDecision::Close).then(|| leaf.common().output_id.clone());
        Some((output_id, decision))
    }
}

impl<P, C, K, R> EgressShardBackend for SrtShardBackend<P, C, K, R>
where
    P: SrtReadinessPoller + Send + 'static,
    C: SrtSocketConfigurator + Send + 'static,
    K: SrtSocketConnector + Send + 'static,
    R: SrtResolveCompletionSource + Send + 'static,
{
    fn resync_count(&self) -> u64 {
        self.resync_count
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
            // Both mean "every leaf here should close, gracefully" —
            // `DrainShard` for future shard-count reconfiguration (the
            // shard itself keeps running afterward), `Shutdown` because the
            // whole process is going down (the shard-runtime layer keeps
            // this shard's loop alive long enough to let leaves flush; see
            // `EgressShardRuntime::run`'s drain window in `shard.rs`).
            // Mirrors `RtmpShardBackend::on_command` exactly.
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
            }
        }
        EgressShardCommandEffect::Continue
    }

    /// Visit one ready leaf, then decide whether to ask for another
    /// `on_ready` pass immediately.
    ///
    /// `poll_ready()` can enqueue several ready leaves from one poll — SRT
    /// always registers write interest, so a single poll commonly finds
    /// every leaf on the shard writable at once. If the leaf visited
    /// *this* call suspends (would block) or closes, that alone must not
    /// stop the shard from draining the rest of an already-nonempty
    /// `self.ready` queue: those leaves were already reported ready and
    /// would otherwise sit stranded until some unrelated future command
    /// happened to touch this shard again. Requeuing whenever `self.ready`
    /// is still nonempty (in addition to the existing "this leaf wants to
    /// continue" case) fixes that: a blocked leaf never blocks its
    /// already-ready neighbors. (Same bug, same fix, as
    /// `RtmpShardBackend::on_ready` — see `docs/egress-implementation.md`
    /// Phase 5 status.)
    fn on_ready(&mut self) -> EgressShardCommandEffect {
        if self.ready.is_empty() {
            self.poll_ready();
        }

        let outcome = self.visit_one_ready_leaf();
        if let Some((Some(output_id), VisitDecision::Close)) = &outcome {
            // `VisitDecision::Close` is only ever produced from
            // `EngineProgress::PeerClosed`/`Failed` (see `visit.rs`) — an
            // explicit `EgressCommand::Remove` never reaches this path — so
            // every close observed here is unexpected from the
            // application's point of view.
            if let Some(socket_ref) = self.output_sockets.get(output_id)
                && let Some(leaf) = self.leaves.get(socket_ref.key.0).and_then(Option::as_ref)
            {
                leaf.common.progress_sink.mark_terminated_unexpectedly();
            }
            self.remove_leaf_by_output(output_id);
        }

        let leaf_wants_more = matches!(&outcome, Some((_, VisitDecision::Continue)));
        if leaf_wants_more || !self.ready.is_empty() {
            EgressShardCommandEffect::ScheduleReady { count: 1 }
        } else {
            EgressShardCommandEffect::Continue
        }
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        let mut resolved = Vec::new();
        self.resolve_completions.drain_resolved(&mut resolved);
        let mut connected_any = false;
        for completion in resolved {
            let connected = self
                .complete_pending_connect(
                    &completion.output_id,
                    completion.generation,
                    &completion.peer_addrs,
                )
                .is_ok();
            connected_any |= connected;
        }
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
            let _ = self.poller.remove(socket_ref.socket);
            if let Some(leaf) = self.leaves.get_mut(socket_ref.key.0).and_then(Option::take) {
                let mut leaf = leaf;
                leaf.engine.close(
                    &mut leaf.transport,
                    crate::media::egress::backend::CloseReason::ShardShutdown,
                );
            }
        }
    }
}

fn duration_millis_u64(duration: std::time::Duration) -> u64 {
    duration.as_millis().min(u128::from(u64::MAX)) as u64
}

#[path = "srt_drain.rs"]
mod srt_drain;

#[cfg(test)]
mod tests;
