//! Runtime adapter for the external `srt-rs` protocol core.
//!
//! The protocol crate is sans-I/O. This module owns the small amount of
//! application transport state needed by Restream: one nonblocking UDP
//! socket, one protocol connection, and one manual timer store. The egress
//! fabric continues to own scheduling and lifecycle; this adapter only moves
//! datagrams through Tokio-owned UDP sockets and `SrtConnection`.
//!
//! Each `srt-rs` connection (`RustSrtSocket`) is owned directly by the
//! `SrtFabricLeaf` that connected it (boxed as `dyn SrtMessageSender`, since
//! `RustSrtSocket` implements that trait directly below) -- there is no
//! socket-id indirection or process-global connection registry.

use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::media::egress::backend::CloseReason;
use crate::media::egress::backends::srt::muxer_ports::SrtEgressMuxerPortState;
use crate::media::snapshots::PublisherQuality;
use bytes::Bytes;
use shiguredo_srt::{ConnectionState, Timestamp};
use srt_transport::OutputDrainBudget;
use srt_transport::tokio_transport::{Conn, GroupConn as TokioGroupConn};
use srt_transport::{LogicalCallerId, LogicalCallerState, LogicalCallerStats};

mod knobs;
pub(crate) use knobs::{apply_optional_udp_buf, desired_udp_buf, shared_io_batch_capacity};
pub use knobs::{recv_budget, recv_budget_or};

fn should_use_shared_srt_egress_state(peer_count: usize, has_shared_state: bool) -> bool {
    peer_count == 1 && has_shared_state
}

enum RustSrtSocket {
    Direct(Box<Conn>),
    Bonded(Box<TokioGroupConn>),
    Shared {
        state: SrtEgressMuxerPortState,
        caller: LogicalCallerId,
    },
}

mod shared;
pub(crate) use shared::SharedSrtEgress;

/// Drives one shard's shared SRT egress socket and `CallerTable` once, if a
/// leaf has bound it yet.
///
/// This is deliberately *not* reachable through `SrtMessageSender::drive`:
/// every `Shared` leaf on a shard holds a clone of the same
/// `SrtEgressMuxerPortState`, and `SharedSrtEgress::drive` is a whole-table
/// operation (drain the common UDP socket, flush common outbound packets,
/// poll every logical caller). Driving it per leaf would take the shared
/// mutex and redo that table-wide work N times per readiness pass for N
/// leaves sharing one multiplexer, so the shard calls this once instead
/// (`SrtShardBackend::poll_ready`).
///
/// This bounds *readiness* driving only. The send path still drives the
/// table after each accepted message (see `RustSrtSocket::send`'s `Shared`
/// arm), and `SrtEgressEngine::send_pending` sends several fragments per
/// visit, so a busy pass performs more than one table drive in total.
/// Batching those into one flush per visit is a separate, older scaling
/// question this does not address.
pub(crate) fn drive_shared_srt_egress(state: &SrtEgressMuxerPortState) {
    let Ok(mut state) = state.lock() else {
        return;
    };
    if let Some(shared) = state.as_mut() {
        let _ = shared.drive(timestamp_now());
    }
}

impl RustSrtSocket {
    fn drive_connection(&mut self, now: Timestamp, runtime: &tokio::runtime::Runtime) -> bool {
        match self {
            Self::Direct(conn) => {
                receive_conn(conn, now);
                conn.fire_expired(now);
                let _ =
                    runtime.block_on(conn.drain_outputs_bounded(now, OutputDrainBudget::default()));
                conn.conn.state() != ConnectionState::Disconnected
            }
            Self::Bonded(conn) => {
                let _ = conn.drive(now, OutputDrainBudget::default());
                conn.group()
                    .members()
                    .iter()
                    .any(|member| member.connection().state() != ConnectionState::Disconnected)
            }
            // Shared leaves get their table-wide driving from
            // `drive_shared_srt_egress`; there is no per-leaf I/O to do here.
            Self::Shared { state, caller } => state
                .lock()
                .ok()
                .and_then(|state| state.as_ref()?.callers.logical_caller(caller)?.state())
                .is_some_and(|state| state != LogicalCallerState::Disconnected),
        }
    }

