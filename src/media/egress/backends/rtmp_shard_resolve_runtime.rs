//! Decorator that queues RTMP DNS resolution on `Add`/`Update`.
//! The single bounded resolver worker reports success or failure for every
//! accepted request. On drop it stops queued lookups and joins after its
//! current system resolver call completes.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{
    Arc,
    mpsc::{self, SyncSender},
};
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
    stopping: Arc<AtomicBool>,
    worker: Option<JoinHandle<()>>,
}

impl RtmpResolveWorkerSet {
    fn new(completion_sender: SyncSender<RtmpResolvedConnect>) -> Self {
        let (request_sender, request_receiver) =
            mpsc::sync_channel::<RtmpResolveRequest>(RTMP_RESOLVE_REQUEST_QUEUE_CAPACITY);
        let stopping = Arc::new(AtomicBool::new(false));
        let worker_stopping = Arc::clone(&stopping);
        let worker = std::thread::spawn(move || {
            while let Ok(request) = request_receiver.recv() {
                if worker_stopping.load(Ordering::SeqCst) {
                    break;
                }
                // A blocking send backpressures the worker onto the bounded
                // request queue instead of dropping a completion, which would
                // leave its pending connect resident forever. The shard drops
                // its completion receiver before joining this worker.
                let completion = RtmpResolvedConnect {
                    peer_addr: resolve_rtmp_peer_host(&request.host, request.port),
                    output_id: request.output_id,
                    generation: request.generation,
                };
                if completion_sender.send(completion).is_err() {
                    break;
                }
            }
        });
        Self {
            request_sender: Some(request_sender),
            stopping,
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
        let request = RtmpResolveRequest {
            output_id,
            generation,
            host,
            port,
        };
        if sender.try_send(request).is_err() {
            return false;
        }
        true
    }

    fn shutdown(&mut self) {
        self.stopping.store(true, Ordering::SeqCst);
        self.request_sender.take();
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl Drop for RtmpResolveWorkerSet {
    fn drop(&mut self) {
        self.shutdown();
    }
}

pub(crate) struct ResolvingRtmpShardBackend<B> {
    // Keep the completion receiver before the worker set in drop order.
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
}

impl<B> EgressShardBackend for ResolvingRtmpShardBackend<B>
where
    B: EgressShardBackend,
{
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        let Some((output_id, generation, host, port)) = resolve_request_from_command(&command)
        else {
            return self.backend.on_command(command);
        };
        let progress = match &command {
            EgressCommand::Add(spec) | EgressCommand::Update(spec) => spec.progress.clone(),
            _ => unreachable!("only RTMP add/update commands request DNS"),
        };
        let rejected_output = output_id.clone();
        if !self
            .resolve_workers
            .spawn(output_id, generation, host, port)
        {
            tracing::warn!(
                output_id = %rejected_output,
                "rtmp fabric leaf rejected: resolver request queue is full"
            );
            self.backend
                .on_command(EgressCommand::Remove(rejected_output));
            progress.mark_terminated_unexpectedly();
            return EgressShardCommandEffect::Continue;
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
