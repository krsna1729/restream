//! Off-loop SRT admission. A connect's ban check, pipeline lookup and
//! publisher/reader registration, and an `srt_call` caller's registration,
//! run as one task per peer, so a stalled lookup or a contended registration
//! delays only that peer's admission, not the shared session loop. An
//! attached publisher's later lifecycle runs in its own task too
//! (`peer_work`).
//!
//! A task is never aborted, so a registration it made ends in order: a
//! panicking task drops its ingest lease, which releases the attempt as
//! abandoned (`crate::ingest_lease`). A peer that disconnects meanwhile is
//! marked as gone: its task registers nothing once its lookup returns,
//! finishes a registration that raced the mark itself, and anything that
//! raced even that is finished in a task of its own. An attached publisher's
//! disconnect cancels its attempt at once (`peer_work`), so a reconnect on
//! the same key is not refused as a duplicate.
//!
//! Publishers and readers of one key are admitted concurrently, so a reader
//! whose task overtakes its publisher's registration is refused with "no
//! active ingest" and retries.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use tokio::task::{Id, JoinSet};
use tracing::{info, warn};

use super::peer_work::PeerWork;
use super::{Ingress, PublisherSession, Session, SrtServer};
use crate::ingest_auth::{AuthenticatedPipeline, PipelineAccessMode};
use crate::security::RateLimitScope;
use crate::srt::calls::CallerPublisher;
use crate::srt::ingress_media::{SrtPublisherMedia, SrtReaderMedia};
use crate::srt::ingress_owner::{IngressCommand, IngressPeer};
use crate::srt_stream_id::{ParsedStreamId, SrtConnectionMode, parse_srt_stream_id};

/// Deadline for the pipeline lookup only: dropping it has no side effect.
/// Registration is never timed (see the module docs).
pub(in crate::srt) const AUTH_DEADLINE: Duration = Duration::from_secs(5);
/// In-flight publish admissions per parsed stream key (one pipeline, the
/// tenant), counting tasks whose peer already left: caps one pipeline's
/// reconnect churn from many IPs. Readers are not counted, so a viewer burst
/// on a key is admitted.
pub(in crate::srt) const MAX_ADMISSIONS_PER_KEY: usize = 4;

/// What an admission task produced, for the session loop to apply.
pub(super) enum Admitted {
    Publish(PublisherSession, Box<SrtPublisherMedia>),
    Read {
        pipeline_id: String,
        reader: Box<SrtReaderMedia>,
    },
    Rejected,
}

/// One finished admission task. `peer` is set only while the peer is still
/// connected; `outcome` is `None` when the task panicked.
pub(super) struct Joined {
    peer: Option<IngressPeer>,
    outcome: Option<Admitted>,
}

struct InFlight {
    peer: IngressPeer,
    /// Unset for calls: the engine placed them, so no client cap applies.
    ip: Option<IpAddr>,
    /// Set for publish admissions only.
    publish_key: Option<String>,
}

/// A still-connected admitting peer: its task, and the mark `abandon` sets.
struct Pending {
    task: Id,
    gone: Arc<AtomicBool>,
}

/// In-flight admissions and their bounds. The caps bound tasks whose peer
/// already left, the one thing the Owner's own peer limits cannot: while a
/// peer is connected the Owner already counts it. So they equal those
/// limits.
pub(super) struct Admissions {
    tasks: JoinSet<Admitted>,
    /// Admitting peers that are still connected. A disconnect removes the
    /// entry and marks the task; that is the only cancellation.
    pending: HashMap<IngressPeer, Pending>,
    /// Every unjoined task, including those whose peer already left.
    in_flight: HashMap<Id, InFlight>,
    per_ip: HashMap<IpAddr, usize>,
    per_publish_key: HashMap<String, usize>,
    /// The Owner's peers per client IP (`RESTREAM_SRT_MAX_PEERS_PER_IP`).
    max_per_ip: usize,
    /// The Owner's peer table size (srt-rs `PeerTableConfig::max_peers`,
    /// which the ingress listener leaves at its default).
    max_total: usize,
}

