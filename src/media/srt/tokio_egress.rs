//! Runtime adapter for the external `srt-rs` protocol core.
//!
//! The protocol crate is sans-I/O. This module owns the small amount of
//! application transport state needed by Restream: one nonblocking UDP
//! socket, one protocol connection, and one manual timer store. The egress
//! fabric continues to own scheduling and lifecycle; this adapter only moves
//! datagrams through Tokio-owned UDP sockets and `SrtConnection`.

use std::collections::{HashMap, HashSet};
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::AsRawFd;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::media::egress::backend::CloseReason;
use crate::media::egress::backends::srt::muxer_ports::SrtEgressMuxerPortState;
use crate::media::egress::scheduler::LeafKey;
use bytes::Bytes;
use shiguredo_srt::{ConnectionState, Timestamp};
use srt_transport::OutputDrainBudget;
use srt_transport::tokio_transport::{Conn, GroupConn as TokioGroupConn};
use srt_transport::{LogicalCallerId, LogicalCallerState, LogicalCallerStats};
use tokio::net::UdpSocket;

#[allow(clippy::upper_case_acronyms)]
pub(crate) type SRTSOCKET = i32;

/// Compatibility snapshot retained for the egress quality/test contract.
/// srt-rs does not expose the native `BStats` ABI; live transport metrics are
/// currently optional, while fakes can still exercise quality conversion.
#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub(crate) struct SrtTraceBStats {
    pub ms_rtt: f64,
    pub mbps_send_rate: f64,
    pub pkt_snd_loss_total: i32,
    pub pkt_snd_drop_total: i32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct SrtEgressInterest {
    pub writable: bool,
}

impl SrtEgressInterest {
    pub const WRITE: Self = Self { writable: true };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct SrtReadyLeaf {
    pub socket: SRTSOCKET,
    pub key: LeafKey,
    pub generation: u64,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SrtEgressPollError {
    pub operation: &'static str,
    pub code: i32,
    pub message: String,
}

impl SrtEgressPollError {}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SrtEgressSendMode {
    FabricNonblocking,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SrtEgressSocketError {
    pub(crate) option: &'static str,
    pub(crate) code: i32,
    pub(crate) message: String,
}

impl std::fmt::Display for SrtEgressSocketError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.message.fmt(f)
    }
}

impl std::error::Error for SrtEgressSocketError {}

pub(crate) struct SrtFabricPoller {
    runtime: std::sync::Arc<tokio::runtime::Runtime>,
    registered: HashMap<SRTSOCKET, (LeafKey, u64)>,
}

impl SrtFabricPoller {
    pub(crate) fn new(_max_events: usize) -> Result<Self, SrtEgressPollError> {
        Ok(Self {
            runtime: srt_runtime().map_err(|e| SrtEgressPollError {
                operation: "tokio_runtime_create",
                code: -1,
                message: e,
            })?,
            registered: HashMap::new(),
        })
    }

    pub(crate) fn register_leaf(
        &mut self,
        socket: SRTSOCKET,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        if !registry()
            .lock()
            .map_err(|_| SrtEgressPollError {
                operation: "srt_registry_lock",
                code: -1,
                message: "SRT registry poisoned".to_string(),
            })?
            .contains_key(&socket)
        {
            return Err(SrtEgressPollError {
                operation: "srt_register_leaf",
                code: -1,
                message: format!("unknown srt-rs socket {socket}"),
            });
        }
        let _ = interest;
        self.registered.insert(socket, (key, generation));
        Ok(())
    }

    pub(crate) fn remove(&mut self, socket: SRTSOCKET) -> Result<(), SrtEgressPollError> {
        self.registered.remove(&socket);
        Ok(())
    }

    pub(crate) fn poll_leaves(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        ready.clear();
        if timeout_ms > 0 && self.registered.is_empty() {
            self.runtime
                .block_on(tokio::time::sleep(Duration::from_millis(timeout_ms as u64)));
        }
        let registered: Vec<_> = self.registered.iter().map(|(s, v)| (*s, *v)).collect();
        let mut driven_shared = HashSet::new();
        for (socket, (key, generation)) in registered {
            let _live = with_socket(socket, |conn| {
                let now = timestamp_now();
                match conn {
                    RustSrtSocket::Shared { state, .. } => {
                        let identity = Arc::as_ptr(state) as usize;
                        if driven_shared.insert(identity) {
                            conn.drive(now, &self.runtime)
                        } else {
                            true
                        }
                    }
                    _ => conn.drive(now, &self.runtime),
                }
            })
            .unwrap_or(false);
            // Keep terminal connections ready as well. The egress leaf must
            // visit a disconnected connection once so `send_message` can
            // return `PeerClosed` and drive the normal retry/cleanup path;
            // dropping it from readiness strands the leaf in `sending`.
            ready.push(SrtReadyLeaf {
                socket,
                key,
                generation,
                writable: true,
            });
        }
        Ok(ready.len())
    }
}

enum RustSrtSocket {
    Direct(Box<Conn>),
    Bonded(TokioGroupConn),
    Shared {
        state: SrtEgressMuxerPortState,
        caller: LogicalCallerId,
    },
}

pub(crate) struct SharedSrtEgress {
    socket: UdpSocket,
    callers: srt_transport::CallerTable,
    outbound: Vec<(SocketAddr, Vec<u8>)>,
}

impl SharedSrtEgress {
    #[cfg(test)]
    pub(crate) fn local_port(&self) -> Option<u16> {
        self.socket.local_addr().ok().map(|address| address.port())
    }