    fn send(&mut self, message: &Bytes, runtime: &tokio::runtime::Runtime) -> SrtSendResult {
        match self {
            Self::Direct(conn) => send_direct_message(conn, message, runtime),
            Self::Bonded(conn) => {
                if conn
                    .group()
                    .members()
                    .iter()
                    .all(|member| member.connection().state() == ConnectionState::Disconnected)
                {
                    return SrtSendResult::PeerClosed;
                }
                if !conn.can_send() {
                    return SrtSendResult::WouldBlock;
                }
                match conn.send_shared(message.clone(), timestamp_now()) {
                    Ok(_) => match conn.drive(timestamp_now(), OutputDrainBudget::default()) {
                        Ok(_) => SrtSendResult::Accepted {
                            bytes: message.len(),
                        },
                        Err(error) => SrtSendResult::Failed {
                            reason: "srt-rs-send",
                            detail: error.to_string(),
                            retryable: true,
                        },
                    },
                    Err(error) => SrtSendResult::Failed {
                        reason: "srt-rs-send",
                        detail: error.to_string(),
                        retryable: true,
                    },
                }
            }
            Self::Shared { state, caller } => {
                let Ok(mut shared) = state.lock() else {
                    return SrtSendResult::Failed {
                        reason: "srt-rs-shared-lock",
                        detail: "shared SRT egress state is poisoned".to_string(),
                        retryable: true,
                    };
                };
                let Some(shared) = shared.as_mut() else {
                    return SrtSendResult::PeerClosed;
                };
                let Some(mut caller) = shared.callers.logical_caller_mut(caller) else {
                    return SrtSendResult::PeerClosed;
                };
                match caller.state() {
                    Some(LogicalCallerState::Disconnected) | None => SrtSendResult::PeerClosed,
                    Some(LogicalCallerState::Connecting) => SrtSendResult::WouldBlock,
                    Some(LogicalCallerState::Connected) if !caller.can_send() => {
                        SrtSendResult::WouldBlock
                    }
                    Some(LogicalCallerState::Connected) => {
                        match caller.send_shared(message.clone(), timestamp_now()) {
                            Ok(_) => match shared.drive(timestamp_now()) {
                                Ok(()) => SrtSendResult::Accepted {
                                    bytes: message.len(),
                                },
                                Err(error) => SrtSendResult::Failed {
                                    reason: "srt-rs-send",
                                    detail: error,
                                    retryable: true,
                                },
                            },
                            Err(error) => SrtSendResult::Failed {
                                reason: "srt-rs-send",
                                detail: error.to_string(),
                                retryable: true,
                            },
                        }
                    }
                }
            }
        }
    }

