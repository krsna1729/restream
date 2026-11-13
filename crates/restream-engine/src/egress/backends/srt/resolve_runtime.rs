//! Decorator that resolves each SRT `Add`/`Update`'s peer hosts (one, or a
//! bond's several, concurrently) on the process resolver
//! (`crate::resolver`), with the output's `LeafPolicy::resolve_timeout` as
//! the deadline. Each request gets exactly one completion (every address,
//! or the first failure, timeout or full resolver queue of any leg, without
//! waiting for the others) in the wrapped backend's queue, which it drains
//! on its media tick; a failure fails that output
//! alone and its egress task retries it. The shard owns no resolver thread,
//! so its teardown never waits on DNS.

use std::net::SocketAddr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::owner_set::{SrtOwnerSettings, SrtOwners};
use super::{SrtResolvedConnect, SrtShardBackend, srt_resolve_completion_queue};
use crate::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::egress::journal::TsFeed;
use crate::egress::metrics::ShardMetrics;
use crate::egress::policy::WorkBudgetConfig;
use crate::egress::shard::{EgressShardBackend, EgressShardCommandEffect, EgressShardIdleWake};
use crate::resolver::{Resolution, ResolveError, Resolver, split_host_port};
use crate::srt::egress_connect::egress_connect_spec;

pub(crate) type ResolvingSrtShardBackendDefault = ResolvingSrtShardBackend<SrtShardBackend>;

pub(crate) struct ResolvingSrtShardBackend<B> {
    backend: B,
    resolver: Arc<Resolver>,
    completions: Sender<SrtResolvedConnect>,
}

impl<B> ResolvingSrtShardBackend<B> {
    pub(crate) fn new(
        backend: B,
        resolver: Arc<Resolver>,
        completions: Sender<SrtResolvedConnect>,
    ) -> Self {
        Self {
            backend,
            resolver,
            completions,
        }
    }

    /// The wrapped shard backend, so tests can inspect what this decorator
    /// was actually constructed around.
    #[cfg(test)]
    pub(crate) fn inner_backend(&self) -> &B {
        &self.backend
    }
}

struct SrtResolveRequest {
    output_id: OutputId,
    generation: u64,
    /// `host:port` per listed peer.
    peer_hosts: Vec<String>,
    deadline: Instant,
}

/// One output's answers, one slot per peer host. The first failure, or the
/// last address in, sends the one completion; later answers are ignored.
struct PeerAnswers {
    output_id: OutputId,
    generation: u64,
    remaining: usize,
    peers: Vec<Option<SocketAddr>>,
    completed: bool,
    completions: Sender<SrtResolvedConnect>,
}

impl PeerAnswers {
    fn record(&mut self, index: usize, resolution: Resolution) {
        if self.completed {
            return;
        }
        let peers = match resolution
            .and_then(|addresses| addresses.first().copied().ok_or(ResolveError::NoAddress))
        {
            Ok(address) => {
                self.peers[index] = Some(address);
                self.remaining -= 1;
                if self.remaining > 0 {
                    return;
                }
                Ok(self.peers.iter().flatten().copied().collect())
            }
            // One failing leg fails the bond: never wait for the others.
            Err(error) => Err(error),
        };
        self.completed = true;
        // The receiver goes only with the shard, when no answer matters.
        let _ = self.completions.send(SrtResolvedConnect {
            output_id: self.output_id.clone(),
            generation: self.generation,
            peers,
        });
    }
}

fn resolve_peers(
    resolver: &Resolver,
    request: SrtResolveRequest,
    completions: Sender<SrtResolvedConnect>,
) {
    let SrtResolveRequest {
        output_id,
        generation,
        peer_hosts,
        deadline,
    } = request;
    let answers = Arc::new(Mutex::new(PeerAnswers {
        output_id,
        generation,
        remaining: peer_hosts.len(),
        peers: vec![None; peer_hosts.len()],
        completed: false,
        completions,
    }));
    for (index, peer) in peer_hosts.iter().enumerate() {
        let answers = Arc::clone(&answers);
        let record =
            move |resolution| restream_sync::lock::lock(&answers).record(index, resolution);
        match split_host_port(peer) {
            Ok((host, port)) => resolver.submit(host, port, deadline, record),
            Err(error) => record(Err(error)),
        }
    }
}

impl<B> EgressShardBackend for ResolvingSrtShardBackend<B>
where
    B: EgressShardBackend,
{
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        // The answer is drained on a later media tick, after the backend has
        // queued the pending connect it completes.
        if let Some(request) = resolve_request_from_command(&command) {
            resolve_peers(&self.resolver, request, self.completions.clone());
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

/// Build one SRT shard backend whose lookups go to `resolver`
/// (`crate::resolver::shared()` in production). Runs on the shard OS thread
/// (it is the shard factory's body) because it constructs the shard's Compio
/// runtime; a runtime that cannot be built is a typed error, not a panic.
pub(crate) fn resolving_srt_shard_backend(
    feed: TsFeed,
    budget: WorkBudgetConfig,
    drain_timeout: std::time::Duration,
    leaf_capacity: usize,
    owner_settings: SrtOwnerSettings,
    resolver: Arc<Resolver>,
) -> Result<ResolvingSrtShardBackendDefault, String> {
    let owners = SrtOwners::new(owner_settings)?;
    let (completion_sender, completion_queue) = srt_resolve_completion_queue();
    let backend = SrtShardBackend::with_runtime_components(feed, budget, completion_queue, owners)
        .with_leaf_capacity(leaf_capacity)
        .with_drain_timeout(drain_timeout);
    Ok(ResolvingSrtShardBackend::new(
        backend,
        resolver,
        completion_sender,
    ))
}

fn resolve_request_from_command(command: &EgressCommand) -> Option<SrtResolveRequest> {
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

fn resolve_request_from_output_spec(spec: &OutputSpec) -> Option<SrtResolveRequest> {
    let ProtocolSpec::Srt { url } = &spec.protocol else {
        return None;
    };
    let connect_spec = egress_connect_spec(url, spec.policy.connect_timeout);
    let peer_hosts = connect_spec.peer_hosts().to_vec();
    if peer_hosts.is_empty() {
        return None;
    }
    Some(SrtResolveRequest {
        output_id: spec.id.clone(),
        generation: spec.generation,
        peer_hosts,
        deadline: Instant::now() + spec.policy.resolve_timeout,
    })
}
