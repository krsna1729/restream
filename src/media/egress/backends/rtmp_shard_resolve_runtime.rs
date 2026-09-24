//! Decorator that queues RTMP DNS resolution on `Add`/`Update`, mirroring
//! `src/media/egress/backends/srt/resolve_runtime.rs`'s
//! `ResolvingSrtShardBackend` shape exactly: one bounded resolver worker per
//! shard, reaped on shutdown.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, SyncSender};
use std::thread::JoinHandle;

use crate::media::egress::command::{EgressCommand, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::RingFeed;
use crate::media::egress::metrics::ShardMetrics;
use crate::media::egress::policy::WorkBudgetConfig;
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::rtmp::parse_rtmp_url;

use super::rtmp_shard::{
    RtmpPublishStartupSource, RtmpReadinessPoller, RtmpResolvedConnect, RtmpShardBackend,
    resolve_rtmp_peer_host, rtmp_resolve_completion_queue,
};

const RTMP_RESOLVE_COMPLETION_QUEUE_CAPACITY: usize = 1024;
const RTMP_RESOLVE_REQUEST_QUEUE_CAPACITY: usize = 1024;

struct RtmpResolveRequest {
    output_id: crate::media::egress::command::OutputId,
    generation: u64,
    host: String,
    port: u16,
}

struct RtmpResolveWorkerSet {
    request_sender: Option<SyncSender<RtmpResolveRequest>>,
    pending: Arc<AtomicUsize>,
    worker: Option<JoinHandle<()>>,
}

impl RtmpResolveWorkerSet {
    fn new(completion_sender: SyncSender<RtmpResolvedConnect>) -> Self {
        let (request_sender, request_receiver) =
            mpsc::sync_channel::<RtmpResolveRequest>(RTMP_RESOLVE_REQUEST_QUEUE_CAPACITY);
        let pending = Arc::new(AtomicUsize::new(0));
        let worker_pending = Arc::clone(&pending);
        let worker = std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                if let Some(peer_addr) = resolve_rtmp_peer_host(&request.host, request.port) {
                    let _ = completion_sender.try_send(RtmpResolvedConnect {
                        output_id: request.output_id,
                        generation: request.generation,
                        peer_addr,
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

    fn spawn(
        &mut self,
        output_id: crate::media::egress::command::OutputId,
        generation: u64,
        host: String,
        port: u16,
    ) -> bool {
        let Some(sender) = self.request_sender.as_ref() else {
            return false;
        };
        self.pending.fetch_add(1, Ordering::Relaxed);
        let request = RtmpResolveRequest {
            output_id,
            generation,
            host,
            port,
        };
        if sender.try_send(request).is_err() {
            self.pending.fetch_sub(1, Ordering::Relaxed);
            return false;
        }
        true
    }

    fn shutdown(&mut self) {
        self.request_sender.take();
        if let Some(worker) = self.worker.take()
            && worker.is_finished()
        {
            let _ = worker.join();
        }
    }

    #[cfg(test)]
    fn worker_count(&self) -> usize {
        self.pending.load(Ordering::Relaxed)
    }
}

impl Drop for RtmpResolveWorkerSet {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) struct ResolvingRtmpShardBackend<B> {
    backend: B,
    resolve_workers: RtmpResolveWorkerSet,
}

impl<B> ResolvingRtmpShardBackend<B> {
    fn new(backend: B, resolve_workers: RtmpResolveWorkerSet) -> Self {
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

impl<B> EgressShardBackend for ResolvingRtmpShardBackend<B>
where
    B: EgressShardBackend,
{
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        if let Some((output_id, generation, host, port)) = resolve_request_from_command(&command) {
            self.resolve_workers
                .spawn(output_id, generation, host, port);
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

    /// Preserve the wrapped Compio idle wait; the default channel-only wait
    /// bypasses its readiness runtime.
    fn wait_idle(
        &mut self,
        commands: &flume::Receiver<EgressCommand>,
        max_wait: std::time::Duration,
    ) -> crate::media::egress::shard::EgressShardIdleWake {
        self.backend.wait_idle(commands, max_wait)
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        self.backend.on_media_tick()
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

pub(crate) type ResolvingRtmpShardBackendWithPoller<P, S> =
    ResolvingRtmpShardBackend<RtmpShardBackend<P, S>>;

#[allow(clippy::too_many_arguments)]
pub(crate) fn resolving_rtmp_shard_backend<P, S>(
    poller: P,
    feed: RingFeed,
    budget: WorkBudgetConfig,
    chunk_size: u32,
    rtmps_client_config: std::sync::Arc<tokio_rustls::rustls::ClientConfig>,
    startup_source: S,
    drain_timeout: std::time::Duration,
    leaf_capacity: usize,
) -> ResolvingRtmpShardBackendWithPoller<P, S>
where
    P: RtmpReadinessPoller,
    S: RtmpPublishStartupSource,
{
    let (completion_sender, completion_queue) =
        rtmp_resolve_completion_queue(RTMP_RESOLVE_COMPLETION_QUEUE_CAPACITY);
    let backend = RtmpShardBackend::with_runtime_components(
        poller,
        feed,
        budget,
        chunk_size,
        rtmps_client_config,
        completion_queue,
        startup_source,
    )
    .with_leaf_capacity(leaf_capacity)
    .with_drain_timeout(drain_timeout);
    ResolvingRtmpShardBackend::new(backend, RtmpResolveWorkerSet::new(completion_sender))
}

fn resolve_request_from_command(
    command: &EgressCommand,
) -> Option<(crate::media::egress::command::OutputId, u64, String, u16)> {
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

fn resolve_request_from_output_spec(
    spec: &OutputSpec,
) -> Option<(crate::media::egress::command::OutputId, u64, String, u16)> {
    let ProtocolSpec::Rtmp { url, .. } = &spec.protocol else {
        return None;
    };
    let parts = parse_rtmp_url(url)?;
    Some((spec.id.clone(), spec.generation, parts.host, parts.port))
}

#[cfg(test)]
#[path = "rtmp_shard_resolve_runtime_tests.rs"]
mod tests;