    fn native_send_backlog_inner(&self) -> Option<NativeSendBacklog> {
        match self {
            Self::Direct(conn) => conn.conn.sender_stats().map(|stats| NativeSendBacklog {
                bytes: stats.payload_bytes_in_buffer,
                packets: stats.packets_in_buffer,
                ms: u32::try_from(stats.buffer_span_micros / 1_000).unwrap_or(u32::MAX),
            }),
            Self::Bonded(conn) => {
                let stats = conn.stats();
                let mut bytes = 0_u64;
                let mut packets = 0_u32;
                let mut span_micros = 0_u64;
                for leg in stats.legs {
                    if let Some(sender) = leg.connection.sender {
                        bytes = bytes.saturating_add(sender.payload_bytes_in_buffer);
                        packets = packets.saturating_add(sender.packets_in_buffer);
                        span_micros = span_micros.max(sender.buffer_span_micros);
                    }
                }
                Some(NativeSendBacklog {
                    bytes,
                    packets,
                    ms: u32::try_from(span_micros / 1_000).unwrap_or(u32::MAX),
                })
            }
            Self::Shared { state, caller } => {
                let shared = state.lock().ok()?;
                let shared = shared.as_ref()?;
                match shared.callers.logical_caller(caller)?.stats()? {
                    LogicalCallerStats::Direct(stats) => {
                        stats.sender.map(|sender| NativeSendBacklog {
                            bytes: sender.payload_bytes_in_buffer,
                            packets: sender.packets_in_buffer,
                            ms: u32::try_from(sender.buffer_span_micros / 1_000)
                                .unwrap_or(u32::MAX),
                        })
                    }
                    LogicalCallerStats::Group(stats) => {
                        let mut bytes = 0_u64;
                        let mut packets = 0_u32;
                        let mut span_micros = 0_u64;
                        for leg in stats.legs {
                            if let Some(sender) = leg.connection.sender {
                                bytes = bytes.saturating_add(sender.payload_bytes_in_buffer);
                                packets = packets.saturating_add(sender.packets_in_buffer);
                                span_micros = span_micros.max(sender.buffer_span_micros);
                            }
                        }
                        Some(NativeSendBacklog {
                            bytes,
                            packets,
                            ms: u32::try_from(span_micros / 1_000).unwrap_or(u32::MAX),
                        })
                    }
                }
            }
        }
    }
}

impl SrtMessageSender for RustSrtSocket {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        let Ok(runtime) = srt_runtime() else {
            return SrtSendResult::Failed {
                reason: "srt-rs-runtime",
                detail: "Tokio runtime unavailable".to_string(),
                retryable: true,
            };
        };
        self.send(message, &runtime)
    }

    /// Feeds inbound datagrams into the connection, fires expired timers,
    /// and drains pending outbound datagrams -- called once per leaf on
    /// every `poll_ready()` pass (see `egress/backends/srt.rs`), independent
    /// of whether `send_message` is called that pass. Without this, a
    /// `Direct`/`Bonded` connection would never process incoming ACKs/NAKs
    /// or advance its congestion/RTT state between sends.
    ///
    /// `Shared` leaves do nothing here: their socket and caller table are
    /// shared with every other shared leaf on the shard, so readiness
    /// driving happens once in `drive_shared_srt_egress` rather than once
    /// per leaf.
    fn drive(&mut self) {
        if matches!(self, Self::Shared { .. }) {
            return;
        }
        let Ok(runtime) = srt_runtime() else {
            return;
        };
        self.drive_connection(timestamp_now(), &runtime);
    }

    /// For `Shared`, disconnects this leaf's logical caller from the
    /// shard's shared UDP socket/table without tearing down the socket
    /// other callers on this shard still use. `Direct`/`Bonded` need no
    /// explicit close: dropping the leaf drops this value, which drops the
    /// owned `Conn`/`GroupConn` (and its Tokio `UdpSocket`) -- exactly the
    /// behavior this had before, just via ownership instead of a registry
    /// removal.
    fn close(&mut self, _reason: CloseReason) {
        if let Self::Shared { state, caller } = self
            && let Ok(mut shared) = state.lock()
        {
            let Some(shared) = shared.as_mut() else {
                return;
            };
            if let Some(mut logical_caller) = shared.callers.logical_caller_mut(caller) {
                logical_caller.disconnect(timestamp_now());
            }
            let _ = shared.callers.remove(*caller);
        }
    }

    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        self.native_send_backlog_inner()
    }

    fn sender_quality(&self) -> Option<PublisherQuality> {
        match self {
            Self::Direct(conn) => {
                let stats = conn.conn.sender_stats()?;
                Some(sender_quality(
                    stats.peer_rtt_micros.map(f64::from),
                    stats.peer_receiving_rate_bytes_per_second,
                    stats.total_lost,
                    stats.total_dropped,
                ))
            }
            Self::Bonded(conn) => {
                let stats = conn.stats();
                Some(group_sender_quality(
                    stats.legs.iter().filter_map(|leg| {
                        leg.connection.sender.as_ref().map(|sender| {
                            (
                                sender.peer_rtt_micros.map(f64::from),
                                sender.peer_receiving_rate_bytes_per_second,
                            )
                        })
                    }),
                    stats.aggregate.wire_sender_packets_lost,
                ))
            }
            Self::Shared { state, caller } => {
                let shared = state.lock().ok()?;
                let shared = shared.as_ref()?;
                match shared.callers.logical_caller(caller)?.stats()? {
                    LogicalCallerStats::Direct(stats) => {
                        let sender = stats.sender?;
                        Some(sender_quality(
                            sender.peer_rtt_micros.map(f64::from),
                            sender.peer_receiving_rate_bytes_per_second,
                            sender.total_lost,
                            sender.total_dropped,
                        ))
                    }
                    LogicalCallerStats::Group(stats) => Some(group_sender_quality(
                        stats.legs.iter().filter_map(|leg| {
                            leg.connection.sender.as_ref().map(|sender| {
                                (
                                    sender.peer_rtt_micros.map(f64::from),
                                    sender.peer_receiving_rate_bytes_per_second,
                                )
                            })
                        }),
                        stats.aggregate.wire_sender_packets_lost,
                    )),
                }
            }
        }
    }
}