impl Admissions {
    pub(super) fn new(max_peers_per_ip: usize) -> Self {
        let max_total = srt_transport::advanced::admission::PeerTableConfig::default().max_peers;
        Self {
            tasks: JoinSet::new(),
            pending: HashMap::new(),
            in_flight: HashMap::new(),
            per_ip: HashMap::new(),
            per_publish_key: HashMap::new(),
            // The same clamp the listener applies to its own per-IP limit.
            max_per_ip: max_peers_per_ip.clamp(1, max_total),
            max_total,
        }
    }

    fn for_ip(&self, ip: &IpAddr) -> usize {
        self.per_ip.get(ip).copied().unwrap_or(0)
    }

    fn for_publish_key(&self, stream_key: &str) -> usize {
        self.per_publish_key.get(stream_key).copied().unwrap_or(0)
    }

    fn has_room(&self, ip: &IpAddr, publish_key: Option<&str>) -> bool {
        self.for_ip(ip) < self.max_per_ip
            && publish_key.is_none_or(|key| self.for_publish_key(key) < MAX_ADMISSIONS_PER_KEY)
            && self.tasks.len() < self.max_total
    }

    fn spawn(
        &mut self,
        peer: IngressPeer,
        ip: Option<IpAddr>,
        publish_key: Option<String>,
        gone: Arc<AtomicBool>,
        admission: impl Future<Output = Admitted> + Send + 'static,
    ) {
        let task = self.tasks.spawn(admission).id();
        self.pending.insert(peer, Pending { task, gone });
        if let Some(ip) = ip {
            *self.per_ip.entry(ip).or_default() += 1;
        }
        if let Some(key) = &publish_key {
            *self.per_publish_key.entry(key.clone()).or_default() += 1;
        }
        self.in_flight.insert(
            task,
            InFlight {
                peer,
                ip,
                publish_key,
            },
        );
    }

    /// The peer disconnected: its task still completes, but registers nothing
    /// past this point and nothing attaches.
    pub(super) fn abandon(&mut self, peer: &IngressPeer) {
        if let Some(pending) = self.pending.remove(peer) {
            pending.gone.store(true, Ordering::Release);
        }
    }

    /// The next finished task. Cancel safe: everything after the join is
    /// synchronous.
    pub(super) async fn join_next(&mut self) -> Option<Joined> {
        let (id, outcome) = match self.tasks.join_next_with_id().await? {
            Ok((id, outcome)) => (id, Some(outcome)),
            Err(error) => {
                let id = error.id();
                super::report_failed("admission", error);
                (id, None)
            }
        };
        let peer = self.settle(id);
        Some(Joined { peer, outcome })
    }

    /// Release the task's bound slots; the peer if it is still connected and
    /// this task is still its admission.
    fn settle(&mut self, id: Id) -> Option<IngressPeer> {
        let InFlight {
            peer,
            ip,
            publish_key,
        } = self.in_flight.remove(&id)?;
        if let Some(ip) = ip {
            release(&mut self.per_ip, ip);
        }
        if let Some(key) = publish_key {
            release(&mut self.per_publish_key, key);
        }
        match self.pending.entry(peer) {
            Entry::Occupied(pending) if pending.get().task == id => Some(pending.remove_entry().0),
            _ => None,
        }
    }
}

fn release<K: Eq + Hash>(counts: &mut HashMap<K, usize>, key: K) {
    if let Entry::Occupied(mut count) = counts.entry(key) {
        *count.get_mut() -= 1;
        if *count.get() == 0 {
            count.remove();
        }
    }
}

