//! Native owner-thread adapter for the external `srt-rs` protocol core.
//!
//! The protocol crate is sans-I/O. This module owns the small amount of
//! application transport state needed by Restream: one native UDP socket per
//! local address family, one protocol caller table, and one manual timer
//! store. The egress fabric continues to own scheduling and lifecycle; this
//! adapter only moves datagrams through the shard's native readiness owner.
//!
//! Each `srt-rs` logical caller (`RustSrtSocket`) is owned directly by the
//! `SrtFabricLeaf` that connected it (boxed as `dyn SrtMessageSender`, since
//! `RustSrtSocket` implements that trait directly below) while the physical
//! UDP sockets and caller table remain shared per shard.

use std::net::SocketAddr;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use crate::media::egress::backend::CloseReason;
use crate::media::egress::backends::srt::muxer_ports::SrtEgressMuxerPortState;
use crate::media::snapshots::PublisherQuality;
use bytes::Bytes;
use shiguredo_srt::Timestamp;
use srt_transport::{LogicalCallerId, LogicalCallerStats};

mod knobs;
pub(crate) use knobs::{apply_optional_udp_buf, desired_udp_buf};
pub use knobs::{recv_budget, recv_budget_or};

#[cfg(test)]
fn should_use_shared_srt_egress_state(peer_count: usize, has_shared_state: bool) -> bool {
    peer_count != 0 && has_shared_state
}

enum RustSrtSocket {
    Shared {
        state: SrtEgressMuxerPortState,
        caller: LogicalCallerId,
    },
}

mod shared;
pub(crate) use shared::{SharedSrtEgress, SrtNativeMetrics};

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
    fn send_shared(&mut self, message: &Bytes) -> SrtSendResult {
        let Self::Shared { state, caller } = self;
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
        match shared.send_shared(*caller, message, timestamp_now()) {
            Ok(0) => SrtSendResult::WouldBlock,
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
            Err(error) if error.kind == shiguredo_srt::ErrorKind::InvalidState => {
                SrtSendResult::PeerClosed
            }
            Err(error) => SrtSendResult::Failed {
                reason: "srt-rs-send",
                detail: error.to_string(),
                retryable: true,
            },
        }
    }

    fn native_send_backlog_inner(&self) -> Option<NativeSendBacklog> {
        match self {
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
        self.send_shared(message)
    }

    /// Native shared leaves do nothing here: their sockets and caller table
    /// are driven once per shard readiness pass rather than once per leaf.
    fn drive(&mut self) {
        let _ = self;
    }

    /// Disconnects this leaf's logical caller from the shard's shared UDP
    /// socket/table without tearing down the socket other callers use.
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

/// Native SRT needs no private runtime or worker pool to initialize.
pub(crate) fn ensure_srt_native() -> Result<(), String> {
    Ok(())
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
                .and_then(|shared| shared.as_ref().and_then(SharedSrtEgress::local_port))
        })
    }
}

/// Connects a new SRT egress transport and hands it back directly — the
/// caller boxes it as `dyn SrtMessageSender` and the leaf owns it for its
/// whole lifetime; there is no id-keyed registry to look it back up through.
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
    let state = config
        .shared_state
        .clone()
        .unwrap_or_else(|| std::sync::Arc::new(std::sync::Mutex::new(None)));
    let caller = {
        let mut shared = state
            .lock()
            .map_err(|_| "shared SRT egress state is poisoned".to_string())?;
        if let Some(existing) = shared.as_mut() {
            existing.ensure_for_peers(config.peer_addrs)?;
        } else {
            *shared = Some(SharedSrtEgress::bind_for_peers(config.peer_addrs)?);
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
            let mode = shiguredo_srt::GroupMode::from_group_type(config.bond_type)
                .ok_or_else(|| "invalid SRT group type".to_string())?;
            let legs = config
                .peer_addrs
                .iter()
                .enumerate()
                .map(|(index, peer)| {
                    let caller = srt_transport::CallerConfig::builder(*peer)
                        .session(session.clone())
                        .connect(connect)
                        .configure_transport(apply_optional_udp_buf)
                        .build()
                        .map_err(|error| error.to_string())?
                        .prepare(srt_transport::RuntimeFlavor::Mio)
                        .map_err(|error| error.to_string())?;
                    let connection = caller
                        .connection(timestamp_now())
                        .map_err(|error| error.to_string())?;
                    Ok(srt_transport::CallerGroupLeg::new(
                        u32::try_from(index + 1).unwrap_or(u32::MAX),
                        u16::try_from(config.peer_addrs.len() - index).unwrap_or(u16::MAX),
                        *peer,
                        connection,
                    ))
                })
                .collect::<Result<Vec<_>, String>>()?;
            shared
                .callers
                .add_group(next_group_id(), mode, legs)
                .map_err(|error| error.to_string())?
        };
        shared.drive(timestamp_now())?;
        caller
    };
    let transport = RustSrtSocket::Shared { state, caller };
    Ok(Box::new(transport))
}

fn percent_decode(value: &str) -> String {
    percent_encoding::percent_decode_str(value)
        .decode_utf8_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests;