/// One sender's srt-rs counters as the cross-protocol quality snapshot the
/// status layer publishes -- `rtmp/ingest.rs` builds the same type from its
/// own protocol counters, so SRT reports through it directly rather than
/// through an intermediate transport-shaped struct.
fn sender_quality<L, D>(
    peer_rtt_micros: Option<f64>,
    peer_receiving_rate_bytes_per_second: Option<L>,
    total_lost: D,
    total_dropped: D,
) -> PublisherQuality
where
    L: Into<f64>,
    D: TryInto<u64>,
{
    PublisherQuality {
        ms_rtt: Some(peer_rtt_micros.unwrap_or(0.0) / 1_000.0),
        mbps_send_rate: Some(
            peer_receiving_rate_bytes_per_second.map_or(0.0, Into::into) / 1_000_000.0,
        ),
        packets_sent_loss: Some(total_lost.try_into().unwrap_or(u64::MAX)),
        packets_sent_drop: Some(total_dropped.try_into().unwrap_or(u64::MAX)),
        ..PublisherQuality::default()
    }
}

/// Bonded/group equivalent, over each leg's `(rtt_micros, send_rate_bytes)`:
/// RTT averaged across legs reporting one, send rate summed across legs,
/// loss taken from the group's own aggregate. Groups report no aggregate
/// TLPKTDROP counter, so drops read as zero.
fn group_sender_quality<L: Into<f64>>(
    legs: impl Iterator<Item = (Option<f64>, Option<L>)>,
    wire_packets_lost: u64,
) -> PublisherQuality {
    let mut rtt_total = 0_f64;
    let mut rtt_count = 0_u64;
    let mut rate = 0_f64;
    for (peer_rtt_micros, peer_receiving_rate_bytes_per_second) in legs {
        if let Some(rtt) = peer_rtt_micros {
            rtt_total += rtt;
            rtt_count += 1;
        }
        rate += peer_receiving_rate_bytes_per_second.map_or(0.0, Into::into);
    }
    PublisherQuality {
        ms_rtt: Some(if rtt_count == 0 {
            0.0
        } else {
            rtt_total / rtt_count as f64 / 1_000.0
        }),
        mbps_send_rate: Some(rate / 1_000_000.0),
        packets_sent_loss: Some(wire_packets_lost),
        packets_sent_drop: Some(0),
        ..PublisherQuality::default()
    }
}

static NEXT_GROUP_ID: OnceLock<Mutex<u32>> = OnceLock::new();
static CLOCK: OnceLock<Instant> = OnceLock::new();
static SRT_RUNTIME: OnceLock<Result<std::sync::Arc<tokio::runtime::Runtime>, String>> =
    OnceLock::new();

