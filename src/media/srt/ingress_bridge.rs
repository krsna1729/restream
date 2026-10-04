//! The Tokio-facing half of the SRT ingress Owners: the command and event
//! vocabulary, the lossy quality-telemetry channel, the start/exit types, and
//! [`SrtIngressHandle`], Tokio's end of the bounded bridges. The Owner threads
//! themselves live in `ingress_owner`.
//!
//! The listener is one Owner thread, or K when `RESTREAM_SRT_INGRESS_OWNERS`
//! asks for a `SO_REUSEPORT` group (one srt-rs `owner_plans` entry each). The
//! bridges carry session lifecycle only (admission, the one-time stream probe,
//! disconnect). Media runs to completion on the Owner that holds the session
//! (`ingress_media`); nothing per packet crosses any bridge.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use srt_transport::advanced::admission::LogicalPeerId;
use srt_transport::{ListenerTransfer, OwnerListenerPlan};
use tokio::sync::mpsc;

use crate::media::mpegts::DemuxProbe;
use crate::media::ring_buffer::RingBuffer;
use crate::media::snapshots::ListenerSocketStats;

use super::ingress_admission::ReceiverGroupId;
use super::ingress_media::{SrtPublisherMedia, SrtReaderMedia};
use super::ingress_owner::{listener_plans, run_owner_thread};
use super::ingress_quality::QualitySample;
use super::srt_policy::SrtIngestPolicyStore;

/// Tokio -> Owner command bridge capacity (per Owner).
pub(crate) const INGRESS_COMMAND_CAPACITY: usize = 256;

/// Owner -> Tokio lifecycle-event bridge capacity (connect, probe,
/// disconnect, fault), shared by every Owner.
pub(crate) const INGRESS_EVENT_CAPACITY: usize = 256;

/// Owner -> Tokio receive-quality telemetry bridge capacity. Unlike the command
/// and event bridges this one is LOSSY by design: a full bridge drops the sample
/// (counted in `telemetryDropped`) instead of ever delaying protocol service.
pub(crate) const INGRESS_TELEMETRY_CAPACITY: usize = 256;

/// One ingress session: the Owner thread that holds it and its id there. The
/// sole cross-thread session handle; no protocol object, table reference,
/// socket id or `SocketAddr` identifies a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct IngressPeer {
    pub(crate) owner: u16,
    pub(crate) id: LogicalPeerId,
}

pub(crate) enum IngressCommand {
    /// Tokio admitted the peer as a publisher; the Owner runs its media.
    AttachPublisher {
        logical_peer: IngressPeer,
        media: Box<SrtPublisherMedia>,
    },
    /// Tokio admitted the peer as a direct read/play reader.
    AttachReader {
        logical_peer: IngressPeer,
        reader: SrtReaderMedia,
    },
    /// Tokio applied the publisher's stream probe; `ring` replaces its ring.
    ProbeApplied {
        logical_peer: IngressPeer,
        ring: Option<Arc<RingBuffer>>,
    },
    /// Begin an orderly protocol disconnect of one peer. The owner retires the
    /// peer when its terminal event arrives (or after a short grace).
    Disconnect { logical_peer: IngressPeer },
    /// Disconnect nothing further; flush, drain the Owner and exit. Sent to
    /// every Owner by [`SrtIngressHandle::shutdown`] only.
    Shutdown,
}

impl IngressCommand {
    /// The Owner a session command is for.
    fn owner(&self) -> Option<u16> {
        match self {
            Self::AttachPublisher { logical_peer, .. }
            | Self::AttachReader { logical_peer, .. }
            | Self::ProbeApplied { logical_peer, .. }
            | Self::Disconnect { logical_peer } => Some(logical_peer.owner),
            Self::Shutdown => None,
        }
    }
}

/// Session-lifecycle vocabulary from the Owners to Tokio.
pub(crate) enum SrtIngressEvent {
    Connected {
        peer: SocketAddr,
        logical_peer: IngressPeer,
        stream_id: String,
    },
    /// A publisher's first stream probe. Its packets are held on the Owner
    /// until Tokio answers with [`IngressCommand::ProbeApplied`].
    Probe {
        logical_peer: IngressPeer,
        probe: DemuxProbe,
    },
    /// Terminal. The Owner has already flushed the publisher's media.
    Disconnected {
        peer: SocketAddr,
        logical_peer: IngressPeer,
        reason: String,
    },
    /// An Owner developed an OWNER-FATAL fault; the listener is gone.
    Fault { detail: String },
}

