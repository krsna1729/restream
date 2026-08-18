use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use mio::net::UdpSocket;
use shiguredo_srt::Timestamp;
use tokio::sync::mpsc::{Receiver, Sender, error::TryRecvError};

use super::super::connection::{self, RustConnection};
use super::super::types::{ConnectionId, IngestEvent, WorkerCommand};
use super::WorkerOptions;

const POLL_MAX_WAIT: Duration = Duration::from_millis(20);

pub(super) fn process_commands(
    commands: &mut Receiver<WorkerCommand>,
    connections: &mut HashMap<SocketAddr, RustConnection>,
    socket: &UdpSocket,
    events: &Sender<IngestEvent>,
    now: Timestamp,
) -> bool {
    loop {
        match commands.try_recv() {
            Ok(WorkerCommand::Stop) | Err(TryRecvError::Disconnected) => return true,
            Err(TryRecvError::Empty) => return false,
            Ok(WorkerCommand::Authorize {
                id,
                logical_id: _,
                accepted,
            }) => {
                let peer = connections
                    .iter()
                    .find_map(|(peer, connection)| (connection.id == id).then_some(*peer));
                let Some(peer) = peer else {
                    continue;
                };
                if !accepted {
                    connections.remove(&peer);
                    let _ = send_event(
                        events,
                        IngestEvent::Disconnected {
                            id,
                            phase: "authorize",
                            reason: "ingest authorization rejected".to_string(),
                            had_error: true,
                        },
                    );
                    continue;
                }
                let Some(connection) = connections.get_mut(&peer) else {
                    continue;
                };
                connection.authorized = true;
                while let Some(payload) = connection.pending.pop_front() {
                    connection.pending_bytes =
                        connection.pending_bytes.saturating_sub(payload.len());
                    if !send_event(events, IngestEvent::Data { id, payload }) {
                        return true;
                    }
                }
                if !connection::service(connection, socket, events, now) {
                    connections.remove(&peer);
                }
            }
            Ok(WorkerCommand::Send { id, payload }) => {
                let peer = connections
                    .iter()
                    .find_map(|(peer, connection)| (connection.id == id).then_some(*peer));
                let Some(peer) = peer else {
                    continue;
                };
                let Some(connection) = connections.get_mut(&peer) else {
                    continue;
                };
                if !connection::queue_send(connection, payload) {
                    connections.remove(&peer);
                    let _ = send_event(
                        events,
                        IngestEvent::Disconnected {
                            id,
                            phase: "send",
                            reason: "Rust SRT outbound queue full".to_string(),
                            had_error: true,
                        },
                    );
                } else if !connection::service(connection, socket, events, now) {
                    connections.remove(&peer);
                }
            }
            Ok(WorkerCommand::Close { id, reason }) => {
                let peer = connections
                    .iter()
                    .find_map(|(peer, connection)| (connection.id == id).then_some(*peer));
                if let Some(peer) = peer {
                    connections.remove(&peer);
                    let _ = send_event(
                        events,
                        IngestEvent::Disconnected {
                            id,
                            phase: "close",
                            reason,
                            had_error: false,
                        },
                    );
                }
            }
            Ok(WorkerCommand::Handoff { .. } | WorkerCommand::ForwardPacket { .. }) => {}
        }
    }
}

pub(super) struct ReceiveState<'a> {
    pub(super) socket: &'a mut UdpSocket,
    pub(super) connections: &'a mut HashMap<SocketAddr, RustConnection>,
    pub(super) events: &'a Sender<IngestEvent>,
    pub(super) options: &'a WorkerOptions,
    pub(super) worker_index: usize,
    pub(super) next_socket_id: &'a mut u32,
    pub(super) start: Instant,
    pub(super) packet: &'a mut [u8],
}

pub(super) fn receive_packets(state: &mut ReceiveState<'_>) -> bool {
    let ReceiveState {
        socket,
        connections,
        events,
        options,
        worker_index,
        next_socket_id,
        start,
        packet,
    } = state;
    loop {
        let (size, peer) = match socket.recv_from(packet) {
            Ok(received) => received,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return true,
            Err(error) => {
                tracing::debug!(%error, "Rust SRT ingest receive failed");
                return true;
            }
        };
        let now = timestamp(*start);
        let connection = connections.entry(peer).or_insert_with(|| {
            let id = ConnectionId {
                worker: *worker_index,
                serial: u64::from(**next_socket_id),
            };
            **next_socket_id = (**next_socket_id).wrapping_add(1).max(1);
            connection::new(id, peer, options)
        });
        if let Err(error) = connection.core.feed_recv_buf(&packet[..size], now) {
            tracing::debug!(%peer, %error, "Rust SRT ingest rejected datagram");
            let id = connection.id;
            connections.remove(&peer);
            if !send_event(
                events,
                IngestEvent::Disconnected {
                    id,
                    phase: "receive",
                    reason: error.to_string(),
                    had_error: true,
                },
            ) {
                return false;
            }
            continue;
        }
        if !connection::service(connection, socket, events, now) {
            connections.remove(&peer);
        }
    }
}

pub(super) fn service_timers(
    connections: &mut HashMap<SocketAddr, RustConnection>,
    socket: &mut UdpSocket,
    events: &Sender<IngestEvent>,
    now: Timestamp,
) -> bool {
    let peers = connections.keys().copied().collect::<Vec<_>>();
    for peer in peers {
        let Some(connection) = connections.get_mut(&peer) else {
            continue;
        };
        let due = connection
            .timers
            .iter()
            .filter_map(|(id, deadline)| (now >= *deadline).then_some(*id))
            .collect::<Vec<_>>();
        for id in due {
            connection.timers.remove(&id);
            if let Err(error) = connection.core.handle_timer(id, now) {
                tracing::debug!(%peer, %error, "Rust SRT ingest timer failed");
            }
        }
        if !connection::service(connection, socket, events, now) {
            connections.remove(&peer);
        }
    }
    true
}

pub(super) fn poll_wait(
    connections: &HashMap<SocketAddr, RustConnection>,
    now: Timestamp,
) -> Duration {
    connections
        .values()
        .flat_map(|connection| connection.timers.values())
        .map(|deadline| Duration::from_micros(deadline.as_micros().saturating_sub(now.as_micros())))
        .min()
        .unwrap_or(POLL_MAX_WAIT)
        .min(POLL_MAX_WAIT)
}

fn send_event(events: &Sender<IngestEvent>, event: IngestEvent) -> bool {
    events.blocking_send(event).is_ok()
}

fn timestamp(start: Instant) -> Timestamp {
    Timestamp::from_micros(start.elapsed().as_micros() as u64)
}
