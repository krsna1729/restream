//! Off-loop SRT admission. A connect's ban check, pipeline lookup and
//! publisher/reader registration run as one task per peer, so a stalled
//! lookup or a contended engine lock delays only that peer, never the shared
//! session loop that serves every tenant's connects, disconnects, probe
//! answers and telemetry.
//!
//! A task is never aborted: `start_publisher` registers the ingest before it
//! returns, and an abort after that point would leave the key locked as a
//! duplicate publisher. A peer that disconnects meanwhile is only unmarked;
//! its task runs to completion and the loop finishes what it registered.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::hash::Hash;
use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use tokio::task::{Id, JoinError, JoinSet};
use tracing::{error, warn};

use super::{Ingress, PublisherSession, Session, SrtServer};
use crate::ingest_auth::PipelineAccessMode;
use crate::security::RateLimitScope;
use crate::srt::ingress_media::{SrtPublisherMedia, SrtReaderMedia};
use crate::srt::ingress_owner::{IngressCommand, IngressPeer};
use crate::srt_stream_id::{ParsedStreamId, SrtConnectionMode, parse_srt_stream_id};

/// Deadline for the pipeline lookup only: dropping it has no side effect.
/// Registration is never timed (see the module docs).
pub(in crate::srt) const AUTH_DEADLINE: Duration = Duration::from_secs(5);
/// In-flight admissions per client IP, counting tasks whose peer already left.
pub(in crate::srt) const MAX_ADMISSIONS_PER_IP: usize = 8;
/// In-flight publish admissions per parsed stream key (one pipeline, the
/// tenant), counted the same way: caps one pipeline's reconnect churn from
/// many IPs. Readers are not counted, so a viewer burst on a key is admitted.
pub(in crate::srt) const MAX_ADMISSIONS_PER_KEY: usize = 4;
/// In-flight admissions per listener, counted the same way.
const MAX_ADMISSIONS: usize = 256;

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
    ip: IpAddr,
    /// Set for publish admissions only.
    publish_key: Option<String>,
}

#[derive(Default)]
pub(super) struct Admissions {
    tasks: JoinSet<Admitted>,
    /// Admitting peers that are still connected, with their task. A
    /// disconnect removes the entry; that is the only cancellation.
    pending: HashMap<IngressPeer, Id>,
    /// Every unjoined task, including those whose peer already left.
    in_flight: HashMap<Id, InFlight>,
    per_ip: HashMap<IpAddr, usize>,
    per_publish_key: HashMap<String, usize>,
}

impl Admissions {
    fn for_ip(&self, ip: &IpAddr) -> usize {
        self.per_ip.get(ip).copied().unwrap_or(0)
    }

    fn for_publish_key(&self, stream_key: &str) -> usize {
        self.per_publish_key.get(stream_key).copied().unwrap_or(0)
    }

    fn has_room(&self, ip: &IpAddr, publish_key: Option<&str>) -> bool {
        self.for_ip(ip) < MAX_ADMISSIONS_PER_IP
            && publish_key.is_none_or(|key| self.for_publish_key(key) < MAX_ADMISSIONS_PER_KEY)
            && self.tasks.len() < MAX_ADMISSIONS
    }

    fn spawn(
        &mut self,
        peer: IngressPeer,
        ip: IpAddr,
        publish_key: Option<String>,
        admission: impl Future<Output = Admitted> + Send + 'static,
    ) {
        let id = self.tasks.spawn(admission).id();
        self.pending.insert(peer, id);
        *self.per_ip.entry(ip).or_default() += 1;
        if let Some(key) = &publish_key {
            *self.per_publish_key.entry(key.clone()).or_default() += 1;
        }
        self.in_flight.insert(
            id,
            InFlight {
                peer,
                ip,
                publish_key,
            },
        );
    }

    /// The peer disconnected: its task still completes, but nothing attaches.
    pub(super) fn abandon(&mut self, peer: &IngressPeer) {
        self.pending.remove(peer);
    }

    /// The next finished task. Cancel safe: everything after the join is
    /// synchronous.
    pub(super) async fn join_next(&mut self) -> Option<Joined> {
        let (id, outcome) = match self.tasks.join_next_with_id().await? {
            Ok((id, outcome)) => (id, Some(outcome)),
            Err(error) => {
                let id = error.id();
                report_failed(error);
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
        release(&mut self.per_ip, ip);
        if let Some(key) = publish_key {
            release(&mut self.per_publish_key, key);
        }
        match self.pending.entry(peer) {
            Entry::Occupied(pending) if *pending.get() == id => Some(pending.remove_entry().0),
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

fn report_failed(error: JoinError) {
    if error.is_panic() {
        let payload = error.into_panic();
        let message = crate::panic_boundary::record_contained(&*payload);
        error!(panic = message, "SRT admission task panicked");
    } else {
        error!(%error, "SRT admission task was cancelled");
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
                in_flight_for_stream = admissions.for_publish_key(&parsed.stream_key),
                in_flight = admissions.tasks.len(),
                "refusing SRT peer: too many admissions in flight"
            );
            return ingress.disconnect(logical_peer);
        }
        let publish_key = publish_key.cloned();
        admissions.spawn(
            logical_peer,
            ip,
            publish_key,
            self.clone().admit(peer, parsed),
        );
        Ok(())
    }

    /// The admission task: IP bans, the pipeline lookup (under its deadline),
    /// then publisher or reader registration.
    async fn admit(self: Arc<Self>, peer: SocketAddr, parsed: ParsedStreamId) -> Admitted {
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
        match parsed.mode {
            SrtConnectionMode::Publish => {
                match self
                    .start_publisher(peer, pipeline, parsed.stream_key)
                    .await
                {
                    Ok((session, media)) => Admitted::Publish(session, media),
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
    /// it registered when the peer already left. Called only with a free
    /// command slot, so the one command it may issue is accepted.
    pub(super) async fn apply_admission(
        &self,
        ingress: &mut Ingress,
        sessions: &mut HashMap<IngressPeer, Session>,
        Joined { peer, outcome }: Joined,
    ) -> Result<(), IngressCommand> {
        let Some(logical_peer) = peer else {
            if let Some(outcome) = outcome {
                self.discard_admission(outcome).await;
            }
            return Ok(());
        };
        match outcome {
            Some(Admitted::Publish(session, media)) => {
                sessions.insert(logical_peer, Session::Publish(session));
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
    pub(super) async fn drain_admissions(&self, admissions: &mut Admissions) {
        while let Some(Joined { outcome, .. }) = admissions.join_next().await {
            if let Some(outcome) = outcome {
                self.discard_admission(outcome).await;
            }
        }
    }

    /// Nothing attaches for a peer that is gone: a publisher's registration
    /// is finished, a reader's muxer lease releases on drop.
    async fn discard_admission(&self, outcome: Admitted) {
        if let Admitted::Publish(session, _media) = outcome {
            self.finish_publisher(session).await;
        }
    }
}
