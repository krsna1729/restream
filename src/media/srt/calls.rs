//! SRT caller inputs (`srt_call`): the engine dials a remote SRT listener and
//! receives its stream, instead of waiting for a publisher to call in.
//!
//! The reconciler writes the wanted calls ([`SrtCallInputs::converge`]) from
//! the input rows; the SRT server's Tokio loop supervises them
//! ([`CallSupervisor`]): resolve the host, ask ingress Owner 0 to dial on its
//! caller socket, and once the call connects, run it as a publisher of its
//! input exactly like a listener peer. A failed or ended call is redialled
//! with capped exponential backoff while its row still wants it.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use super::egress_connect::egress_connect_spec;
use super::ingress_bridge::{CallToken, IngressCommand, IngressPeer};
use std::hash::Hash;

/// How long one call's handshake may take before it counts as failed.
const CALL_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
/// How long resolving the listener's host may take.
const RESOLVE_TIMEOUT: Duration = Duration::from_secs(5);
const BACKOFF_FIRST: Duration = Duration::from_secs(1);
const BACKOFF_MAX: Duration = Duration::from_secs(30);
/// The ingress Owner whose caller side dials every call. Its caller side
/// binds an IPv4 and an IPv6 socket as each family is first needed.
const CALL_OWNER: u16 = 0;

pub use restream_srt::calls::{SrtCallInputs, SrtCallSpec, validate_srt_call_url};

/// The next wait after `failures` consecutive failed attempts (at least one).
fn backoff(failures: u32) -> Duration {
    let doublings = failures.saturating_sub(1).min(16);
    BACKOFF_FIRST
        .saturating_mul(1 << doublings)
        .min(BACKOFF_MAX)
}

enum Phase<P> {
    /// Redial at the instant.
    Waiting(Instant),
    Resolving,
    /// Asked the Owner to dial; the session is known once the pool admits it.
    Dialing(Option<P>),
    /// Connected and publishing.
    Up(P),
}

struct Call<P> {
    spec: SrtCallSpec,
    token: CallToken,
    phase: Phase<P>,
    failures: u32,
}

/// What the server loop does for a connected call.
pub(super) struct CallerPublisher {
    pub(super) pipeline: crate::media::ingest_auth::AuthenticatedPipeline,
    pub(super) stream_key: String,
}

/// Every address each listed host resolved to, in listed order.
pub(super) type Resolved = (CallToken, Result<Vec<Vec<SocketAddr>>, String>);

/// The SRT server's per-run call state. Every method that may command the
/// Owner first checks the bridge's one command slot is free and otherwise
/// leaves its work for the next tick, so it never displaces an event reply.
/// Generic over the session key (`IngressPeer` in production) so the state
/// machine is testable without minting transport ids.
pub(super) struct CallSupervisor<P = IngressPeer> {
    next_token: u64,
    seen_generation: u64,
    calls: HashMap<String, Call<P>>,
    by_token: HashMap<CallToken, String>,
    by_peer: HashMap<P, String>,
    resolved_tx: tokio::sync::mpsc::UnboundedSender<Resolved>,
    pub(super) resolved: tokio::sync::mpsc::UnboundedReceiver<Resolved>,
}

impl<P: Copy + Eq + Hash> CallSupervisor<P> {
    pub(super) fn new() -> Self {
        let (resolved_tx, resolved) = tokio::sync::mpsc::unbounded_channel();
        Self {
            next_token: 1,
            seen_generation: 0,
            calls: HashMap::new(),
            by_token: HashMap::new(),
            by_peer: HashMap::new(),
            resolved_tx,
            resolved,
        }
    }

    /// Follow the wanted set, and start every call whose wait is over.
    /// `disconnect` hangs up a session; it returns false when the command
    /// slot is taken, and the rest of the work waits for the next tick.
    pub(super) fn tick(
        &mut self,
        inputs: &SrtCallInputs,
        now: Instant,
        mut disconnect: impl FnMut(P) -> bool,
    ) {
        if let Some((generation, wanted)) = inputs.snapshot_since(self.seen_generation) {
            let stale: Vec<String> = self
                .calls
                .iter()
                .filter(|(id, call)| wanted.get(*id) != Some(&call.spec))
                .map(|(id, _)| id.clone())
                .collect();
            for input_id in stale {
                let peer = match self.calls.get(&input_id).map(|call| &call.phase) {
                    Some(Phase::Dialing(Some(peer)) | Phase::Up(peer)) => Some(*peer),
                    _ => None,
                };
                if let Some(peer) = peer {
                    if !disconnect(peer) {
                        return;
                    }
                    self.by_peer.remove(&peer);
                }
                if let Some(call) = self.calls.remove(&input_id) {
                    self.by_token.remove(&call.token);
                }
            }
            for (input_id, spec) in wanted {
                self.calls.entry(input_id).or_insert_with(|| Call {
                    spec,
                    token: CallToken(0),
                    phase: Phase::Waiting(now),
                    failures: 0,
                });
            }
            self.seen_generation = generation;
        }
        for (input_id, call) in &mut self.calls {
            let Phase::Waiting(at) = call.phase else {
                continue;
            };
            if at > now {
                continue;
            }
            self.by_token.remove(&call.token);
            call.token = CallToken(self.next_token);
            self.next_token += 1;
            self.by_token.insert(call.token, input_id.clone());
            call.phase = Phase::Resolving;
            let spec = egress_connect_spec(&call.spec.url, CALL_CONNECT_TIMEOUT);
            let hosts = spec.peer_hosts().to_vec();
            let token = call.token;
            let tx = self.resolved_tx.clone();
            tokio::spawn(async move {
                let _ = tx.send((token, resolve(hosts).await));
            });
        }
    }