/// What the owner threads report when they exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IngressExit {
    /// `shutdown_and_drain` reached quiescence on every Owner (or there was
    /// nothing to drain).
    pub(crate) quiescent: bool,
    pub(crate) fault: Option<String>,
}

/// Why the owner threads could not start.
#[derive(Debug)]
pub(crate) struct IngressStartError(pub(crate) String);

impl std::fmt::Display for IngressStartError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// Everything the owner threads are built from.
#[derive(Clone)]
pub(crate) struct IngressConfig {
    pub(crate) bind: SocketAddr,
    pub(crate) policy_store: Arc<SrtIngestPolicyStore>,
    pub(crate) receiver_group: ReceiverGroupId,
    pub(crate) stats: Arc<ListenerSocketStats>,
    pub(crate) command_capacity: usize,
    pub(crate) event_capacity: usize,
    pub(crate) telemetry_capacity: usize,
    /// Owner threads sharing the port; 1 is one Owner on one socket.
    pub(crate) owners: usize,
    /// Peers one client IP may hold (srt-rs `max_peers_per_ip`).
    pub(crate) max_peers_per_ip: usize,
}

/// One Owner thread's place in the listener: its plan, the timestamp origin
/// every Owner shares (relocated sessions carry their timers), and the
/// transfer inboxes it receives from and sends to.
pub(crate) struct OwnerSeat {
    pub(crate) index: u16,
    pub(crate) plan: OwnerListenerPlan,
    pub(crate) epoch: Instant,
    pub(crate) inbox: flume::Receiver<ListenerTransfer>,
    pub(crate) members: Vec<flume::Sender<ListenerTransfer>>,
}

struct OwnerLink {
    commands: flume::Sender<IngressCommand>,
    thread: Option<std::thread::JoinHandle<IngressExit>>,
}

/// Tokio's handle to the owner threads.
pub(crate) struct SrtIngressHandle {
    pub(crate) events: mpsc::Receiver<SrtIngressEvent>,
    /// Lossy, stamped receive-quality samples from the Owners.
    pub(crate) telemetry: mpsc::Receiver<QualitySample>,
    owners: Vec<OwnerLink>,
    local_addr: SocketAddr,
}

impl SrtIngressHandle {
    /// Build each Owner's runtime and listener on its own OS thread and wait
    /// for every verdict. A runtime or listener that cannot be built is a
    /// typed error (the started Owners are stopped); there is no fallback
    /// transport.
    pub(crate) async fn start(config: IngressConfig) -> Result<Self, IngressStartError> {
        let plans = listener_plans(&config).map_err(IngressStartError)?;
        let (events_tx, events_rx) = mpsc::channel(config.event_capacity.max(1));
        let (telemetry_tx, telemetry_rx) = mpsc::channel(config.telemetry_capacity.max(1));
        let (inbox_tx, inbox_rx): (Vec<_>, Vec<_>) =
            plans.iter().map(|_| flume::unbounded()).unzip();
        let epoch = Instant::now();
        let mut owners = Vec::with_capacity(plans.len());
        let mut ready = Vec::with_capacity(plans.len());
        for (index, (plan, inbox)) in plans.into_iter().zip(inbox_rx).enumerate() {
            let (commands_tx, commands_rx) = flume::bounded(config.command_capacity.max(1));
            let (ready_tx, ready_rx) = flume::bounded::<Result<SocketAddr, String>>(1);
            let seat = OwnerSeat {
                index: u16::try_from(index)
                    .map_err(|_| IngressStartError("too many SRT ingress Owners".to_string()))?,
                plan,
                epoch,
                inbox,
                members: inbox_tx.clone(),
            };
            let (owner_config, events, telemetry) =
                (config.clone(), events_tx.clone(), telemetry_tx.clone());
            let spawned = std::thread::Builder::new()
                // `srt-in-<port>[-<i>]`: unique per Owner and within the
                // kernel's 15-byte thread name.
                .name(owner_thread_name(config.bind.port(), index, config.owners))
                .spawn(move || {
                    run_owner_thread(seat, owner_config, commands_rx, events, telemetry, ready_tx)
                });
            let thread = match spawned {
                Ok(thread) => thread,
                Err(error) => {
                    stop_started(owners).await;
                    return Err(IngressStartError(format!(
                        "spawn SRT ingress thread: {error}"
                    )));
                }
            };
            owners.push(OwnerLink {
                commands: commands_tx,
                thread: Some(thread),
            });
            ready.push(ready_rx);
        }
        let mut local_addr = None;
        for ready_rx in ready {
            let verdict = match ready_rx.recv_async().await {
                Ok(verdict) => verdict,
                Err(_) => Err("SRT ingress thread exited before reporting readiness".to_string()),
            };
            match verdict {
                Ok(addr) => {
                    local_addr.get_or_insert(addr);
                }
                Err(message) => {
                    stop_started(owners).await;
                    return Err(IngressStartError(message));
                }
            }
        }
        let local_addr = local_addr
            .ok_or_else(|| IngressStartError("SRT ingress started no Owner".to_string()))?;
        Ok(Self {
            events: events_rx,
            telemetry: telemetry_rx,
            owners,
            local_addr,
        })
    }

