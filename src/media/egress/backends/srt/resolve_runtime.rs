use std::thread::JoinHandle;

use super::{
    NativeSrtSocketConfigurator, NativeSrtSocketConnector, SrtResolveCompletionQueue,
    SrtResolveRequest, SrtResolveWorkerError, SrtResolvedConnect, SrtShardBackend,
    duration_millis_u64, spawn_srt_resolve_worker, srt_resolve_completion_queue,
};
use crate::media::egress::command::{EgressCommand, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::srt::{SrtFabricEgressConnectSpec, SrtFabricPoller};
use std::sync::mpsc::SyncSender;

const SRT_RESOLVE_COMPLETION_QUEUE_CAPACITY: usize = 1024;

pub(crate) type ResolvingNativeSrtShardBackend = ResolvingSrtShardBackend<
    SrtShardBackend<
        SrtFabricPoller,
        NativeSrtSocketConfigurator,
        NativeSrtSocketConnector,
        SrtResolveCompletionQueue,
    >,
>;

pub(crate) type ResolvingSrtShardBackendWithPoller<P> = ResolvingSrtShardBackend<
    SrtShardBackend<
        P,
        NativeSrtSocketConfigurator,
        NativeSrtSocketConnector,
        SrtResolveCompletionQueue,
    >,
>;

#[derive(Debug)]
pub(crate) struct SrtResolveWorkerSet {
    completion_sender: SyncSender<SrtResolvedConnect>,
    workers: Vec<JoinHandle<Result<(), SrtResolveWorkerError>>>,
}

impl SrtResolveWorkerSet {
    pub(crate) fn new(completion_sender: SyncSender<SrtResolvedConnect>) -> Self {
        Self {
            completion_sender,
            workers: Vec::new(),
        }
    }

    fn spawn(&mut self, request: SrtResolveRequest) {
        self.workers.push(spawn_srt_resolve_worker(
            request,
            self.completion_sender.clone(),
        ));
    }

    fn reap_finished(&mut self) {
        let mut index = 0;
        while index < self.workers.len() {
            if self.workers[index].is_finished() {
                let worker = self.workers.swap_remove(index);
                let _ = worker.join();
            } else {
                index += 1;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn worker_count(&self) -> usize {
        self.workers.len()
    }
}

pub(crate) struct ResolvingSrtShardBackend<B> {
    backend: B,
    resolve_workers: SrtResolveWorkerSet,
}

impl<B> ResolvingSrtShardBackend<B> {
    pub(crate) fn new(backend: B, resolve_workers: SrtResolveWorkerSet) -> Self {
        Self {
            backend,
            resolve_workers,
        }
    }

    #[cfg(test)]
    pub(crate) fn worker_count(&self) -> usize {
        self.resolve_workers.worker_count()
    }
}

impl<B> EgressShardBackend for ResolvingSrtShardBackend<B>
where
    B: EgressShardBackend,
{
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        if let Some(request) = resolve_request_from_command(&command) {
            self.resolve_workers.spawn(request);
        }
        self.backend.on_command(command)
    }

    fn timer_generation(&self, output_id: &crate::media::egress::command::OutputId) -> Option<u64> {
        self.backend.timer_generation(output_id)
    }

    fn on_timer(
        &mut self,
        output_id: crate::media::egress::command::OutputId,
        generation: u64,
    ) -> EgressShardCommandEffect {
        self.backend.on_timer(output_id, generation)
    }

    fn on_ready(&mut self) -> EgressShardCommandEffect {
        self.backend.on_ready()
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        let effect = self.backend.on_media_tick();
        self.resolve_workers.reap_finished();
        effect
    }

    fn on_shutdown(&mut self) {
        self.backend.on_shutdown();
        self.resolve_workers.reap_finished();
    }
}

pub(crate) fn resolving_srt_shard_backend<P>(
    poller: P,
    feed: TsFeed,
    budget: WorkBudget,
    srt_egress_muxer_port_reuse: Option<std::sync::Arc<std::sync::Mutex<Option<u16>>>>,
    drain_timeout: std::time::Duration,
) -> ResolvingSrtShardBackend<
    SrtShardBackend<
        P,
        NativeSrtSocketConfigurator,
        NativeSrtSocketConnector,
        SrtResolveCompletionQueue,
    >,
>
where
    P: super::SrtReadinessPoller,
{
    resolving_srt_shard_backend_with_configurator(
        poller,
        feed,
        budget,
        NativeSrtSocketConfigurator,
        srt_egress_muxer_port_reuse,
        drain_timeout,
    )
}

pub(crate) fn resolving_srt_shard_backend_with_configurator<P, C>(
    poller: P,
    feed: TsFeed,
    budget: WorkBudget,
    socket_configurator: C,
    // Shared local-UDP-port reuse state for the libsrt egress multiplexer
    // (see `SrtShardBackend::with_srt_egress_muxer_port_reuse`). `None`
    // leaves reuse disabled (every existing test/no-config caller); `Some`
    // is the same `Arc<Mutex<Option<u16>>>` the legacy SRT egress path
    // shares via `MediaEngine::srt_egress_muxer_port_handle`, so a socket
    // connected by either path can be reused by the other.
    srt_egress_muxer_port_reuse: Option<std::sync::Arc<std::sync::Mutex<Option<u16>>>>,
    drain_timeout: std::time::Duration,
) -> ResolvingSrtShardBackend<
    SrtShardBackend<P, C, NativeSrtSocketConnector, SrtResolveCompletionQueue>,
>
where
    P: super::SrtReadinessPoller,
    C: super::SrtSocketConfigurator,
{
    let (completion_sender, completion_queue) =
        srt_resolve_completion_queue(SRT_RESOLVE_COMPLETION_QUEUE_CAPACITY);
    let mut backend = SrtShardBackend::with_runtime_components(
        poller,
        feed,
        budget,
        socket_configurator,
        NativeSrtSocketConnector,
        completion_queue,
    )
    .with_drain_timeout(drain_timeout);
    if let Some(state) = srt_egress_muxer_port_reuse {
        backend = backend.with_srt_egress_muxer_port_reuse(state, true);
    }
    ResolvingSrtShardBackend::new(backend, SrtResolveWorkerSet::new(completion_sender))
}

fn resolve_request_from_command(command: &EgressCommand) -> Option<SrtResolveRequest> {
    match command {
        EgressCommand::Add(spec) | EgressCommand::Update(spec) => {
            resolve_request_from_output_spec(spec)
        }
        EgressCommand::Remove(_)
        | EgressCommand::FeedWake
        | EgressCommand::DrainShard(_)
        | EgressCommand::Shutdown => None,
    }
}

fn resolve_request_from_output_spec(spec: &OutputSpec) -> Option<SrtResolveRequest> {
    let ProtocolSpec::Srt { url } = &spec.protocol else {
        return None;
    };
    let connect_spec =
        SrtFabricEgressConnectSpec::from_url(url, duration_millis_u64(spec.policy.connect_timeout));
    let peer_hosts = connect_spec.peer_hosts().to_vec();
    if peer_hosts.is_empty() {
        return None;
    }
    Some(SrtResolveRequest::new(
        spec.id.clone(),
        spec.generation,
        peer_hosts,
    ))
}
