//! The vocabulary the SRT ingress Owners and Tokio exchange: bridge
//! capacities, session ids, the command and event enums, the exit report,
//! and the configuration and per-Owner seat the threads start from. A leaf:
//! the bridge (`ingress_bridge`), the Owners (`ingress_owner`) and quality
//! telemetry (`ingress_quality`) all import it, and it imports none of them.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;

use srt_transport::advanced::admission::LogicalPeerId;
use srt_transport::advanced::caller::LogicalCallerId;
use srt_transport::{ListenerTransfer, OwnerListenerPlan};

use crate::snapshots::ListenerSocketStats;
use restream_mpegts::DemuxProbe;
use restream_sync::RingBuffer;

use super::ingress_admission::{AdmissionGuard, ReceiverGroupId};
use super::ingress_media::{ProbeIdentity, SrtPublisherMedia, SrtReaderMedia};
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

/// One ingress session on its Owner: a peer the listener admitted, or a
/// caller this Owner dialed for an `srt_call` input. Both feed the same
/// media path.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum SessionId {
    Peer(LogicalPeerId),
    Caller(LogicalCallerId),
}

/// One ingress session: the Owner thread that holds it and its id there. The
/// sole cross-thread session handle; no protocol object, table reference,
/// socket id or `SocketAddr` identifies a session.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct IngressPeer {
    pub(crate) owner: u16,
    pub(crate) id: SessionId,
}

/// Tokio's name for one outbound call it asked an Owner to make, until the
/// Owner reports the session it became.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) struct CallToken(pub(crate) u64);

pub(crate) enum IngressCommand {
    /// Tokio admitted the peer as a publisher; the Owner runs its media.
    AttachPublisher {
        logical_peer: IngressPeer,
        media: Box<SrtPublisherMedia>,
    },
    /// Tokio admitted the peer as a direct read/play reader.
    AttachReader {
        logical_peer: IngressPeer,
        reader: Box<SrtReaderMedia>,
    },
    /// Tokio applied the publisher's stream probe; `ring` replaces its ring.
    ProbeApplied {
        logical_peer: IngressPeer,
        identity: ProbeIdentity,
        ring: Option<Arc<RingBuffer>>,
    },
    /// Begin an orderly protocol disconnect of one peer. The owner retires the
    /// peer when its terminal event arrives (or after a short grace).
    Disconnect { logical_peer: IngressPeer },
    /// Dial a remote SRT listener for an `srt_call` input. The Owner answers
    /// with [`SrtIngressEvent::CallStarted`] or [`SrtIngressEvent::CallFailed`];
    /// a started call later reports `Connected` and, at its end,
    /// `Disconnected`, like an admitted peer.
    Call {
        owner: u16,
        token: CallToken,
        /// The resolved listener address, for lifecycle events.
        remote: SocketAddr,
        request: Box<restream_srt::egress_connect::SrtConnectRequest>,
    },
    /// Disconnect nothing further; flush, drain the Owner and exit. Sent to
    /// every Owner by `SrtIngressHandle::shutdown` only.
    Shutdown,
}

impl IngressCommand {
    /// The Owner a session command is for.
    pub(super) fn owner(&self) -> Option<u16> {
        match self {
            Self::AttachPublisher { logical_peer, .. }
            | Self::AttachReader { logical_peer, .. }
            | Self::ProbeApplied { logical_peer, .. }
            | Self::Disconnect { logical_peer } => Some(logical_peer.owner),
            Self::Call { owner, .. } => Some(*owner),
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
    /// A publisher's stream probe or authoritative container topology update.
    /// Its packets are held until Tokio answers with [`IngressCommand::ProbeApplied`].
    Probe {
        logical_peer: IngressPeer,
        identity: ProbeIdentity,
        // Topology-only allocation keeps every bounded bridge slot compact.
        probe: Box<DemuxProbe>,
    },
    /// Terminal. The Owner has already flushed the publisher's media.
    Disconnected {
        peer: SocketAddr,
        logical_peer: IngressPeer,
        reason: String,
    },
    /// An Owner developed an OWNER-FATAL fault; the listener is gone.
    Fault { detail: String },
    /// The Owner admitted a call: `logical_peer` is its session from now on.
    CallStarted {
        token: CallToken,
        logical_peer: IngressPeer,
    },
    /// The Owner could not start the call (or the pool expired or refused
    /// it before admission); nothing is left behind.
    CallFailed { token: CallToken, reason: String },
}

/// What the owner threads report when they exit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IngressExit {
    /// `shutdown_and_drain` reached quiescence on every Owner (or there was
    /// nothing to drain).
    pub(crate) quiescent: bool,
    pub(crate) fault: Option<String>,
}

/// Everything the owner threads are built from.
#[derive(Clone)]
pub(crate) struct IngressConfig {
    pub(crate) bind: SocketAddr,
    pub(crate) policy_store: Arc<SrtIngestPolicyStore>,
    pub(crate) receiver_group: ReceiverGroupId,
    /// IP bans and failed-guess reporting for the admission resolver.
    pub(crate) admission_guard: AdmissionGuard,
    pub(crate) stats: Arc<ListenerSocketStats>,
    pub(crate) command_capacity: usize,
    pub(crate) event_capacity: usize,
    pub(crate) telemetry_capacity: usize,
    /// Owner threads sharing the port; 1 is one Owner on one socket.
    pub(crate) owners: usize,
    /// Peers one client IP may hold (srt-rs `max_peers_per_ip`).
    pub(crate) max_peers_per_ip: usize,
    /// One Owner's window-memory budget (srt-rs `max_window_memory_bytes`);
    /// `None` is no budget.
    pub(crate) window_memory_bytes: Option<u64>,
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
