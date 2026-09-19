//! Native UDP + SRT protocol ownership for the admission loop.
//!
//! This worker owns the UDP descriptor, the one ring, the fixed
//! receive-buffer reserve, the `PeerTable`, its TX pool, and its timers.
//! Admission policy (stream-key authorization) resolves synchronously
//! against the shared policy store; only async control-plane requests
//! (pipeline authentication, session lifecycle) cross the runtime boundary
//! as bounded events.

use std::collections::VecDeque;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::os::fd::AsRawFd;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::Duration;

use restream_dataplane::TxPool;
use restream_dataplane::udp::{
    UdpDriverDatagram, UdpInterest, UdpReadyEvent, UdpSendCompletion, UringUdpDriver,
};
use srt_proto::Timestamp;
use srt_transport::advanced::admission::{
    AdmissionOptions, AdmissionResolution, PeerTable, RejectionReason,
};
use srt_transport::advanced::telemetry::IngressTelemetry;
use srt_transport::{ListenerEncryptionConfig, ListenerPeerPolicy, PolicyOverride};
use tokio::sync::mpsc;
use tracing::error;

use crate::domain::srt_ingest::ResolvedSrtCrypto;
use crate::media::snapshots::ListenerSocketStats;

use super::super::srt_policy::SrtIngestPolicyStore;
use crate::media::srt_stream_id::{SrtConnectionMode, parse_srt_stream_id};

const CHANNEL_CAPACITY: usize = 256;
const BUFFER_COUNT: u16 = CHANNEL_CAPACITY as u16;
const BUFFER_SIZE: usize = 2_048;
pub(super) const MAX_OUTBOUND: usize = 1024;
const SEND_CAPACITY: usize = 64;
/// Driver send-slot partition (one ring, two TX lanes): legacy Tokio-driven
/// traffic uses `0..LEGACY_TX_SLOTS`, protocol replies use
/// `LEGACY_TX_SLOTS..SEND_CAPACITY`. Completions demultiplex by slot, so
/// neither lane can consume the other's CQE.
pub(super) const LEGACY_TX_SLOTS: usize = 32;
/// TX pool for protocol replies (handshake, ACK/NAK, shutdown): preallocated
/// final storage, no per-packet Vec. Media payloads (DataReceived) cross to
/// Tokio as refcounted Bytes; only async control-plane requests cross beyond
/// that.
const PROTO_TX_SLOTS: usize = 256;
pub(super) const PROTO_TX_SLOT_SIZE: usize = 2_048;
/// Bound on control events (Connected/DataReceived/Disconnected) queued to
/// Tokio per worker iteration. Overflow drops with a counter, never blocks
/// the owner thread.
const MAX_EVENTS_PER_TICK: usize = 64;

struct CompatReceiver {
    free: Vec<Vec<u8>>,
}

impl CompatReceiver {
    fn new() -> Self {
        Self {
            free: (0..BUFFER_COUNT).map(|_| vec![0; BUFFER_SIZE]).collect(),
        }
    }

    /// Receive and admit inline on the owner thread. Protocol replies go
    /// through `admit_out`; only async-requiring control events cross via
    /// `events_tx`. Returns datagrams consumed (for tests/diagnostics).
    fn recv(&mut self, socket: &UdpSocket, admit: &mut OwnerAdmit<'_>) -> io::Result<()> {
        for _ in 0..CHANNEL_CAPACITY {
            let Some(mut buffer) = self.free.pop() else {
                admit.stats.dropped_pool.fetch_add(1, Ordering::Relaxed);
                admit
                    .listener_stats
                    .native_rx_pool_drops
                    .fetch_add(1, Ordering::Relaxed);
                return Ok(());
            };
            match socket.recv_from(&mut buffer) {
                Ok((len, peer)) => {
                    admit.stats.recv_datagrams.fetch_add(1, Ordering::Relaxed);
                    admit
                        .listener_stats
                        .native_rx_datagrams
                        .fetch_add(1, Ordering::Relaxed);
                    admit.admit(peer, &buffer[..len]);
                    // Buffer is consumed by admit (copied into protocol
                    // state); recycle the storage immediately. Payload bytes
                    // live in srt-rs connection state after admit.
                    buffer.clear();
                    if buffer.capacity() < BUFFER_SIZE {
                        buffer.reserve(BUFFER_SIZE - buffer.capacity());
                    }
                    // Reuse as zeroed receive storage next round.
                    buffer.resize(BUFFER_SIZE, 0);
                    self.free.push(buffer);
                }
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.free.push(buffer);
                    return Ok(());
                }
                Err(error) => {
                    self.free.push(buffer);
                    return Err(error);
                }
            }
        }
        Ok(())
    }
}