fn srt_runtime() -> Result<std::sync::Arc<tokio::runtime::Runtime>, String> {
    SRT_RUNTIME
        .get_or_init(|| {
            tokio::runtime::Builder::new_multi_thread()
                .worker_threads(2)
                .enable_all()
                .build()
                .map(std::sync::Arc::new)
                .map_err(|e| e.to_string())
        })
        .clone()
}

/// Forces the shared `srt-rs` Tokio runtime to exist (building it on first
/// call, cheaply reusing it afterward), surfacing a build failure -- so a
/// resource-exhaustion failure is caught once at fabric-spawn time instead of
/// silently deferred to the first real connect attempt.
pub(crate) fn ensure_srt_runtime() -> Result<(), String> {
    srt_runtime().map(|_| ())
}

fn next_group_id() -> u32 {
    let next = NEXT_GROUP_ID.get_or_init(|| Mutex::new(10));
    let mut next = next.lock().unwrap_or_else(|error| error.into_inner());
    let id = *next;
    *next = next.saturating_add(1);
    id
}

pub(super) fn timestamp_now() -> Timestamp {
    let start = CLOCK.get_or_init(Instant::now);
    Timestamp::from_micros(start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
}

fn send_direct_message(
    conn: &mut Conn,
    message: &Bytes,
    runtime: &tokio::runtime::Runtime,
) -> SrtSendResult {
    if conn.conn.state() == ConnectionState::Disconnected {
        return SrtSendResult::PeerClosed;
    }
    if conn.conn.state() != ConnectionState::Connected {
        return SrtSendResult::WouldBlock;
    }
    let now = timestamp_now();
    if !conn.conn.can_send_with_pacing(now) {
        return SrtSendResult::WouldBlock;
    }
    match conn.conn.send_shared(message.clone(), now) {
        Ok(()) => match runtime
            .block_on(conn.drain_outputs_bounded(timestamp_now(), OutputDrainBudget::default()))
        {
            Ok(_) => SrtSendResult::Accepted {
                bytes: message.len(),
            },
            Err(error) => SrtSendResult::Failed {
                reason: "srt-rs-send",
                detail: error.to_string(),
                retryable: true,
            },
        },
        Err(error) => SrtSendResult::Failed {
            reason: "srt-rs-send",
            detail: error.to_string(),
            retryable: true,
        },
    }
}

fn receive_conn(conn: &mut Conn, now: Timestamp) {
    let _ = conn.recv_ready(now, recv_budget_or(srt_transport::RecvBudget::default()));
}

pub(crate) trait SrtMessageSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult;
    fn close(&mut self, reason: CloseReason);
    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        None
    }
    /// This transport's sender-side quality snapshot, already in the
    /// cross-protocol shape the status layer publishes.
    fn sender_quality(&self) -> Option<PublisherQuality> {
        None
    }
    /// Drives this transport's I/O for one tick (receive, timers, drain) --
    /// called once per leaf per `poll_ready()` pass. Fakes have no real I/O
    /// to drive, so the default is a no-op.
    fn drive(&mut self) {}
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct NativeSendBacklog {
    pub bytes: u64,
    pub packets: u32,
    pub ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SrtSendResult {
    Accepted {
        bytes: usize,
    },
    WouldBlock,
    PeerClosed,
    Failed {
        reason: &'static str,
        detail: String,
        retryable: bool,
    },
}

impl<T: SrtMessageSender + ?Sized> SrtMessageSender for Box<T> {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        (**self).send_message(message)
    }
    fn close(&mut self, reason: CloseReason) {
        (**self).close(reason)
    }
    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        (**self).native_send_backlog()
    }
    fn sender_quality(&self) -> Option<PublisherQuality> {
        (**self).sender_quality()
    }
    fn drive(&mut self) {
        (**self).drive()
    }
}

#[derive(Clone)]
pub(crate) struct SrtFabricEgressConnectSpec {
    peer_hosts: Vec<String>,
    stream_id: String,
    passphrase: Option<String>,
    key_length: Option<shiguredo_srt::KeyLength>,
    bond_type: shiguredo_srt::GroupType,
    connect_timeout_ms: u64,
}