    /// A resolution finished: the Owner command to dial, or `None` after
    /// recording the failure.
    pub(super) fn resolved(
        &mut self,
        (token, result): Resolved,
        now: Instant,
    ) -> Option<IngressCommand> {
        let call = self.current(token)?;
        let request = result.and_then(|hosts| {
            let addrs = place(&hosts)?;
            let request = egress_connect_spec(&call.spec.url, CALL_CONNECT_TIMEOUT)
                .connect_request(&addrs)?;
            Ok((addrs[0], request))
        });
        match request {
            Ok((remote, request)) => {
                call.phase = Phase::Dialing(None);
                Some(IngressCommand::Call {
                    owner: CALL_OWNER,
                    token,
                    remote,
                    request: Box::new(request),
                })
            }
            Err(reason) => {
                self.failed(token, &reason, now);
                None
            }
        }
    }

    /// The pool admitted a call. `false`: the call is no longer wanted and
    /// the session must be hung up.
    pub(super) fn started(&mut self, token: CallToken, peer: P) -> bool {
        let Some(call) = self.current(token) else {
            return false;
        };
        call.phase = Phase::Dialing(Some(peer));
        let input_id = call.spec.input_id.clone();
        self.by_peer.insert(peer, input_id);
        true
    }

    /// The call could not be placed, or a wanted call's attempt ended.
    pub(super) fn failed(&mut self, token: CallToken, reason: &str, now: Instant) {
        let Some(call) = self.current(token) else {
            return;
        };
        call.failures += 1;
        let wait = backoff(call.failures);
        tracing::warn!(input_id = %call.spec.input_id, %reason, retry_in_ms = wait.as_millis() as u64,
            "SRT call failed");
        call.phase = Phase::Waiting(now + wait);
    }

    /// The session connected: the publisher to start for it, when it is a
    /// wanted call's.
    pub(super) fn connected(&mut self, peer: P) -> Option<CallerPublisher> {
        let input_id = self.by_peer.get(&peer)?;
        let call = self.calls.get_mut(input_id)?;
        call.phase = Phase::Up(peer);
        call.failures = 0;
        Some(CallerPublisher {
            pipeline: crate::media::ingest_auth::AuthenticatedPipeline {
                id: call.spec.pipeline_id.clone(),
                input_id: call.spec.input_id.clone(),
                selected: call.spec.selected,
            },
            stream_key: call.spec.stream_key.clone(),
        })
    }

    /// Whether `peer` is a call's session (else it is a listener peer).
    pub(super) fn owns(&self, peer: &P) -> bool {
        self.by_peer.contains_key(peer)
    }

    /// The session ended: redial after the backoff.
    pub(super) fn disconnected(&mut self, peer: P, reason: &str, now: Instant) {
        let Some(input_id) = self.by_peer.remove(&peer) else {
            return;
        };
        let Some(call) = self.calls.get(&input_id) else {
            return;
        };
        let token = call.token;
        let was_up = matches!(call.phase, Phase::Up(current) if current == peer);
        let dialing = matches!(call.phase, Phase::Dialing(Some(current)) if current == peer);
        if was_up {
            // A call that ran redials promptly, then backs off if it keeps failing.
            if let Some(call) = self.calls.get_mut(&input_id) {
                call.failures = 0;
            }
        }
        if was_up || dialing {
            self.failed(token, reason, now);
        }
    }

    fn current(&mut self, token: CallToken) -> Option<&mut Call<P>> {
        let input_id = self.by_token.get(&token)?;
        self.calls
            .get_mut(input_id)
            .filter(|call| call.token == token)
    }
}

/// Choose the address each listed host is dialled at: IPv4 when every host
/// has an IPv4 address, else IPv6 when every one has an IPv6 address. A
/// bond's legs negotiate one payload size, so they share one family.
fn place(hosts: &[Vec<SocketAddr>]) -> Result<Vec<SocketAddr>, String> {
    let pick = |v6: bool| -> Option<Vec<SocketAddr>> {
        hosts
            .iter()
            .map(|addrs| addrs.iter().copied().find(|addr| addr.is_ipv6() == v6))
            .collect()
    };
    pick(false).or_else(|| pick(true)).ok_or_else(|| {
        "the listed hosts share no address family; every leg of a call must be reachable \
         over IPv4, or every leg over IPv6"
            .to_string()
    })
}

/// Every address of every listed host. The system resolver blocks, so it
/// runs on the blocking pool (never on a Tokio worker), bounded by
/// `RESOLVE_TIMEOUT` per host.
async fn resolve(hosts: Vec<String>) -> Result<Vec<Vec<SocketAddr>>, String> {
    let mut resolved = Vec::with_capacity(hosts.len());
    for host in hosts {
        let lookup = host.clone();
        let addrs: Vec<SocketAddr> = tokio::time::timeout(
            RESOLVE_TIMEOUT,
            tokio::task::spawn_blocking(move || {
                std::net::ToSocketAddrs::to_socket_addrs(lookup.as_str())
                    .map(Iterator::collect::<Vec<_>>)
            }),
        )
        .await
        .map_err(|_| format!("resolving {host} timed out"))?
        .map_err(|_| format!("resolving {host} was interrupted"))?
        .map_err(|error| format!("resolving {host}: {error}"))?;
        if addrs.is_empty() {
            return Err(format!("{host} resolved to no address"));
        }
        resolved.push(addrs);
    }
    Ok(resolved)
}

#[cfg(test)]
#[path = "calls_tests.rs"]
mod tests;
