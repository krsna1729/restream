//! Decorator that resolves each RTMP `Add`/`Update` destination on the
//! process resolver (`crate::resolver`), with the output's
//! `LeafPolicy::resolve_timeout` as its deadline. Every request is answered
//! (addresses, failure, timeout or a full resolver queue) into the wrapped
//! backend's completion queue, which it drains on its media tick; a failure
//! fails that output alone and its egress task retries it. The shard owns no
//! resolver thread, so its teardown never waits on DNS.

use std::sync::{Arc, mpsc::Sender};
use std::time::{Duration, Instant};

use crate::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::egress::journal::RingFeed;
use crate::egress::metrics::ShardMetrics;
use crate::egress::policy::WorkBudgetConfig;
use crate::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::resolver::{ResolveError, Resolver};
use restream_rtmp::parse_rtmp_url;

use super::rtmp_shard::{
    RtmpPublishStartupSource, RtmpReadinessPoller, RtmpResolvedConnect, RtmpShardBackend,
    rtmp_resolve_completion_queue,
};

struct RtmpResolveRequest {
    output_id: OutputId,
    generation: u64,
    host: String,
    port: u16,
    timeout: Duration,
}

pub(crate) struct ResolvingRtmpShardBackend<B> {
    backend: B,
    resolver: Arc<Resolver>,
    completions: Sender<RtmpResolvedConnect>,
}

impl<B> ResolvingRtmpShardBackend<B> {
    fn new(backend: B, resolver: Arc<Resolver>, completions: Sender<RtmpResolvedConnect>) -> Self {
        Self {
            backend,
            resolver,
            completions,
        }
    }

    fn resolve(&self, request: RtmpResolveRequest) {
        let RtmpResolveRequest {
            output_id,
            generation,
            host,
            port,
            timeout,
        } = request;
        let completions = self.completions.clone();
        self.resolver
            .submit(&host, port, Instant::now() + timeout, move |resolution| {
                // The receiver goes only with the shard, when no answer matters.
                let _ = completions.send(RtmpResolvedConnect {
                    output_id,
                    generation,
                    peer: resolution.and_then(|addresses| {
                        addresses.first().copied().ok_or(ResolveError::NoAddress)
                    }),
                });
            });
    }
}

impl<B> EgressShardBackend for ResolvingRtmpShardBackend<B>
where
    B: EgressShardBackend,
{
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        // The answer is drained on a later media tick, after the backend has
        // queued the pending connect it completes.
        if let Some(request) = resolve_request_from_command(&command) {
            self.resolve(request);
        }
        self.backend.on_command(command)
    }

    fn timer_generation(&self, output_id: &OutputId) -> Option<u64> {
        self.backend.timer_generation(output_id)
    }

    fn on_timer(&mut self, output_id: OutputId, generation: u64) -> EgressShardCommandEffect {
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
    ) -> crate::egress::shard::EgressShardIdleWake {
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

/// Build one RTMP shard backend whose lookups go to `resolver`
/// (`crate::resolver::shared()` in production).
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
    resolver: Arc<Resolver>,
) -> ResolvingRtmpShardBackendWithPoller<P, S>
where
    P: RtmpReadinessPoller,
    S: RtmpPublishStartupSource,
{
    let (completion_sender, completion_queue) = rtmp_resolve_completion_queue();
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
    ResolvingRtmpShardBackend::new(backend, resolver, completion_sender)
}

fn resolve_request_from_command(command: &EgressCommand) -> Option<RtmpResolveRequest> {
    match command {
        EgressCommand::Add(spec) | EgressCommand::Update(spec) => {
            resolve_request_from_output_spec(spec)
        }
        EgressCommand::Remove(_)
        | EgressCommand::Abandon(_)
        | EgressCommand::FeedWake
        | EgressCommand::DrainShard(_)
        | EgressCommand::Shutdown => None,
    }
}

fn resolve_request_from_output_spec(spec: &OutputSpec) -> Option<RtmpResolveRequest> {
    let ProtocolSpec::Rtmp { url, .. } = &spec.protocol else {
        return None;
    };
    let parts = parse_rtmp_url(url)?;
    Some(RtmpResolveRequest {
        output_id: spec.id.clone(),
        generation: spec.generation,
        host: parts.host,
        port: parts.port,
        timeout: spec.policy.resolve_timeout,
    })
}

#[cfg(test)]
mod tests;