    fn bind(peer: SocketAddr) -> Result<Self, String> {
        let bind = match peer.ip() {
            IpAddr::V4(_) => SocketAddr::new(IpAddr::V4(Ipv4Addr::UNSPECIFIED), 0),
            IpAddr::V6(_) => SocketAddr::new(IpAddr::V6(Ipv6Addr::UNSPECIFIED), 0),
        };
        let socket = std::net::UdpSocket::bind(bind).map_err(|error| error.to_string())?;
        socket
            .set_nonblocking(true)
            .map_err(|error| error.to_string())?;
        srt_transport::set_sock_bufs(socket.as_raw_fd(), DESIRED_UDP_BUF)
            .map_err(|error| error.to_string())?;
        let socket = UdpSocket::from_std(socket).map_err(|error| error.to_string())?;
        Ok(Self {
            socket,
            callers: srt_transport::CallerTable::new(),
            outbound: Vec::new(),
        })
    }

    fn drive(&mut self, now: Timestamp, runtime: &tokio::runtime::Runtime) -> Result<(), String> {
        let mut buffer = [0_u8; 2048];
        loop {
            match self.socket.try_recv_from(&mut buffer) {
                Ok((size, peer)) => self
                    .callers
                    .feed(peer, &buffer[..size], now)
                    .map_err(|error| error.to_string())?,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => return Err(error.to_string()),
            };
        }
        self.callers
            .poll_outbound_bounded(now, OutputDrainBudget::default(), &mut self.outbound);
        for (peer, packet) in self.outbound.drain(..) {
            runtime
                .block_on(self.socket.send_to(&packet, peer))
                .map_err(|error| error.to_string())?;
        }
        Ok(())
    }
}

impl RustSrtSocket {
    fn drive(&mut self, now: Timestamp, runtime: &tokio::runtime::Runtime) -> bool {
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
            Self::Shared { state, caller } => state
                .lock()
                .ok()
                .and_then(|mut state| {
                    let shared = state.as_mut()?;
                    let _ = shared.drive(now, runtime);
                    shared.callers.logical_caller(caller)?.state()
                })
                .is_some_and(|state| state != LogicalCallerState::Disconnected),
        }
    }