impl SrtFabricEgressConnectSpec {
    pub(crate) fn from_url(url: &str, connect_timeout_ms: u64) -> Self {
        let clean = url.strip_prefix("srt://").unwrap_or(url);
        let mut parts = clean.splitn(2, '?');
        let host = parts.next().unwrap_or_default().to_string();
        let mut stream_id = String::new();
        let mut passphrase = None;
        let mut key_length = None;
        let mut bond_type = shiguredo_srt::GroupType::Backup;
        let mut peers = vec![host];
        if let Some(query) = parts.next() {
            for pair in query.split('&') {
                let Some((key, value)) = pair.split_once('=') else {
                    continue;
                };
                match key {
                    "streamid" => stream_id = percent_decode(value),
                    "passphrase" => passphrase = Some(percent_decode(value)),
                    "pbkeylen" => {
                        key_length = value
                            .parse::<usize>()
                            .ok()
                            .and_then(shiguredo_srt::KeyLength::from_len)
                    }
                    "bond" => peers.extend(value.split(',').map(str::to_string)),
                    "type" => match value.to_ascii_lowercase().as_str() {
                        "broadcast" => bond_type = shiguredo_srt::GroupType::Broadcast,
                        "backup" => bond_type = shiguredo_srt::GroupType::Backup,
                        _ => {}
                    },
                    _ => {}
                }
            }
        }
        Self {
            peer_hosts: std::mem::take(&mut peers),
            stream_id,
            passphrase,
            key_length,
            bond_type,
            connect_timeout_ms,
        }
    }

    pub(crate) fn peer_hosts(&self) -> &[String] {
        &self.peer_hosts
    }

    pub(crate) fn connect_config<'a>(
        &'a self,
        peer_addrs: &'a [SocketAddr],
        shared_state: Option<SrtEgressMuxerPortState>,
    ) -> SrtFabricEgressConnectConfig<'a> {
        SrtFabricEgressConnectConfig {
            peer_addrs,
            stream_id: &self.stream_id,
            passphrase: self.passphrase.as_deref(),
            key_length: self.key_length,
            bond_type: self.bond_type,
            connect_timeout_ms: self.connect_timeout_ms,
            shared_state,
        }
    }
}

pub(crate) struct SrtFabricEgressConnectConfig<'a> {
    peer_addrs: &'a [SocketAddr],
    stream_id: &'a str,
    passphrase: Option<&'a str>,
    key_length: Option<shiguredo_srt::KeyLength>,
    bond_type: shiguredo_srt::GroupType,
    connect_timeout_ms: u64,
    shared_state: Option<SrtEgressMuxerPortState>,
}

#[cfg(test)]
impl SrtFabricEgressConnectSpec {
    pub(crate) fn stream_id(&self) -> &str {
        &self.stream_id
    }

    pub(crate) fn bond_type(&self) -> shiguredo_srt::GroupType {
        self.bond_type
    }
}

#[cfg(test)]
impl SrtFabricEgressConnectConfig<'_> {
    pub(crate) fn peer_addrs(&self) -> &[SocketAddr] {
        self.peer_addrs
    }

    pub(crate) fn stream_id(&self) -> &str {
        self.stream_id
    }

    pub(crate) fn connect_timeout_ms(&self) -> u64 {
        self.connect_timeout_ms
    }

    pub(crate) fn has_muxer_port_claim(&self) -> bool {
        self.shared_state.is_some()
    }

    pub(crate) fn muxer_port_claim_bind_port(&self) -> Option<u16> {
        self.shared_state.as_ref().and_then(|state| {
            state
                .lock()
                .ok()
                .and_then(|shared| {
                    shared
                        .as_ref()
                        .and_then(|shared| shared.socket.local_addr().ok())
                })
                .map(|address| address.port())
        })
    }
}

