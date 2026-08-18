use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;
use std::time::Duration;

use mio::net::UdpSocket;
use shiguredo_srt::{
    ConnectionEvent, ConnectionOptions, ConnectionOutput, ConnectionState, ErrorKind, KeyLength,
    SrtConnection, TimerId, Timestamp,
};
use tokio::sync::mpsc::Sender;

use crate::domain::srt_ingest::ResolvedSrtCrypto;

use super::super::SrtIngestPolicyStore;
use super::super::srt_stream_id::parse_srt_stream_id;
use super::WorkerOptions;
use super::types::{ConnectionId, IngestEvent};

const PENDING_PACKET_LIMIT: usize = 256;
const PENDING_BYTE_LIMIT: usize = 4 * 1024 * 1024;
const PENDING_OUTBOUND_PACKET_LIMIT: usize = 4096;
const PENDING_OUTBOUND_BYTE_LIMIT: usize = 4 * 1024 * 1024;

pub(super) struct RustConnection {
    pub(super) id: ConnectionId,
    pub(super) peer: SocketAddr,
    pub(super) core: SrtConnection,
    pub(super) timers: HashMap<TimerId, Timestamp>,
    pub(super) authorized: bool,
    pub(super) pending: VecDeque<Vec<u8>>,
    pub(super) pending_bytes: usize,
    pub(super) pending_outbound: VecDeque<Vec<u8>>,
    pub(super) pending_outbound_bytes: usize,
}

pub(super) fn new(id: ConnectionId, peer: SocketAddr, options: &WorkerOptions) -> RustConnection {
    RustConnection {
        id,
        peer,
        core: SrtConnection::new_listener(ConnectionOptions {
            socket_id: id.serial as u32,
            passphrase: options.passphrase.clone(),
            key_length: options.key_length,
            tsbpd_delay: options.tsbpd_delay,
            ..ConnectionOptions::default()
        }),
        timers: HashMap::new(),
        authorized: false,
        pending: VecDeque::new(),
        pending_bytes: 0,
        pending_outbound: VecDeque::new(),
        pending_outbound_bytes: 0,
    }
}

pub(super) fn apply_listener_policy(
    connection: &mut RustConnection,
    policy_store: &SrtIngestPolicyStore,
    packet: &[u8],
) -> Result<(), String> {
    if connection.core.state() != ConnectionState::Listening {
        return Ok(());
    }
    let Some(identity) = srt_lifecycle::handshake_identity(packet) else {
        return Ok(());
    };
    if !identity.is_conclusion {
        return Ok(());
    }
    let Some(stream_id) = identity.stream_id else {
        return Ok(());
    };
    let parsed = parse_srt_stream_id(&stream_id);
    let Some(policy) = policy_store.resolved_policy(&parsed.stream_key) else {
        return Ok(());
    };
    let (passphrase, key_length) = match policy.crypto {
        ResolvedSrtCrypto::Plaintext => (None, KeyLength::Aes128),
        ResolvedSrtCrypto::Encrypted {
            passphrase,
            pbkeylen,
        } => (
            Some(passphrase),
            match pbkeylen {
                16 => KeyLength::Aes128,
                24 => KeyLength::Aes192,
                32 => KeyLength::Aes256,
                other => return Err(format!("unsupported SRT pbkeylen {other}")),
            },
        ),
    };
    let tsbpd_delay = u16::try_from(policy.latency_ms)
        .map_err(|_| format!("invalid SRT latency {}", policy.latency_ms))?;
    connection
        .core
        .set_listener_policy(passphrase, key_length, tsbpd_delay)
        .map_err(|error| error.to_string())
}

pub(super) fn queue_send(connection: &mut RustConnection, payload: Vec<u8>) -> bool {
    if connection.pending_outbound.len() >= PENDING_OUTBOUND_PACKET_LIMIT
        || connection
            .pending_outbound_bytes
            .saturating_add(payload.len())
            > PENDING_OUTBOUND_BYTE_LIMIT
    {
        return false;
    }
    connection.pending_outbound_bytes = connection
        .pending_outbound_bytes
        .saturating_add(payload.len());
    connection.pending_outbound.push_back(payload);
    true
}

pub(super) fn pending_send_wait(connection: &RustConnection, now: Timestamp) -> Option<Duration> {
    if connection.pending_outbound.is_empty() {
        None
    } else {
        Some(Duration::from_micros(connection.core.time_until_send(now)))
    }
}

pub(super) fn service(
    connection: &mut RustConnection,
    socket: &UdpSocket,
    events: &Sender<IngestEvent>,
    now: Timestamp,
) -> bool {
    if drain_core_outputs(
        &mut connection.core,
        socket,
        connection.peer,
        &mut connection.timers,
        now,
    )
    .is_err()
    {
        return false;
    }

    if !flush_pending(connection, socket, events, now) {
        return false;
    }

    service_events(connection, events)
}

