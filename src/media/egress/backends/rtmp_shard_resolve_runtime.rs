#![allow(dead_code)]

//! Decorator that spawns RTMP DNS resolution on `Add`/`Update`, mirroring
//! `src/media/egress/backends/srt/resolve_runtime.rs`'s
//! `ResolvingSrtShardBackend` shape exactly: one resolve worker thread per
//! pending connect, reaped on the next `on_media_tick`.

use std::sync::mpsc::SyncSender;
use std::thread::JoinHandle;

use crate::media::egress::command::{EgressCommand, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::RingFeed;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::rtmp::parse_rtmp_url;

use super::rtmp_shard::{
    RtmpPublishStartupSource, RtmpReadinessPoller, RtmpResolveCompletionQueue,
    RtmpResolveWorkerError, RtmpResolvedConnect, RtmpShardBackend, rtmp_resolve_completion_queue,
    spawn_rtmp_resolve_worker,
};

const RTMP_RESOLVE_COMPLETION_QUEUE_CAPACITY: usize = 1024;

#[derive(Debug)]
struct RtmpResolveWorkerSet {
    completion_sender: SyncSender<RtmpResolvedConnect>,
    workers: Vec<JoinHandle<Result<(), RtmpResolveWorkerError>>>,
}

impl RtmpResolveWorkerSet {
    fn new(completion_sender: SyncSender<RtmpResolvedConnect>) -> Self {
        Self {
            completion_sender,
            workers: Vec::new(),
        }
    }

    fn spawn(
        &mut self,
        output_id: crate::media::egress::command::OutputId,
        generation: u64,
        host: String,
        port: u16,
    ) {
        self.workers.push(spawn_rtmp_resolve_worker(
            output_id,
            generation,
            host,
            port,
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
    fn worker_count(&self) -> usize {
        self.workers.len()
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

pub(crate) type ResolvingRtmpShardBackendWithPoller<P, S> =
    ResolvingRtmpShardBackend<RtmpShardBackend<P, RtmpResolveCompletionQueue, S>>;

pub(crate) fn resolving_rtmp_shard_backend<P, S>(
    poller: P,
    feed: RingFeed,
    budget: WorkBudget,
    chunk_size: u32,
    rtmps_client_config: std::sync::Arc<tokio_rustls::rustls::ClientConfig>,
    startup_source: S,
    drain_timeout: std::time::Duration,
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