/// Connects a new SRT egress transport and hands it back directly -- the
/// caller (`SrtSocketConnector::connect`) boxes it as `dyn SrtMessageSender`
/// and the leaf owns it for its whole lifetime; there is no id-keyed
/// registry to look it back up through.
pub(crate) fn connect_fabric_srt_egress_socket(
    config: SrtFabricEgressConnectConfig<'_>,
) -> Result<Box<dyn SrtMessageSender + Send>, String> {
    if config.peer_addrs.is_empty() {
        return Err("SRT connect requires a peer address".to_string());
    }
    let mut session = srt_transport::SessionConfig::default();
    session.set_stream_id((!config.stream_id.is_empty()).then(|| config.stream_id.to_string()));
    if let Some(passphrase) = config.passphrase {
        let mut encryption = srt_transport::EncryptionConfig::new(passphrase);
        if let Some(key_length) = config.key_length {
            encryption = encryption.key_length(key_length);
        }
        session.set_encryption(Some(encryption));
    }
    let connect = srt_transport::ConnectConfig {
        max_in_flight: std::num::NonZeroUsize::MIN,
        attempt_deadline: Duration::from_millis(config.connect_timeout_ms.max(1)),
    };
    let runtime = srt_runtime()?;
    let _runtime_guard = runtime.enter();
    let transport = if should_use_shared_srt_egress_state(
        config.peer_addrs.len(),
        config.shared_state.is_some(),
    ) {
        let state = config
            .shared_state
            .clone()
            .expect("shared SRT egress state selected by predicate");
        let caller = {
            let mut shared = state
                .lock()
                .map_err(|_| "shared SRT egress state is poisoned".to_string())?;
            if shared.is_none() {
                *shared = Some(SharedSrtEgress::bind(config.peer_addrs[0], &runtime)?);
            }
            let shared = shared.as_mut().expect("initialized above");
            let connection = session
                .caller(timestamp_now())
                .map_err(|error| error.to_string())?;
            let caller = shared
                .callers
                .add_direct(srt_transport::CallerLeg::new(
                    config.peer_addrs[0],
                    connection,
                ))
                .map_err(|error| error.to_string())?;
            shared.drive(timestamp_now())?;
            caller
        };
        RustSrtSocket::Shared { state, caller }
    } else if config.peer_addrs.len() == 1 {
        let caller = srt_transport::CallerConfig::builder(config.peer_addrs[0])
            .session(session)
            .connect(connect)
            .configure_transport(apply_optional_udp_buf)
            .build()
            .map_err(|e| e.to_string())?
            .prepare(srt_transport::RuntimeFlavor::Tokio)
            .map_err(|e| e.to_string())?;
        let socket = caller.bind_socket().map_err(|e| e.to_string())?;
        let tokio_socket = tokio::net::UdpSocket::from_std(socket).map_err(|e| e.to_string())?;
        let conn = caller
            .connection(timestamp_now())
            .map_err(|e| e.to_string())?;
        RustSrtSocket::Direct(Box::new(Conn::new(conn, tokio_socket)))
    } else {
        let legs = config
            .peer_addrs
            .iter()
            .enumerate()
            .map(|(index, peer)| {
                srt_transport::CallerConfig::builder(*peer)
                    .session(session.clone())
                    .connect(connect)
                    .configure_transport(apply_optional_udp_buf)
                    .build()
                    .map(|caller| {
                        srt_transport::GroupCallerLeg::new(
                            u32::try_from(index + 1).unwrap_or(u32::MAX),
                            u16::try_from(config.peer_addrs.len() - index).unwrap_or(u16::MAX),
                            caller,
                        )
                    })
                    .map_err(|error| error.to_string())
            })
            .collect::<Result<Vec<_>, _>>()?;
        RustSrtSocket::Bonded(Box::new(
            TokioGroupConn::caller(
                srt_transport::GroupConfig::new(next_group_id(), config.bond_type),
                legs,
                timestamp_now(),
            )
            .map_err(|error| error.to_string())?,
        ))
    };
    Ok(Box::new(transport))
}

fn percent_decode(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests;