fn flush_pending(
    connection: &mut RustConnection,
    socket: &UdpSocket,
    events: &Sender<IngestEvent>,
    now: Timestamp,
) -> bool {
    while let Some(payload) = connection.pending_outbound.front() {
        if !connection.core.can_send_with_pacing(now) {
            break;
        }
        match connection.core.send(payload, now) {
            Ok(()) => {
                let Some(payload) = connection.pending_outbound.pop_front() else {
                    return false;
                };
                connection.pending_outbound_bytes = connection
                    .pending_outbound_bytes
                    .saturating_sub(payload.len());
            }
            Err(error) if error.kind == ErrorKind::InvalidState => break,
            Err(error) => {
                let _ = send_event(
                    events,
                    IngestEvent::Disconnected {
                        id: connection.id,
                        phase: "send",
                        reason: error.to_string(),
                        had_error: true,
                    },
                );
                return false;
            }
        }
    }
    drain_core_outputs(
        &mut connection.core,
        socket,
        connection.peer,
        &mut connection.timers,
        now,
    )
    .is_ok()
}

pub(super) fn drain_core_outputs(
    core: &mut SrtConnection,
    socket: &UdpSocket,
    peer: SocketAddr,
    timers: &mut HashMap<TimerId, Timestamp>,
    now: Timestamp,
) -> io::Result<()> {
    while let Some(output) = core.poll_output() {
        match output {
            ConnectionOutput::SendPacket(packet) => match socket.send_to(&packet, peer) {
                Ok(_) => {}
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                Err(error) => return Err(error),
            },
            ConnectionOutput::SetTimer {
                id,
                duration_micros,
            } => {
                timers.insert(id, now.add_micros(duration_micros));
            }
            ConnectionOutput::ClearTimer { id } => {
                timers.remove(&id);
            }
        }
    }
    Ok(())
}

fn service_events(connection: &mut RustConnection, events: &Sender<IngestEvent>) -> bool {
    while let Some(event) = connection.core.poll_event() {
        match event {
            ConnectionEvent::Connected => {
                let stream_id = connection
                    .core
                    .peer_stream_id()
                    .unwrap_or_default()
                    .to_string();
                let group = connection.core.peer_group_extension();
                let peer_socket_id = connection.core.peer_socket_id();
                if !send_event(
                    events,
                    IngestEvent::Connected {
                        id: connection.id,
                        peer: connection.peer,
                        stream_id,
                        group,
                        peer_socket_id,
                    },
                ) {
                    return false;
                }
                break;
            }
            ConnectionEvent::DataReceived { payload, .. } => {
                if connection.authorized {
                    if !send_event(
                        events,
                        IngestEvent::Data {
                            id: connection.id,
                            payload,
                        },
                    ) {
                        return false;
                    }
                } else if connection.pending.len() >= PENDING_PACKET_LIMIT
                    || connection.pending_bytes.saturating_add(payload.len()) > PENDING_BYTE_LIMIT
                {
                    let _ = send_event(
                        events,
                        IngestEvent::Disconnected {
                            id: connection.id,
                            phase: "receive",
                            reason: "ingest authorization buffer full".to_string(),
                            had_error: true,
                        },
                    );
                    return false;
                } else {
                    connection.pending_bytes =
                        connection.pending_bytes.saturating_add(payload.len());
                    connection.pending.push_back(payload);
                }
            }
            ConnectionEvent::Disconnected { reason } => {
                let _ = send_event(
                    events,
                    IngestEvent::Disconnected {
                        id: connection.id,
                        phase: "disconnect",
                        reason,
                        had_error: false,
                    },
                );
                return false;
            }
            ConnectionEvent::Error(error) => {
                let _ = send_event(
                    events,
                    IngestEvent::Disconnected {
                        id: connection.id,
                        phase: "receive",
                        reason: error,
                        had_error: true,
                    },
                );
                return false;
            }
            ConnectionEvent::StateChanged(_) | ConnectionEvent::KeyRefreshNeeded { .. } => {}
        }
    }
    true
}