    fn send_message(
        &mut self,
        message: &Bytes,
        runtime: &tokio::runtime::Runtime,
    ) -> SrtSendResult {
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
                match conn.send(message, timestamp_now()) {
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
                        match caller.send(message, timestamp_now()) {
                            Ok(_) => match shared.drive(timestamp_now(), runtime) {
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

    fn native_send_backlog(&self) -> Option<NativeSendBacklog> {
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

static SOCKETS: OnceLock<Mutex<HashMap<SRTSOCKET, RustSrtSocket>>> = OnceLock::new();
static NEXT_SOCKET: OnceLock<Mutex<SRTSOCKET>> = OnceLock::new();
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

fn registry() -> &'static Mutex<HashMap<SRTSOCKET, RustSrtSocket>> {
    SOCKETS.get_or_init(|| Mutex::new(HashMap::new()))
}

pub(super) fn timestamp_now() -> Timestamp {
    let start = CLOCK.get_or_init(Instant::now);
    Timestamp::from_micros(start.elapsed().as_micros().min(u128::from(u64::MAX)) as u64)
}

fn with_socket<R>(socket: SRTSOCKET, f: impl FnOnce(&mut RustSrtSocket) -> R) -> Option<R> {
    let mut sockets = registry().lock().ok()?;
    sockets.get_mut(&socket).map(f)
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
    match conn.conn.send(message, timestamp_now()) {
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
    let mut buf = [0u8; 2048];
    loop {
        match conn.sock.try_recv(&mut buf) {
            Ok(size) => {
                let _ = conn.conn.feed_recv_buf(&buf[..size], now);
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
            Err(_) => break,
        }
    }
}

pub(crate) trait SrtMessageSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult;
    fn close(&mut self, reason: CloseReason);
    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        None
    }
    fn sender_quality_stats(&self) -> Option<SrtTraceBStats> {
        None
    }
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
    fn sender_quality_stats(&self) -> Option<SrtTraceBStats> {
        (**self).sender_quality_stats()
    }
}

struct RustSrtMessageSender {
    socket: Option<SRTSOCKET>,
}

impl SrtMessageSender for RustSrtMessageSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        let Some(socket) = self.socket else {
            return SrtSendResult::PeerClosed;
        };
        let Ok(runtime) = srt_runtime() else {
            return SrtSendResult::Failed {
                reason: "srt-rs-runtime",
                detail: "Tokio runtime unavailable".to_string(),
                retryable: true,
            };
        };
        let Some(result) = with_socket(socket, |conn| conn.send_message(message, &runtime)) else {
            return SrtSendResult::PeerClosed;
        };
        result
    }

    fn close(&mut self, _reason: CloseReason) {
        if let Some(socket) = self.socket.take() {
            let _ = registry().lock().map(|mut sockets| {
                if let Some(RustSrtSocket::Shared { state, caller }) = sockets.remove(&socket)
                    && let Ok(mut shared) = state.lock()
                {
                    let Some(shared) = shared.as_mut() else {
                        return;
                    };
                    if let Some(mut caller) = shared.callers.logical_caller_mut(&caller) {
                        caller.disconnect(timestamp_now());
                    }
                    let _ = shared.callers.remove(caller);
                }
            });
        }
    }

    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        let socket = self.socket?;
        with_socket(socket, |conn| conn.native_send_backlog()).flatten()
    }

    fn sender_quality_stats(&self) -> Option<SrtTraceBStats> {
        let socket = self.socket?;
        with_socket(socket, |conn| match conn {
            RustSrtSocket::Direct(conn) => {
                let stats = conn.conn.sender_stats()?;
                Some(SrtTraceBStats {
                    ms_rtt: stats.peer_rtt_micros.map_or(0.0, f64::from) / 1_000.0,
                    mbps_send_rate: stats
                        .peer_receiving_rate_bytes_per_second
                        .map_or(0.0, |rate| rate as f64 / 1_000_000.0),
                    pkt_snd_loss_total: i32::try_from(stats.total_lost).unwrap_or(i32::MAX),
                    pkt_snd_drop_total: i32::try_from(stats.total_dropped).unwrap_or(i32::MAX),
                })
            }
            RustSrtSocket::Bonded(conn) => {
                let stats = conn.stats();
                let mut rtt_total = 0_f64;
                let mut rtt_count = 0_u64;
                let mut rate = 0_f64;
                for leg in &stats.legs {
                    if let Some(sender) = &leg.connection.sender {
                        if let Some(rtt) = sender.peer_rtt_micros {
                            rtt_total += f64::from(rtt);
                            rtt_count += 1;
                        }
                        rate += sender
                            .peer_receiving_rate_bytes_per_second
                            .map_or(0.0, |value| value as f64);
                    }
                }
                Some(SrtTraceBStats {
                    ms_rtt: if rtt_count == 0 {
                        0.0
                    } else {
                        rtt_total / rtt_count as f64 / 1_000.0
                    },
                    mbps_send_rate: rate / 1_000_000.0,
                    pkt_snd_loss_total: i32::try_from(stats.aggregate.wire_sender_packets_lost)
                        .unwrap_or(i32::MAX),
                    pkt_snd_drop_total: 0,
                })
            }
            RustSrtSocket::Shared { state, caller } => {
                let shared = state.lock().ok()?;
                let shared = shared.as_ref()?;
                match shared.callers.logical_caller(caller)?.stats()? {
                    LogicalCallerStats::Direct(stats) => {
                        let sender = stats.sender?;
                        Some(SrtTraceBStats {
                            ms_rtt: sender.peer_rtt_micros.map_or(0.0, f64::from) / 1_000.0,
                            mbps_send_rate: sender
                                .peer_receiving_rate_bytes_per_second
                                .map_or(0.0, |rate| rate as f64 / 1_000_000.0),
                            pkt_snd_loss_total: i32::try_from(sender.total_lost)
                                .unwrap_or(i32::MAX),
                            pkt_snd_drop_total: i32::try_from(sender.total_dropped)
                                .unwrap_or(i32::MAX),
                        })
                    }
                    LogicalCallerStats::Group(stats) => {
                        let mut rtt_total = 0_f64;
                        let mut rtt_count = 0_u64;
                        let mut rate = 0_f64;
                        for leg in &stats.legs {
                            if let Some(sender) = &leg.connection.sender {
                                if let Some(rtt) = sender.peer_rtt_micros {
                                    rtt_total += f64::from(rtt);
                                    rtt_count += 1;
                                }
                                rate += sender
                                    .peer_receiving_rate_bytes_per_second
                                    .map_or(0.0, |value| value as f64);
                            }
                        }
                        Some(SrtTraceBStats {
                            ms_rtt: if rtt_count == 0 {
                                0.0
                            } else {
                                rtt_total / rtt_count as f64 / 1_000.0
                            },
                            mbps_send_rate: rate / 1_000_000.0,
                            pkt_snd_loss_total: i32::try_from(
                                stats.aggregate.wire_sender_packets_lost,
                            )
                            .unwrap_or(i32::MAX),
                            pkt_snd_drop_total: 0,
                        })
                    }
                }
            }
        })
        .flatten()
    }
}

pub(crate) fn srt_fabric_message_sender(socket: SRTSOCKET) -> Box<dyn SrtMessageSender + Send> {
    Box::new(RustSrtMessageSender {
        socket: Some(socket),
    })
}

pub(crate) fn configure_connected_srt_egress_socket(
    socket: SRTSOCKET,
    _mode: SrtEgressSendMode,
) -> Result<(), SrtEgressSocketError> {
    registry()
        .lock()
        .map_err(|_| SrtEgressSocketError {
            option: "srt-rs",
            code: -1,
            message: "SRT registry poisoned".to_string(),
        })?
        .contains_key(&socket)
        .then_some(())
        .ok_or_else(|| SrtEgressSocketError {
            option: "srt-rs",
            code: -1,
            message: format!("unknown srt-rs socket {socket}"),
        })
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

pub(crate) fn connect_fabric_srt_egress_socket(
    config: SrtFabricEgressConnectConfig<'_>,
) -> Result<SRTSOCKET, String> {
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
    let id = {
        let next = NEXT_SOCKET.get_or_init(|| Mutex::new(10));
        let mut next = next.lock().map_err(|_| "SRT socket id lock poisoned")?;
        let id = *next;
        *next = next.saturating_add(1);
        id
    };
    let transport = if let Some(state) = config.shared_state.clone() {
        let caller = {
            let mut shared = state
                .lock()
                .map_err(|_| "shared SRT egress state is poisoned".to_string())?;
            if shared.is_none() {
                *shared = Some(SharedSrtEgress::bind(config.peer_addrs[0])?);
            }
            let shared = shared.as_mut().expect("initialized above");
            let caller = if config.peer_addrs.len() == 1 {
                let connection = session
                    .caller(timestamp_now())
                    .map_err(|error| error.to_string())?;
                shared
                    .callers
                    .add_direct(srt_transport::CallerLeg::new(
                        config.peer_addrs[0],
                        connection,
                    ))
                    .map_err(|error| error.to_string())?
            } else {
                let mode = shiguredo_srt::GroupMode::from_group_type(config.bond_type).ok_or_else(
                    || "bonded SRT egress requires broadcast or backup mode".to_string(),
                )?;
                let legs = config
                    .peer_addrs
                    .iter()
                    .enumerate()
                    .map(|(index, peer)| {
                        session
                            .caller(timestamp_now())
                            .map(|connection| {
                                srt_transport::CallerGroupLeg::new(
                                    u32::try_from(index + 1).unwrap_or(u32::MAX),
                                    u16::try_from(config.peer_addrs.len() - index)
                                        .unwrap_or(u16::MAX),
                                    *peer,
                                    connection,
                                )
                            })
                            .map_err(|error| error.to_string())
                    })
                    .collect::<Result<Vec<_>, _>>()?;
                shared
                    .callers
                    .add_group(id as u32, mode, legs)
                    .map_err(|error| error.to_string())?
            };
            shared.drive(timestamp_now(), &runtime)?;
            caller
        };
        RustSrtSocket::Shared { state, caller }
    } else if config.peer_addrs.len() == 1 {
        let caller = srt_transport::CallerConfig::builder(config.peer_addrs[0])
            .session(session)
            .connect(connect)
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
        RustSrtSocket::Bonded(
            TokioGroupConn::caller(
                srt_transport::GroupConfig::new(id as u32, config.bond_type),
                legs,
                timestamp_now(),
            )
            .map_err(|error| error.to_string())?,
        )
    };
    registry()
        .lock()
        .map_err(|_| "SRT registry poisoned".to_string())?
        .insert(id, transport);
    Ok(id)
}

pub(crate) const DESIRED_UDP_BUF: usize = 8 * 1024 * 1024;

fn percent_decode(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .into_owned()
}