enum ReceiveMode {
    Native(Box<UringUdpDriver>),
    Compat(CompatReceiver),
}

fn receiver_capability_error(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error(),
        Some(libc::EINVAL | libc::ENOSYS | libc::EOPNOTSUPP | libc::EPERM)
    ) || matches!(
        error.kind(),
        io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
    )
}

#[derive(Debug, Default)]
pub(crate) struct NativeSrtIngressStats {
    pub(crate) dropped_pool: AtomicU64,
    pub(crate) event_drops: AtomicU64,
    pub(crate) recv_datagrams: AtomicU64,
    pub(crate) sent_datagrams: AtomicU64,
}

/// Bounded control event from the native protocol owner to Tokio. The
/// `PeerTable` lives on the worker: handshake admission, timer firing, ACK/
/// NAK/retransmit replies, and DataReceived reassembly all progress there.
/// Tokio handles only what needs async: pipeline authentication, session
/// lifecycle, and media-pipeline publish.
pub(crate) enum SrtIngressEvent {
    Connected {
        peer: SocketAddr,
        logical_peer: srt_transport::advanced::admission::LogicalPeerId,
        stream_id: String,
    },
    Media {
        peer: SocketAddr,
        logical_peer: srt_transport::advanced::admission::LogicalPeerId,
        payload: bytes::Bytes,
    },
    Disconnected {
        peer: SocketAddr,
        logical_peer: srt_transport::advanced::admission::LogicalPeerId,
        reason: String,
    },
}

pub(crate) struct NativeSrtIngress {
    pub(crate) events: mpsc::Receiver<SrtIngressEvent>,
    pub(crate) outbound: mpsc::Sender<(SocketAddr, Vec<u8>)>,
    pub(crate) stats: Arc<NativeSrtIngressStats>,
}

impl NativeSrtIngress {
    pub(crate) fn start(
        socket: UdpSocket,
        listener_stats: Arc<ListenerSocketStats>,
        peers: PeerTable,
        admission: AdmissionOptions,
        telemetry: IngressTelemetry,
        policy_store: Arc<SrtIngestPolicyStore>,
    ) -> io::Result<Self> {
        socket.set_nonblocking(true)?;
        let (events_tx, events) = mpsc::channel(CHANNEL_CAPACITY);
        let (outbound, outbound_rx) = mpsc::channel(CHANNEL_CAPACITY);
        let stats = Arc::new(NativeSrtIngressStats::default());
        let worker_stats = stats.clone();
        thread::Builder::new()
            .name("restream-srt-ingress".to_string())
            .spawn(move || {
                if let Err(error) = run_worker(
                    socket,
                    peers,
                    admission,
                    telemetry,
                    policy_store,
                    events_tx,
                    outbound_rx,
                    worker_stats,
                    listener_stats,
                ) {
                    error!(%error, "native SRT ingress stopped");
                }
            })
            .map_err(|error| io::Error::other(format!("spawn SRT ingress: {error}")))?;
        Ok(Self {
            events,
            outbound,
            stats,
        })
    }
}