fn send_event(events: &Sender<IngestEvent>, event: IngestEvent) -> bool {
    events.blocking_send(event).is_ok()
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::domain::srt_ingest::{
        SrtGlobalIngestConfig, SrtPipelineIngestConfig, SrtPipelineIngestMode,
    };

    use super::super::super::{SrtIngestPolicyEntry, SrtIngestPolicyStore};
    use super::*;

    fn exchange(caller: &mut SrtConnection, listener: &mut SrtConnection, now: Timestamp) {
        while let Some(ConnectionOutput::SendPacket(packet)) = caller.poll_output() {
            let _ = listener.feed_recv_buf(&packet, now);
        }
        while let Some(ConnectionOutput::SendPacket(packet)) = listener.poll_output() {
            let _ = caller.feed_recv_buf(&packet, now);
        }
    }

    fn next_send_packet(connection: &mut SrtConnection) -> Vec<u8> {
        while let Some(output) = connection.poll_output() {
            if let ConnectionOutput::SendPacket(packet) = output {
                return packet;
            }
        }
        panic!("connection did not emit a packet");
    }

    fn connected_listener() -> SrtConnection {
        let mut caller = SrtConnection::new_caller(ConnectionOptions {
            tsbpd_delay: 0,
            ..ConnectionOptions::default()
        });
        let mut listener = SrtConnection::new_listener(ConnectionOptions {
            tsbpd_delay: 0,
            ..ConnectionOptions::default()
        });
        caller
            .connect(Timestamp::from_micros(0))
            .expect("caller connects");
        for round in 0..10 {
            exchange(
                &mut caller,
                &mut listener,
                Timestamp::from_micros(round * 10_000),
            );
            if listener.state() == shiguredo_srt::ConnectionState::Connected {
                return listener;
            }
        }
        panic!("listener did not connect");
    }

    #[test]
    fn listener_policy_helper_applies_stream_crypto_before_conclusion() {
        let passphrase = "stream-policy-passphrase".to_string();
        let mut pipeline_policy = SrtPipelineIngestConfig {
            mode: SrtPipelineIngestMode::Encrypted,
            passphrase: Some(passphrase.clone()),
            pbkeylen: Some(32),
            latency_ms: Some(2_000),
        };
        pipeline_policy.validate().expect("policy validates");
        let policy_store = Arc::new(SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &[SrtIngestPolicyEntry::new(
                "pipeline-1",
                "encrypted-stream",
                pipeline_policy,
            )],
        ));
        let mut caller = SrtConnection::new_caller(ConnectionOptions {
            passphrase: Some(passphrase),
            crypto_salt: Some([0x24; 16]),
            key_length: shiguredo_srt::KeyLength::Aes256,
            stream_id: Some("publish:encrypted-stream".to_string()),
            tsbpd_delay: 0,
            ..ConnectionOptions::default()
        });
        let mut connection = new(
            ConnectionId {
                worker: 0,
                serial: 1,
            },
            "127.0.0.1:29001".parse().expect("peer address parses"),
            &WorkerOptions {
                passphrase: None,
                key_length: shiguredo_srt::KeyLength::Aes128,
                tsbpd_delay: 250,
            },
        );

        caller
            .connect(Timestamp::from_micros(0))
            .expect("caller connects");
        let induction = next_send_packet(&mut caller);
        connection
            .core
            .feed_recv_buf(&induction, Timestamp::from_micros(0))
            .expect("listener receives induction");
        let induction_response = next_send_packet(&mut connection.core);
        caller
            .feed_recv_buf(&induction_response, Timestamp::from_micros(0))
            .expect("caller receives induction response");
        let conclusion = next_send_packet(&mut caller);

        apply_listener_policy(&mut connection, &policy_store, &conclusion)
            .expect("policy applies before conclusion");
        connection
            .core
            .feed_recv_buf(&conclusion, Timestamp::from_micros(10_000))
            .expect("listener accepts encrypted conclusion");
        assert_eq!(
            connection.core.state(),
            shiguredo_srt::ConnectionState::Connected
        );
    }

    #[test]
    fn outbound_queue_rejects_byte_limit() {
        let mut connection = new(
            ConnectionId {
                worker: 0,
                serial: 1,
            },
            "127.0.0.1:29001".parse().expect("peer address parses"),
            &WorkerOptions {
                passphrase: None,
                key_length: shiguredo_srt::KeyLength::Aes128,
                tsbpd_delay: 0,
            },
        );

        assert!(queue_send(
            &mut connection,
            vec![0; PENDING_OUTBOUND_BYTE_LIMIT]
        ));
        assert!(!queue_send(&mut connection, vec![0]));
    }

    #[test]
    fn pending_send_wait_uses_core_pacing_deadline() {
        let mut connection = new(
            ConnectionId {
                worker: 0,
                serial: 1,
            },
            "127.0.0.1:29001".parse().expect("peer address parses"),
            &WorkerOptions {
                passphrase: None,
                key_length: shiguredo_srt::KeyLength::Aes128,
                tsbpd_delay: 0,
            },
        );
        connection.core = connected_listener();
        let now = Timestamp::from_micros(100_000);
        connection
            .core
            .send(&[0; 1316], now)
            .expect("connected core sends");
        assert!(queue_send(&mut connection, vec![0; 1316]));

        let wait = pending_send_wait(&connection, now).expect("pending send has a deadline");
        assert!(wait > Duration::ZERO);
        assert!(wait < Duration::from_millis(20));
    }
}
