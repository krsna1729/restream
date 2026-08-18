use std::collections::{HashMap, VecDeque};
use std::io;
use std::net::SocketAddr;

use mio::net::UdpSocket;
use shiguredo_srt::{
    ConnectionEvent, ConnectionOptions, ConnectionOutput, SrtConnection, TimerId, Timestamp,
};
use tokio::sync::mpsc::Sender;

use super::super::types::{ConnectionId, IngestEvent};
use super::WorkerOptions;

const PENDING_PACKET_LIMIT: usize = 256;
const PENDING_BYTE_LIMIT: usize = 4 * 1024 * 1024;

pub(super) struct RustConnection {
    pub(super) id: ConnectionId,
    pub(super) peer: SocketAddr,
    pub(super) core: SrtConnection,
    pub(super) timers: HashMap<TimerId, Timestamp>,
    pub(super) authorized: bool,
    pub(super) pending: VecDeque<Vec<u8>>,
    pub(super) pending_bytes: usize,
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
    }
}

pub(super) fn service(
    connection: &mut RustConnection,
    socket: &UdpSocket,
    events: &Sender<IngestEvent>,
    now: Timestamp,
) -> bool {
    while let Some(output) = connection.core.poll_output() {
        match output {
            ConnectionOutput::SendPacket(packet) => {
                match socket.send_to(&packet, connection.peer) {
                    Ok(_) => {}
                    Err(error) if error.kind() == io::ErrorKind::WouldBlock => {}
                    Err(error) => {
                        tracing::debug!(peer = %connection.peer, %error, "Rust SRT ingest response failed");
                        return false;
                    }
                }
            }
            ConnectionOutput::SetTimer {
                id,
                duration_micros,
            } => {
                connection
                    .timers
                    .insert(id, now.add_micros(duration_micros));
            }
            ConnectionOutput::ClearTimer { id } => {
                connection.timers.remove(&id);
            }
        }
    }

    while let Some(event) = connection.core.poll_event() {
        match event {
            ConnectionEvent::Connected => {
                let stream_id = connection
                    .core
                    .peer_stream_id()
                    .unwrap_or_default()
                    .to_string();
                if !send_event(
                    events,
                    IngestEvent::Connected {
                        id: connection.id,
                        peer: connection.peer,
                        stream_id,
                    },
                ) {
                    return false;
                }
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