impl SrtServer {
    /// Admit a connected peer off the loop, or refuse it at once when its IP,
    /// its stream key (publishers only) or the listener already has the
    /// maximum admissions in flight.
    pub(super) fn start_admission(
        self: &Arc<Self>,
        ingress: &mut Ingress,
        admissions: &mut Admissions,
        peer: SocketAddr,
        logical_peer: IngressPeer,
        stream_id: String,
    ) -> Result<(), IngressCommand> {
        let parsed = parse_srt_stream_id(&stream_id);
        let ip = peer.ip();
        let publish_key = (parsed.mode == SrtConnectionMode::Publish).then_some(&parsed.stream_key);
        if !admissions.has_room(&ip, publish_key.map(String::as_str)) {
            warn!(
                peer = %peer,
                in_flight_for_ip = admissions.for_ip(&ip),
                in_flight_for_stream = publish_key.map(|key| admissions.for_publish_key(key)),
                in_flight = admissions.tasks.len(),
                "refusing SRT peer: too many admissions in flight"
            );
            return ingress.disconnect(logical_peer);
        }
        let publish_key = publish_key.cloned();
        let gone = Arc::new(AtomicBool::new(false));
        let admission = self.clone().admit(peer, parsed, gone.clone());
        admissions.spawn(logical_peer, Some(ip), publish_key, gone, admission);
        Ok(())
    }

    /// A call connected: register it as a publisher of its own input off the
    /// loop, with no stream-key or IP checks (the engine placed the call).
    pub(super) fn start_call(
        self: &Arc<Self>,
        admissions: &mut Admissions,
        peer: SocketAddr,
        logical_peer: IngressPeer,
        caller: CallerPublisher,
    ) {
        let gone = Arc::new(AtomicBool::new(false));
        let admission = self.clone().admit_call(peer, caller, gone.clone());
        admissions.spawn(logical_peer, None, None, gone, admission);
    }

    async fn admit_call(
        self: Arc<Self>,
        peer: SocketAddr,
        caller: CallerPublisher,
        gone: Arc<AtomicBool>,
    ) -> Admitted {
        let input_id = caller.pipeline.input_id.clone();
        match self
            .register_publisher(peer, caller.pipeline, caller.stream_key, &gone)
            .await
        {
            Ok(admitted) => {
                if matches!(admitted, Admitted::Publish(..)) {
                    info!(peer = %peer, input_id = %input_id, "SRT call connected");
                }
                admitted
            }
            Err(error) => {
                warn!(peer = %peer, input_id = %input_id, %error, "hanging up SRT call");
                Admitted::Rejected
            }
        }
    }

    /// Register a publisher; one whose peer left meanwhile is finished at
    /// once.
    async fn register_publisher(
        &self,
        peer: SocketAddr,
        pipeline: AuthenticatedPipeline,
        stream_key: String,
        gone: &AtomicBool,
    ) -> Result<Admitted, String> {
        let (session, media) = self.start_publisher(peer, pipeline, stream_key).await?;
        if gone.load(Ordering::Acquire) {
            self.finish_publisher(session).await;
            return Ok(Admitted::Rejected);
        }
        Ok(Admitted::Publish(session, media))
    }

