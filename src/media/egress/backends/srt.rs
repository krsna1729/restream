#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
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
use crate::media::srt::{
    NativeSendBacklog, SRTSOCKET, SrtEgressEngine, SrtEgressInterest, SrtEgressPollError,
    SrtEgressSendMode, SrtFabricEgressConnectConfig, SrtFabricEgressConnectSpec, SrtFabricPoller,
    SrtMessageSender, SrtReadyLeaf, connect_fabric_srt_egress_socket, srt_fabric_message_sender,
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
    /// Anchor for stall aging before any progress has been recorded.
    observed_since: Instant,
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
            observed_since: Instant::now(),
        }
    }

    pub(crate) fn common(&self) -> &LeafCommon {
        &self.common
    }

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
    /// state.  A declining native backlog counts as protocol progress (the
    /// peer acknowledged data), so a leaf draining slowly through libsrt is
    /// backpressured rather than stalled; a leaf whose native buffer holds
    /// data without any decline past the no-progress deadline is stalled.
    pub(crate) fn observe_stall(&mut self, now: Instant) -> LeafStallClass {
        let pressure = self.pressure();
        let native_bytes = pressure.native_backlog.map_or(0, |backlog| backlog.bytes);
        if native_bytes < self.last_native_backlog_bytes {
            self.common.progress.last_protocol_progress = Some(now);
        }
        self.last_native_backlog_bytes = native_bytes;

        let last_progress = self
            .common
            .progress
            .last_byte_progress
            .into_iter()
            .chain(self.common.progress.last_protocol_progress)
            .max()
            .unwrap_or(self.observed_since);
        let age = now.saturating_duration_since(last_progress);
        classify_stall(pressure.pending_bytes(), age, &self.common.limits)
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

pub(crate) fn requeue_after_srt_visit(decision: VisitDecision) -> bool {
    matches!(decision, VisitDecision::Continue)
}

pub(crate) fn srt_fabric_leaf_from_socket(common: LeafCommon, socket: SRTSOCKET) -> NativeSrtLeaf {
    SrtFabricLeaf::new(common, srt_fabric_message_sender(socket))
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
        }
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
            self.remove_leaf_socket(previous);
        }
        Ok(key)
    }

    pub(crate) fn add_resolved_socket_with<T>(
        &mut self,
        common: LeafCommon,
        config: SrtFabricEgressConnectConfig<'_>,
        mut connector: T,
    ) -> Result<LeafKey, SrtBackendConnectError>
    where
        T: SrtSocketConnector,
    {
        let output_id = common.output_id.clone();
        let result = connector
            .connect(config)
            .map_err(SrtBackendConnectError::Connect)
            .and_then(|socket| {
                self.add_connected_socket(common, socket)
                    .map_err(SrtBackendConnectError::Add)
            });
        match &result {
            Ok(key) => {
                tracing::info!(
                    output_id = %output_id,
                    leaf_key = key.0,
                    "srt fabric leaf connected"
                );
            }
            Err(error) => {
                tracing::warn!(
                    output_id = %output_id,
                    error = ?error,
                    "srt fabric leaf connect failed"
                );
            }
        }
        result
    }

    pub(crate) fn complete_pending_connect_with<T>(
        &mut self,
        output_id: &OutputId,
        generation: u64,
        peer_addrs: &[std::net::SocketAddr],
        connector: T,
    ) -> Result<LeafKey, SrtPendingConnectError>
    where
        T: SrtSocketConnector,
    {
        let Some(pending) = self.pending_connects.remove(output_id) else {
            return Err(SrtPendingConnectError::Missing);
        };
        if pending.common.generation != generation {
            self.pending_connects.insert(output_id.clone(), pending);
            return Err(SrtPendingConnectError::Stale);
        }
        let config = pending.connect_spec.connect_config(peer_addrs, None);
        self.add_resolved_socket_with(pending.common, config, connector)
            .map_err(SrtPendingConnectError::Connect)
    }

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
        let config = pending.connect_spec.connect_config(peer_addrs, None);
        let socket = self.socket_connector.connect(config).map_err(|error| {
            SrtPendingConnectError::Connect(SrtBackendConnectError::Connect(error))
        })?;
        self.add_connected_socket(pending.common, socket)
            .map_err(|error| SrtPendingConnectError::Connect(SrtBackendConnectError::Add(error)))
    }

    fn add_leaf(
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
            self.remove_leaf_socket(previous);
        }
        Ok(key)
    }

    fn remove_leaf_by_output(&mut self, output_id: &OutputId) -> bool {
        self.pending_connects.remove(output_id);
        let Some(socket_ref) = self.output_sockets.remove(output_id) else {
            return false;
        };
        self.remove_leaf_socket(socket_ref)
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

    fn remove_leaf_socket(&mut self, socket_ref: SrtLeafSocket) -> bool {
        let _ = self.poller.remove(socket_ref.socket);
        let Some(leaf) = self.leaves.get_mut(socket_ref.key.0).and_then(Option::take) else {
            return false;
        };
        let mut leaf = leaf;
        leaf.engine.close(
            &mut leaf.transport,
            crate::media::egress::backend::CloseReason::Removed,
        );
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

    /// Minimum interval between stall sweeps: the native bstats probe is one
    /// FFI call per leaf, so the sweep runs at human-observable cadence, not
    /// per media tick.
    const STALL_SWEEP_INTERVAL: std::time::Duration = std::time::Duration::from_secs(1);

    /// Close every leaf whose combined application and native pending state
    /// has made no progress within the no-progress deadline.  Closed leaves
    /// surface as terminated outputs; the application retry policy owns
    /// reconnection (SRT recovery capability is reconnect-only).
    fn sweep_stalled_leaves(&mut self, now: Instant) {
        if self
            .last_stall_sweep
            .is_some_and(|last| now.saturating_duration_since(last) < Self::STALL_SWEEP_INTERVAL)
        {
            return;
        }
        self.last_stall_sweep = Some(now);

        let stalled: Vec<OutputId> = self
            .output_sockets
            .iter()
            .filter_map(|(output_id, socket_ref)| {
                let leaf = self.leaves.get_mut(socket_ref.key.0)?.as_mut()?;
                (leaf.observe_stall(now) == LeafStallClass::Stalled).then(|| output_id.clone())
            })
            .collect();

        for output_id in stalled {
            let Some(socket_ref) = self.output_sockets.remove(&output_id) else {
                continue;
            };
            let _ = self.poller.remove(socket_ref.socket);
            if let Some(leaf) = self.leaves.get_mut(socket_ref.key.0).and_then(Option::take) {
                let mut leaf = leaf;
                leaf.engine.close(
                    &mut leaf.transport,
                    crate::media::egress::backend::CloseReason::NoProgress,
                );
            }
        }
    }

    /// Visit the next ready leaf.  Returns the output ID alongside the
    /// decision so the caller can remove a closed leaf: closing is otherwise
    /// silently dropped, leaking a connected-but-dead socket and stalling
    /// the output forever (PeerClosed/Failed after the shared FeedOverrun
    /// path now resynchronizes in place instead of closing).
    fn visit_one_ready_leaf(&mut self) -> Option<(OutputId, VisitDecision)> {
        let event = self.ready.pop_front()?;
        let budget = WorkBudget::new(
            self.budget_max_units,
            self.budget_max_bytes,
            self.budget_window,
        );
        let feed = &self.feed;
        let leaf = self.leaves.get_mut(event.key.0).and_then(Option::as_mut)?;
        let output_id = leaf.common().output_id.clone();
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
            EngineVisitResult::Visited(outcome) => outcome.decision,
        };
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
                self.remove_leaf_by_output(&output_id);
            }
            EgressCommand::FeedWake | EgressCommand::DrainShard(_) | EgressCommand::Shutdown => {}
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
        if let Some((output_id, VisitDecision::Close)) = &outcome {
            self.remove_leaf_by_output(output_id);
        }

        let leaf_wants_more = matches!(&outcome, Some((_, VisitDecision::Continue)));
        if leaf_wants_more || !self.ready.is_empty() {
            EgressShardCommandEffect::ScheduleReady { count: 1 }
        } else {
            EgressShardCommandEffect::Continue
        }
    }

    fn on_media_tick(&mut self) {
        let mut resolved = Vec::new();
        self.resolve_completions.drain_resolved(&mut resolved);
        for completion in resolved {
            let _ = self.complete_pending_connect(
                &completion.output_id,
                completion.generation,
                &completion.peer_addrs,
            );
        }
        self.sweep_stalled_leaves(Instant::now());
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

#[cfg(test)]
mod tests;