/// Sink that writes SRT protocol replies directly into the worker's
/// preallocated TX pool and queues them for the driver send path. No
/// intermediate `Vec<u8>` on the normal hot path.
fn resolve_ingress_policy(
    store: &SrtIngestPolicyStore,
    request: &srt_transport::advanced::admission::AdmissionRequest,
) -> AdmissionResolution {
    let stream_id = request
        .claimed_identity
        .stream_id
        .as_deref()
        .unwrap_or_default();
    let parsed = parse_srt_stream_id(stream_id);
    if parsed.stream_key.is_empty()
        || !matches!(
            parsed.mode,
            SrtConnectionMode::Publish | SrtConnectionMode::Read
        )
    {
        return AdmissionResolution::Reject {
            reason: RejectionReason::BAD_MODE,
        };
    }
    let Some(resolved) = store.resolved_policy(&parsed.stream_key) else {
        return AdmissionResolution::Reject {
            reason: RejectionReason::UNAUTHORIZED,
        };
    };
    let mut policy = ListenerPeerPolicy {
        latency: PolicyOverride::Set(std::time::Duration::from_millis(
            resolved.latency_ms.max(0) as u64
        )),
        encryption: PolicyOverride::Set(None),
        ..ListenerPeerPolicy::default()
    };
    if let ResolvedSrtCrypto::Encrypted {
        passphrase,
        pbkeylen,
    } = resolved.crypto
    {
        let Some(key_length) = srt_proto::crypto::KeyLength::from_len(pbkeylen as usize) else {
            return AdmissionResolution::Reject {
                reason: RejectionReason::BAD_REQUEST,
            };
        };
        let Ok(encryption) = ListenerEncryptionConfig::new(passphrase, key_length) else {
            return AdmissionResolution::Reject {
                reason: RejectionReason::BAD_REQUEST,
            };
        };
        policy.encryption = PolicyOverride::Set(Some(encryption));
    }
    AdmissionResolution::Configure(policy)
}

pub(super) fn timestamp_now() -> Timestamp {
    Timestamp::from_micros(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|elapsed| elapsed.as_micros())
            .unwrap_or(0) as u64,
    )
}

/// Protocol owner state for one ingress worker iteration. Admits
/// datagrams into the `PeerTable` synchronously (policy store is `Sync`
/// via `RwLock`); timer replies and control events are handled by
/// `drive_protocol`, not here.
struct OwnerAdmit<'a> {
    peers: &'a mut PeerTable,
    admission: &'a AdmissionOptions,
    telemetry: &'a IngressTelemetry,
    policy_store: &'a Arc<SrtIngestPolicyStore>,
    stats: &'a NativeSrtIngressStats,
    listener_stats: &'a ListenerSocketStats,
}

