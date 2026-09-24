use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, mpsc};
use std::thread::JoinHandle;

use super::owner_set::{SrtOwnerSettings, SrtOwners};
use super::{SrtResolveRequest, SrtResolvedConnect, SrtShardBackend, srt_resolve_completion_queue};
use crate::media::egress::command::{EgressCommand, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::metrics::ShardMetrics;
use crate::media::egress::policy::WorkBudgetConfig;
use crate::media::egress::shard::{
    EgressShardBackend, EgressShardCommandEffect, EgressShardIdleWake,
};
use crate::media::srt::SrtFabricEgressConnectSpec;
use std::sync::mpsc::SyncSender;

const SRT_RESOLVE_COMPLETION_QUEUE_CAPACITY: usize = 1024;
const SRT_RESOLVE_REQUEST_QUEUE_CAPACITY: usize = 1024;

pub(crate) type ResolvingSrtShardBackendDefault = ResolvingSrtShardBackend<SrtShardBackend>;

pub(crate) struct SrtResolveWorkerSet {
    request_sender: Option<SyncSender<SrtResolveRequest>>,
    pending: Arc<AtomicUsize>,
    worker: Option<JoinHandle<()>>,
}

impl SrtResolveWorkerSet {
    pub(crate) fn new(completion_sender: SyncSender<SrtResolvedConnect>) -> Self {
        let (request_sender, request_receiver) =
            mpsc::sync_channel::<SrtResolveRequest>(SRT_RESOLVE_REQUEST_QUEUE_CAPACITY);
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = Arc::clone(&pending);
        let worker = std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                let output_id = request.output_id.clone();
                let generation = request.generation;
                if super::resolve_srt_peer_hosts(request, completion_sender.clone()).is_err() {
                    // An empty address list is the bounded failure completion:
                    // the shard removes the matching pending connect instead
                    // of leaving an unresolved output resident forever.
                    let _ = completion_sender.send(SrtResolvedConnect {
                        output_id,
                        generation,
                        peer_addrs: Vec::new(),
                    });
                }
                worker_pending.fetch_sub(1, Ordering::Relaxed);
            }
        });
        Self {
            request_sender: Some(request_sender),
            pending,
            worker: Some(worker),
        }
    }

    /// Queue a bounded batch for the one resolver worker owned by this shard.
    fn spawn_batch(&mut self, requests: Vec<SrtResolveRequest>) {
        let Some(sender) = self.request_sender.as_ref() else {
            return;
        };
        for request in requests {
            self.pending.fetch_add(1, Ordering::Relaxed);
            if sender.try_send(request).is_err() {
                self.pending.fetch_sub(1, Ordering::Relaxed);
                break;
            }
        }
    }

    fn shutdown(&mut self) {
        self.request_sender.take();
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
    }
}

impl Drop for SrtResolveWorkerSet {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) struct ResolvingSrtShardBackend<B> {
    backend: B,
    resolve_workers: SrtResolveWorkerSet,
    /// Pending resolves buffered during `on_command` and flushed in
    /// `on_media_tick` into the shard's bounded resolver request queue.
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

    /// The wrapped shard backend, so tests can inspect what this decorator
    /// was actually constructed around.
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

    /// The idle wait belongs to the wrapped backend (it parks inside its
    /// Compio runtime); the default channel wait here would bypass it.
    fn wait_idle(
        &mut self,
        commands: &flume::Receiver<EgressCommand>,
        max_wait: std::time::Duration,
    ) -> EgressShardIdleWake {
        self.backend.wait_idle(commands, max_wait)
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
        effect
    }

    fn on_shutdown(&mut self) {
        self.backend.on_shutdown();
        self.resolve_workers.shutdown();
    }

    fn resync_count(&self) -> u64 {
        self.backend.resync_count()
    }

    fn budget_exhaustion_count(&self) -> u64 {
        self.backend.budget_exhaustion_count()
    }

    fn observe_metrics(&self, metrics: &mut ShardMetrics) {
        self.backend.observe_metrics(metrics);
    }
}

/// Build one SRT shard backend. Runs on the shard OS thread (it is the shard
/// factory's body) because it constructs the shard's Compio runtime; a
/// runtime that cannot be built is a typed error, not a panic.
pub(crate) fn resolving_srt_shard_backend(
    feed: TsFeed,
    budget: WorkBudgetConfig,
    drain_timeout: std::time::Duration,
    leaf_capacity: usize,
    owner_settings: SrtOwnerSettings,
) -> Result<ResolvingSrtShardBackendDefault, String> {
    let owners = SrtOwners::new(owner_settings)?;
    let (completion_sender, completion_queue) =
        srt_resolve_completion_queue(SRT_RESOLVE_COMPLETION_QUEUE_CAPACITY);
    let backend = SrtShardBackend::with_runtime_components(feed, budget, completion_queue, owners)
        .with_leaf_capacity(leaf_capacity)
        .with_drain_timeout(drain_timeout);
    Ok(ResolvingSrtShardBackend::new(
        backend,
        SrtResolveWorkerSet::new(completion_sender),
    ))
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
    let connect_spec = SrtFabricEgressConnectSpec::from_url(url, spec.policy.connect_timeout);
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
