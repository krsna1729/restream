//! Peer resolution for a pending SRT connect, and the production backend
//! constructor that wires it to the process resolver (`crate::resolver`).
//!
//! A pending connect's peer hosts (one, or a bond's several, concurrently)
//! resolve with the output's `LeafPolicy::resolve_timeout` as the deadline.
//! Each request gets exactly one completion (every address, or the first
//! failure, timeout or full resolver queue of any leg, without waiting for
//! the others) in the backend's queue, which it drains on its media tick; a
//! failure fails that output alone and its egress task retries it. The
//! shard owns no resolver thread, so its teardown never waits on DNS.

use std::net::SocketAddr;
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use super::{SrtOwnerSettings, SrtOwners, SrtResolvedConnect, SrtShardBackend};
use crate::egress::command::OutputId;
use crate::egress::journal::TsFeed;
use crate::egress::policy::WorkBudgetConfig;
use crate::resolver::{Resolution, ResolveError, Resolver, split_host_port};

/// Build one SRT shard backend whose lookups go to `resolver`
/// (`crate::resolver::shared()` in production). Runs on the shard OS thread
/// (it is the shard factory's body) because it constructs the shard's Compio
/// runtime; a runtime that cannot be built is a typed error, not a panic.
pub(crate) fn srt_shard_backend(
    feed: TsFeed,
    budget: WorkBudgetConfig,
    drain_timeout: std::time::Duration,
    leaf_capacity: usize,
    owner_settings: SrtOwnerSettings,
    resolver: Arc<Resolver>,
) -> Result<SrtShardBackend, String> {
    let owners = SrtOwners::new(owner_settings)?;
    Ok(SrtShardBackend::new(feed, budget, owners, Some(resolver))
        .with_leaf_capacity(leaf_capacity)
        .with_drain_timeout(drain_timeout))
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

/// Submits one lookup per `host:port` in `peer_hosts`; never blocks (see
/// `Resolver::submit`).
pub(super) fn resolve_peers(
    resolver: &Resolver,
    output_id: &OutputId,
    generation: u64,
    peer_hosts: &[String],
    deadline: Instant,
    completions: &Sender<SrtResolvedConnect>,
) {
    let answers = Arc::new(Mutex::new(PeerAnswers {
        output_id: output_id.clone(),
        generation,
        remaining: peer_hosts.len(),
        peers: vec![None; peer_hosts.len()],
        completed: false,
        completions: completions.clone(),
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