    /// The address every Owner's listener socket is bound to.
    pub(crate) fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    /// Offer a session command to the Owner that holds the session, without
    /// blocking. `Err(command)` hands it back when that Owner's bounded bridge
    /// is full (or its thread is gone), so the caller keeps its media/session
    /// state and retries instead of losing it.
    pub(crate) fn try_send(&self, command: IngressCommand) -> Result<(), IngressCommand> {
        let Some(link) = command
            .owner()
            .and_then(|owner| self.owners.get(usize::from(owner)))
        else {
            // Shutdown goes through `shutdown`; an unknown Owner holds nothing.
            return Ok(());
        };
        link.commands
            .try_send(command)
            .map_err(|error| match error {
                flume::TrySendError::Full(command) | flume::TrySendError::Disconnected(command) => {
                    command
                }
            })
    }

    /// Whether any Owner thread is gone. One dead member black-holes its share
    /// of the port, so the listener is down as a whole.
    pub(crate) fn is_closed(&self) -> bool {
        self.owners
            .iter()
            .any(|link| link.commands.is_disconnected())
    }

    /// Orderly stop: deliver `Shutdown` to every Owner (bounded wait for
    /// bridge room), then join them all and report one truthful verdict.
    pub(crate) async fn shutdown(self) -> IngressExit {
        stop_started(self.owners).await
    }
}

fn owner_thread_name(port: u16, index: usize, owners: usize) -> String {
    if owners > 1 {
        format!("srt-in-{port}-{index}")
    } else {
        format!("srt-in-{port}")
    }
}

/// Shut down and join every Owner in `owners`: all quiescent, first fault.
async fn stop_started(owners: Vec<OwnerLink>) -> IngressExit {
    let deadline = Instant::now() + Duration::from_secs(2);
    for link in &owners {
        let mut command = IngressCommand::Shutdown;
        loop {
            match link.commands.try_send(command) {
                Ok(()) | Err(flume::TrySendError::Disconnected(_)) => break,
                Err(flume::TrySendError::Full(back)) => {
                    if Instant::now() >= deadline {
                        break;
                    }
                    command = back;
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
            }
        }
    }
    let mut exit = IngressExit {
        quiescent: true,
        fault: None,
    };
    for OwnerLink { commands, thread } in owners {
        // Dropping the sender also stops the owner thread if the explicit
        // command could not be queued.
        drop(commands);
        let Some(thread) = thread else {
            continue;
        };
        let verdict = match tokio::task::spawn_blocking(move || thread.join()).await {
            Ok(Ok(verdict)) => verdict,
            _ => IngressExit {
                quiescent: false,
                fault: Some("SRT ingress owner thread panicked".to_string()),
            },
        };
        exit.quiescent &= verdict.quiescent;
        if exit.fault.is_none() {
            exit.fault = verdict.fault;
        }
    }
    exit
}
