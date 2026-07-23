#![allow(dead_code)]

use std::collections::{HashMap, VecDeque};
use std::net::{SocketAddr, ToSocketAddrs};
use std::sync::mpsc::{self, Receiver, SyncSender, TryRecvError, TrySendError};
use std::thread::{self, JoinHandle};

use crate::media::egress::backend::{ProtocolEngine, Readiness};
use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::{LeafLimits, WorkBudget};
use crate::media::egress::scheduler::{LeafKey, VisitDecision};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::egress::visit::{EngineVisit, EngineVisitResult};
use crate::media::srt::{
    SRTSOCKET, SrtEgressEngine, SrtEgressInterest, SrtEgressPollError, SrtEgressSendMode,
    SrtFabricEgressConnectConfig, SrtFabricEgressConnectSpec, SrtFabricPoller, SrtMessageSender,
    SrtReadyLeaf, connect_fabric_srt_egress_socket, srt_fabric_message_sender,
};

mod add_error;
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
        }
    }

    pub(crate) fn common(&self) -> &LeafCommon {
        &self.common
    }

    pub(crate) fn pending_message_bytes(&self) -> usize {
        self.engine.pending_message_bytes()
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
    budget: WorkBudget,
    leaves: Vec<Option<NativeSrtLeaf>>,
    output_sockets: HashMap<OutputId, SrtLeafSocket>,
    ready: VecDeque<SrtReadyLeaf>,
    poll_buffer: Vec<SrtReadyLeaf>,
    pending_connects: HashMap<OutputId, PendingSrtConnect>,
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
        Self {
            poller,
            socket_configurator,
            socket_connector,
            resolve_completions,
            feed,
            budget,
            leaves: Vec::new(),
            output_sockets: HashMap::new(),
            ready: VecDeque::new(),
            poll_buffer: Vec::new(),
            pending_connects: HashMap::new(),
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
        let socket = connector
            .connect(config)
            .map_err(SrtBackendConnectError::Connect)?;
        self.add_connected_socket(common, socket)
            .map_err(SrtBackendConnectError::Add)
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
        );
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

    fn visit_one_ready_leaf(&mut self) -> Option<VisitDecision> {
        let event = self.ready.pop_front()?;
        let budget = self.budget;
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

        match result {
            EngineVisitResult::StaleGeneration => Some(VisitDecision::Suspend),
            EngineVisitResult::Visited(outcome) => Some(outcome.decision),
        }
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
            EgressCommand::DrainShard(_) | EgressCommand::Shutdown => {}
        }
        EgressShardCommandEffect::Continue
    }

    fn on_ready(&mut self) -> EgressShardCommandEffect {
        if self.ready.is_empty() {
            self.poll_ready();
        }

        if matches!(self.visit_one_ready_leaf(), Some(VisitDecision::Continue)) {
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