impl OwnerAdmit<'_> {
    fn admit(&mut self, peer: SocketAddr, payload: &[u8]) {
        let now = timestamp_now();
        let store = self.policy_store.clone();
        let _ = self.peers.admit_with_resolver(
            peer,
            payload,
            now,
            self.admission,
            0,
            1,
            self.telemetry,
            move |request| resolve_ingress_policy(&store, request),
        );
    }
}
#[allow(clippy::too_many_arguments)]
fn run_worker(
    socket: UdpSocket,
    mut peers: PeerTable,
    admission: AdmissionOptions,
    telemetry: IngressTelemetry,
    policy_store: Arc<SrtIngestPolicyStore>,
    events_tx: mpsc::Sender<SrtIngressEvent>,
    mut outbound_rx: mpsc::Receiver<(SocketAddr, Vec<u8>)>,
    stats: Arc<NativeSrtIngressStats>,
    listener_stats: Arc<ListenerSocketStats>,
) -> io::Result<()> {
    let fd = socket.as_raw_fd();
    // One ring for this owner thread: readiness + sends + multishot receive
    // share one `UringUdpDriver`. The compat path below keeps the old
    // recv_from behavior when the kernel cannot provide the multishot
    // receive facility.
    let mut receiver =
        match UringUdpDriver::new_fixed(1, 256, SEND_CAPACITY, BUFFER_COUNT, BUFFER_SIZE) {
            Ok(mut driver) => match driver.register_fixed(fd, 0, 1, UdpInterest::READ_WRITE) {
                Ok(()) => ReceiveMode::Native(Box::new(driver)),
                Err(error) if receiver_capability_error(&error) => {
                    ReceiveMode::Compat(CompatReceiver::new())
                }
                Err(error) => return Err(error),
            },
            Err(error) if receiver_capability_error(&error) => {
                ReceiveMode::Compat(CompatReceiver::new())
            }
            Err(error) => return Err(error),
        };
    let mut outbound = VecDeque::with_capacity(MAX_OUTBOUND);
    let mut inflight = std::iter::repeat_with(|| None)
        .take(LEGACY_TX_SLOTS)
        .collect::<Vec<Option<(SocketAddr, Vec<u8>)>>>();
    let mut completions = vec![
        UdpSendCompletion {
            slot: 0,
            generation: 0,
            result: 0,
        };
        SEND_CAPACITY
    ];
    let mut stashed_completions: Vec<UdpSendCompletion> = Vec::with_capacity(SEND_CAPACITY);
    let mut ready = [UdpReadyEvent {
        fd: -1,
        slot: 0,
        generation: 0,
        readable: false,
        writable: false,
    }];
    let mut received = vec![
        UdpDriverDatagram {
            slot: 0,
            buffer_id: 0,
            offset: 0,
            len: 0,
            peer: "0.0.0.0:0".parse().expect("valid zero socket address"),
        };
        CHANNEL_CAPACITY
    ];
    let mut pool_starved = false;
    // Protocol owner state: PeerTable + timers + TX pool live here, on the
    // socket owner. Tokio never admits, feeds, or times out.
    let mut tx_pool = TxPool::new(PROTO_TX_SLOTS, PROTO_TX_SLOT_SIZE)
        .map_err(|error| io::Error::other(format!("SRT ingress TX pool: {error:?}")))?;
    let mut proto_outbound: VecDeque<ProtoDatagram> = VecDeque::with_capacity(MAX_OUTBOUND);
    let mut proto_inflight: Vec<Option<ProtoDatagram>> = std::iter::repeat_with(|| None)
        .take(SEND_CAPACITY)
        .collect();
    let mut proto_completions = vec![
        UdpSendCompletion {
            slot: 0,
            generation: 0,
            result: 0,
        };
        SEND_CAPACITY
    ];
    let mut proto_events: Vec<srt_transport::advanced::admission::AdmissionEvent> =
        Vec::with_capacity(MAX_EVENTS_PER_TICK);
    let mut event_drops: u64 = 0;
    let mut tx_pool_empty: u64 = 0;

    // Deadline-aware wait: the next PeerTable timer caps the ring wait so
    // retransmit/handshake timeouts fire without a Tokio tick.

    // Forward due timers + collect replies into the TX pool, then emit
    // async-requiring control events. Shared by Native and Compat paths.
    // (Implemented inline below per path because `driver` borrows differ.)

    loop {
        // Legacy Tokio->worker datagrams (Tokio `poll_outbound` replies):
        // still honored during migration, sent via the same driver ring.
        while outbound.len() < MAX_OUTBOUND {
            match outbound_rx.try_recv() {
                Ok(packet) => outbound.push_back(packet),
                Err(mpsc::error::TryRecvError::Empty) => break,
                Err(mpsc::error::TryRecvError::Disconnected) => {
                    stats.event_drops.fetch_add(event_drops, Ordering::Relaxed);
                    return Ok(());
                }
            }
        }
        if let ReceiveMode::Native(driver) = &mut receiver {
            flush_outbound(
                driver,
                fd,
                &mut outbound,
                &mut inflight,
                &mut completions,
                &mut stashed_completions,
                &stats,
                &listener_stats,
            )?;
            flush_proto_outbound(
                driver,
                fd,
                &mut proto_outbound,
                &mut proto_inflight,
                &mut proto_completions,
                &mut stashed_completions,
                &mut tx_pool,
                &stats,
                &listener_stats,
            )?;
        }

        if events_tx.is_closed()
            && outbound.is_empty()
            && inflight.iter().all(Option::is_none)
            && proto_outbound.is_empty()
            && proto_inflight.iter().all(Option::is_none)
        {
            stats.event_drops.fetch_add(event_drops, Ordering::Relaxed);
            return Ok(());
        }
        match &mut receiver {
            ReceiveMode::Native(driver) => {
                // One ring, one wait: readiness + sends + multishot receive
                // share this single `poll`. The wait is capped by the next
                // protocol deadline so timers fire on the owner.
                let wait = Duration::from_micros(
                    peers
                        .time_until_next_deadline(timestamp_now(), 5_000)
                        .min(5_000),
                );
                let (ready_count, received_count) =
                    match driver.poll(wait, &mut ready, &mut received) {
                        Ok(counts) => counts,
                        Err(error)
                            if stats.recv_datagrams.load(Ordering::Relaxed) == 0
                                && receiver_capability_error(&error) =>
                        {
                            receiver = ReceiveMode::Compat(CompatReceiver::new());
                            continue;
                        }
                        Err(error) => return Err(error),
                    };
                for event in ready.iter().take(ready_count) {
                    driver
                        .rearm(fd, event.slot, event.generation)
                        .map_err(|error| {
                            io::Error::other(format!("rearm SRT ingress poll: {error}"))
                        })?;
                }
                if driver.available_buffers(0) == 0 {
                    if !pool_starved {
                        stats.dropped_pool.fetch_add(1, Ordering::Relaxed);
                        listener_stats
                            .native_rx_pool_drops
                            .fetch_add(1, Ordering::Relaxed);
                        pool_starved = true;
                    }
                } else {
                    pool_starved = false;
                }
                {
                    let mut admit = OwnerAdmit {
                        peers: &mut peers,
                        admission: &admission,
                        telemetry: &telemetry,
                        policy_store: &policy_store,
                        stats: &stats,
                        listener_stats: &listener_stats,
                    };
                    for datagram in received.iter().take(received_count).copied() {
                        stats.recv_datagrams.fetch_add(1, Ordering::Relaxed);
                        listener_stats
                            .native_rx_datagrams
                            .fetch_add(1, Ordering::Relaxed);
                        let recv_buffers = driver.buffers(0).expect("ingress slot remains live");
                        let Some(payload) =
                            recv_buffers.payload(datagram.buffer_id, datagram.offset, datagram.len)
                        else {
                            let _ = driver.recycle(0, datagram.buffer_id);
                            continue;
                        };
                        // Admit synchronously on the owner: zero-copy view,
                        // no channel crossing for handshake/timer traffic.
                        admit.admit(datagram.peer, payload);
                        let _ = driver.recycle(0, datagram.buffer_id);
                    }
                }
                // Due timers + replies -> TX pool -> driver sends, then
                // forward async-requiring events. All on the owner.
                drive_protocol(
                    driver,
                    fd,
                    &mut peers,
                    &admission,
                    &telemetry,
                    &mut tx_pool,
                    &mut proto_outbound,
                    &mut proto_inflight,
                    &mut proto_completions,
                    &mut stashed_completions,
                    &mut proto_events,
                    &mut tx_pool_empty,
                    &events_tx,
                    &mut event_drops,
                    &stats,
                    &listener_stats,
                )?;
                flush_outbound(
                    driver,
                    fd,
                    &mut outbound,
                    &mut inflight,
                    &mut completions,
                    &mut stashed_completions,
                    &stats,
                    &listener_stats,
                )?;
            }
            ReceiveMode::Compat(compat) => {
                {
                    let mut admit = OwnerAdmit {
                        peers: &mut peers,
                        admission: &admission,
                        telemetry: &telemetry,
                        policy_store: &policy_store,
                        stats: &stats,
                        listener_stats: &listener_stats,
                    };
                    compat.recv(&socket, &mut admit)?;
                }
                drive_protocol_compat(
                    &socket,
                    &mut peers,
                    &admission,
                    &telemetry,
                    &mut tx_pool,
                    &mut proto_outbound,
                    &mut proto_events,
                    &mut tx_pool_empty,
                    &events_tx,
                    &mut event_drops,
                    &stats,
                    &listener_stats,
                )?;
                std::thread::sleep(Duration::from_millis(1));
            }
        }
        if event_drops != 0 {
            stats
                .event_drops
                .fetch_add(std::mem::replace(&mut event_drops, 0), Ordering::Relaxed);
        }
    }
}

/// Fire due timers, collect replies directly into the TX pool, submit
/// them on the shared ring, and forward async-requiring control events.
/// All protocol progression stays on the socket owner; Tokio receives only
/// Connected/Media/Disconnected.
#[path = "native_ingress_drive.rs"]
mod drive;
use drive::*;

#[cfg(test)]
#[path = "native_ingress_tests.rs"]
mod tests;