    /// The admission task: IP bans, the pipeline lookup (under its deadline),
    /// then publisher or reader registration, unless the peer is `gone`.
    async fn admit(
        self: Arc<Self>,
        peer: SocketAddr,
        parsed: ParsedStreamId,
        gone: Arc<AtomicBool>,
    ) -> Admitted {
        let client_ip = peer.ip().to_string();
        let access_mode = match parsed.mode {
            SrtConnectionMode::Publish => PipelineAccessMode::SrtPublish,
            SrtConnectionMode::Read => PipelineAccessMode::SrtRead,
        };
        if self
            .security
            .is_ip_banned_for(RateLimitScope::SrtPublish, &client_ip)
            .or_else(|| {
                self.security
                    .is_ip_banned_for(RateLimitScope::SrtRead, &client_ip)
            })
            .is_some()
        {
            return Admitted::Rejected;
        }
        let lookup = self
            .pipeline_access
            .authenticate(access_mode, &parsed.stream_key, &client_ip);
        let pipeline = match tokio::time::timeout(AUTH_DEADLINE, lookup).await {
            Ok(Ok(pipeline)) => pipeline,
            Ok(Err(error)) => {
                warn!(peer = %peer, error = ?error, "rejecting unauthorized SRT stream");
                return Admitted::Rejected;
            }
            Err(_) => {
                warn!(
                    peer = %peer,
                    deadline_ms = AUTH_DEADLINE.as_millis() as u64,
                    "rejecting SRT peer: pipeline lookup exceeded its deadline"
                );
                return Admitted::Rejected;
            }
        };
        // A registration for a peer that already left is never needed, and
        // until it is finished a reconnect on the key is a duplicate.
        if gone.load(Ordering::Acquire) {
            return Admitted::Rejected;
        }
        match parsed.mode {
            SrtConnectionMode::Publish => {
                match self
                    .register_publisher(peer, pipeline, parsed.stream_key, &gone)
                    .await
                {
                    Ok(admitted) => admitted,
                    Err(error) => {
                        warn!(peer = %peer, %error, "rejecting SRT publisher");
                        Admitted::Rejected
                    }
                }
            }
            SrtConnectionMode::Read => match self.start_reader(&pipeline.id).await {
                Ok(reader) => Admitted::Read {
                    pipeline_id: pipeline.id,
                    reader: Box::new(reader),
                },
                Err(error) => {
                    warn!(peer = %peer, %error, "rejecting SRT reader");
                    Admitted::Rejected
                }
            },
        }
    }

    /// Apply a finished admission to its still-connected peer, or finish what
    /// it registered, off the loop, when the peer already left. Called only
    /// with a free command slot, so the one command it may issue is accepted.
    pub(super) fn apply_admission(
        self: &Arc<Self>,
        ingress: &mut Ingress,
        sessions: &mut HashMap<IngressPeer, Session>,
        peers: &mut PeerWork,
        Joined { peer, outcome }: Joined,
    ) -> Result<(), IngressCommand> {
        let Some(logical_peer) = peer else {
            if let Some(outcome) = outcome {
                self.discard_admission(peers, outcome);
            }
            return Ok(());
        };
        match outcome {
            Some(Admitted::Publish(session, media)) => {
                let publisher = peers.attach(self, logical_peer, session);
                sessions.insert(logical_peer, Session::Publish(publisher));
                ingress.command(IngressCommand::AttachPublisher {
                    logical_peer,
                    media,
                })
            }
            Some(Admitted::Read {
                pipeline_id,
                reader,
            }) => {
                sessions.insert(
                    logical_peer,
                    Session::Read {
                        pipeline_id,
                        closing: false,
                    },
                );
                ingress.command(IngressCommand::AttachReader {
                    logical_peer,
                    reader,
                })
            }
            Some(Admitted::Rejected) | None => ingress.disconnect(logical_peer),
        }
    }

    /// Listener exit: every late admission runs to completion and whatever it
    /// registered is finished, so a restart on the same engine never inherits
    /// a registration. The Owner is already shut down, so nothing attaches.
    pub(super) async fn drain_admissions(
        self: &Arc<Self>,
        admissions: &mut Admissions,
        peers: &mut PeerWork,
    ) {
        while let Some(Joined { outcome, .. }) = admissions.join_next().await {
            if let Some(outcome) = outcome {
                self.discard_admission(peers, outcome);
            }
        }
    }

    /// Nothing attaches for a peer that is gone: a publisher's registration
    /// is finished (in its own task), a reader's muxer lease releases on
    /// drop.
    fn discard_admission(self: &Arc<Self>, peers: &mut PeerWork, outcome: Admitted) {
        if let Admitted::Publish(session, _media) = outcome {
            peers.finish(self, session);
        }
    }
}
