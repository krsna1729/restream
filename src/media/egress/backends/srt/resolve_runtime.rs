use std::thread::JoinHandle;

use super::{
    NativeSrtSocketConfigurator, NativeSrtSocketConnector, SrtResolveCompletionQueue,
    SrtResolveRequest, SrtResolveWorkerError, SrtResolvedConnect, SrtShardBackend,
    duration_millis_u64, srt_resolve_completion_queue,
};
use crate::media::egress::command::{EgressCommand, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::srt::SrtFabricEgressConnectSpec;
use std::sync::mpsc::SyncSender;

const SRT_RESOLVE_COMPLETION_QUEUE_CAPACITY: usize = 1024;

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

    /// Spawn a single worker that resolves a batch of requests, collapsing
    /// N thread creations into 1. Each request is resolved sequentially in
    /// the worker; completions are sent to the shared completion queue.
    fn spawn_batch(&mut self, requests: Vec<SrtResolveRequest>) {
        if requests.is_empty() {
            return;
        }
        let sender = self.completion_sender.clone();
        self.workers.push(std::thread::spawn(move || {
            for request in requests {
                let _ = super::resolve_srt_peer_hosts(request, sender.clone());
            }
            Ok(())
        }));
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
    /// Pending resolves buffered during `on_command` and flushed in
    /// `on_media_tick` — batches N resolves into one thread instead of
    /// spawning a thread per command. At 1,200-output scale this reduces
    /// thread creations from 1,200 to ~shard-iterations (~30/s = 40/iter).
    pending_resolves: Vec<SrtResolveRequest>,
}

impl<B> ResolvingSrtShardBackend<B> {
    pub(crate) fn new(backend: B, resolve_workers: SrtResolveWorkerSet) -> Self {
        Self {
            backend,
            resolve_workers,
            pending_resolves: Vec::with_capacity(64),
        }
    }

    #[cfg(test)]
    pub(crate) fn worker_count(&self) -> usize {
        self.resolve_workers.worker_count()
    }

    /// The wrapped shard backend, so factory-wiring tests can inspect what
    /// this decorator was actually constructed around.
    #[cfg(test)]
    pub(crate) fn inner_backend(&self) -> &B {
        &self.backend
    }
}

impl<B> EgressShardBackend for ResolvingSrtShardBackend<B>
where
    B: EgressShardBackend,
{
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        // Buffer the resolve request instead of spawning a thread per
        // command. `on_media_tick` flushes the batch.
        if let Some(request) = resolve_request_from_command(&command) {
            self.pending_resolves.push(request);
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
        // Flush buffered resolve requests into the worker set, batching
        // all pending resolves into a single thread per tick instead of
        // one thread per command. At 1,200-output scale this collapses
        // ~1,200 thread creations into ~40 (one per shard iteration).
        if !self.pending_resolves.is_empty() {
            let batch = std::mem::take(&mut self.pending_resolves);
            self.resolve_workers.spawn_batch(batch);
        }
        self.resolve_workers.reap_finished();
        effect
    }

    fn on_shutdown(&mut self) {
        self.backend.on_shutdown();
        self.resolve_workers.reap_finished();
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolving_srt_shard_backend<P>(
    poller: P,
    feed: TsFeed,
    budget: WorkBudget,
    srt_egress_muxer_port_reuse: Option<super::muxer_ports::SrtEgressMuxerPortState>,
    drain_timeout: std::time::Duration,
    connect_admission: Option<std::sync::Arc<tokio::sync::Semaphore>>,
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
        connect_admission,
    )
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolving_srt_shard_backend_with_configurator<P, C>(
    poller: P,
    feed: TsFeed,
    budget: WorkBudget,
    socket_configurator: C,
    // This shard's application-owned UDP socket and logical caller table
    // (see `SrtShardBackend::with_srt_egress_muxer_port_reuse`).
    // `None` leaves reuse disabled (every existing test/no-config caller);
    // `Some` is the per-shard state minted by `SrtEgressMuxerPorts::shard`
    // in `factory.rs`/`engine_egress_fabric.rs`, so leaves on this shard
    // share one srt-rs socket/table and other shards get their own.
    srt_egress_muxer_port_reuse: Option<super::muxer_ports::SrtEgressMuxerPortState>,
    drain_timeout: std::time::Duration,
    // Engine-wide connect-concurrency admission control (see
    // `srt_connect_admission.rs`). `None` leaves connects unthrottled
    // (every existing test/no-config caller); `Some` is the one shared
    // handle from `MediaEngine::srt_egress_connect_admission_handle`.
    connect_admission: Option<std::sync::Arc<tokio::sync::Semaphore>>,
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
    .with_drain_timeout(drain_timeout)
    .with_connect_admission(connect_admission);
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
