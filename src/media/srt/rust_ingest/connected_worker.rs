use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token};
use shiguredo_srt::Timestamp;
use tokio::sync::mpsc::{Receiver, Sender, error::TryRecvError};

use super::connected_group::{ConnectedGroup, GroupKey};
use super::connection::{self, RustConnection};
use super::socket;
use super::types::{ConnectionId, IngestEvent, WorkerCommand};

const MAX_WAIT: Duration = Duration::from_millis(20);

struct ConnectedPeer {
    socket: UdpSocket,
    id: ConnectionId,
    peer: std::net::SocketAddr,
    route: PeerRoute,
}

enum PeerRoute {
    Pending(RustConnection),
    Single(RustConnection),
    Group { key: GroupKey, member_id: u32 },
    Closed,
}

#[derive(Clone)]
enum RouteKind {
    Pending,
    Single,
    Group(GroupKey, u32),
}

struct WorkerState<'a> {
    poll: &'a mut Poll,
    peers: &'a mut Vec<Option<ConnectedPeer>>,
    indexes: &'a mut HashMap<std::net::SocketAddr, usize>,
    groups: &'a mut HashMap<GroupKey, ConnectedGroup>,
    port: u16,
    udp_buffer: usize,
    events: &'a Sender<IngestEvent>,
    release_sender: &'a std::sync::mpsc::Sender<std::net::SocketAddr>,
    now: Timestamp,
}

pub(super) fn spawn(
    worker_index: usize,
    port: u16,
    udp_buffer: usize,
    stop: Arc<AtomicBool>,
    commands: Receiver<WorkerCommand>,
    events: Sender<IngestEvent>,
    release_sender: std::sync::mpsc::Sender<std::net::SocketAddr>,
) -> io::Result<JoinHandle<()>> {
    std::thread::Builder::new()
        .name(format!("restream-srt-rust-connected-{worker_index}"))
        .spawn(move || {
            run(
                worker_index,
                port,
                udp_buffer,
                stop,
                commands,
                events,
                release_sender,
            )
        })
}

fn run(
    worker_index: usize,
    port: u16,
    udp_buffer: usize,
    stop: Arc<AtomicBool>,
    mut commands: Receiver<WorkerCommand>,
    events: Sender<IngestEvent>,
    release_sender: std::sync::mpsc::Sender<std::net::SocketAddr>,
) {
    let mut poll = match Poll::new() {
        Ok(poll) => poll,
        Err(error) => {
            tracing::error!(worker = worker_index, %error, "connected Rust ingest poll creation failed");
            return;
        }
    };
    let mut peers = Vec::<Option<ConnectedPeer>>::new();
    let mut indexes = HashMap::<std::net::SocketAddr, usize>::new();
    let mut groups = HashMap::<GroupKey, ConnectedGroup>::new();
    let mut poll_events = Events::with_capacity(1024);
    let mut packet = vec![0u8; 64 * 1024];
    let start = Instant::now();

    while !stop.load(Ordering::Acquire) {
        let mut state = WorkerState {
            poll: &mut poll,
            peers: &mut peers,
            indexes: &mut indexes,
            groups: &mut groups,
            port,
            udp_buffer,
            events: &events,
            release_sender: &release_sender,
            now: timestamp(start),
        };
        if process_commands(&mut commands, &mut state) {
            break;
        }

        let wait = poll_wait(&peers, timestamp(start));
        if let Err(error) = poll.poll(&mut poll_events, Some(wait))
            && error.kind() != io::ErrorKind::Interrupted
        {
            tracing::error!(worker = worker_index, %error, "connected Rust ingest poll failed");
            break;
        }
        for event in &poll_events {
            let Some(index) = event.token().0.checked_sub(1) else {
                continue;
            };
            loop {
                let size = match peers
                    .get_mut(index)
                    .and_then(Option::as_mut)
                    .map(|peer| peer.socket.recv(&mut packet))
                {
                    Some(Ok(size)) => size,
                    Some(Err(error)) if error.kind() == io::ErrorKind::WouldBlock => break,
                    Some(Err(error)) => {
                        tracing::debug!(%error, "connected Rust ingest receive failed");
                        break;
                    }
                    None => break,
                };
                let mut state = WorkerState {
                    poll: &mut poll,
                    peers: &mut peers,
                    indexes: &mut indexes,
                    groups: &mut groups,
                    port,
                    udp_buffer,
                    events: &events,
                    release_sender: &release_sender,
                    now: timestamp(start),
                };
                if !service_packet(&mut state, index, &packet[..size]) {
                    cleanup_route(&mut state, index);
                    break;
                }
            }
        }
        let mut state = WorkerState {
            poll: &mut poll,
            peers: &mut peers,
            indexes: &mut indexes,
            groups: &mut groups,
            port,
            udp_buffer,
            events: &events,
            release_sender: &release_sender,
            now: timestamp(start),
        };
        service_timers(&mut state);
    }
}

fn process_commands(commands: &mut Receiver<WorkerCommand>, state: &mut WorkerState<'_>) -> bool {
    loop {
        match commands.try_recv() {
            Ok(WorkerCommand::Stop) | Err(TryRecvError::Disconnected) => return true,
            Err(TryRecvError::Empty) => return false,
            Ok(WorkerCommand::Handoff { connection }) => {
                admit_peer(state, *connection);
            }
            Ok(WorkerCommand::ForwardPacket { peer, packet }) => {
                if let Some(index) = state.indexes.get(&peer).copied()
                    && !service_packet(state, index, &packet)
                {
                    cleanup_route(state, index);
                }
            }
            Ok(WorkerCommand::Authorize {
                id,
                logical_id,
                accepted,
            }) => {
                let Some(index) = state
                    .peers
                    .iter()
                    .position(|peer| peer.as_ref().is_some_and(|peer| peer.id == id))
                else {
                    continue;
                };
                if !accepted {
                    let peer = state.peers[index].as_ref().map(|peer| peer.peer);
                    if let Some(peer) = peer {
                        let _ = send_event(
                            state.events,
                            IngestEvent::Disconnected {
                                id,
                                phase: "authorize",
                                reason: "ingest authorization rejected".to_string(),
                                had_error: true,
                            },
                        );
                        remove_peer(state, peer);
                    }
                    continue;
                }
                authorize_peer(state, index, logical_id);
            }
        }
    }
}

fn admit_peer(state: &mut WorkerState<'_>, connection: RustConnection) {
    let peer = connection.peer;
    let connection_id = connection.id;
    let Ok(std_socket) = socket::connect_reuseport(state.port, peer, state.udp_buffer) else {
        handoff_failure(
            state,
            connection_id,
            peer,
            "connected Rust ingest socket creation failed",
        );
        return;
    };
    let mut socket = UdpSocket::from_std(std_socket);
    let index = state
        .peers
        .iter()
        .position(Option::is_none)
        .unwrap_or(state.peers.len());
    if state
        .poll
        .registry()
        .register(&mut socket, Token(index + 1), Interest::READABLE)
        .is_err()
    {
        handoff_failure(
            state,
            connection_id,
            peer,
            "connected Rust ingest socket registration failed",
        );
        return;
    }
    let connected = ConnectedPeer {
        socket,
        id: connection.id,
        peer,
        route: PeerRoute::Pending(connection),
    };
    if index == state.peers.len() {
        state.peers.push(Some(connected));
    } else {
        state.peers[index] = Some(connected);
    }
    state.indexes.insert(peer, index);
    let service_ok = state
        .peers
        .get_mut(index)
        .and_then(Option::as_mut)
        .and_then(|peer| match &mut peer.route {
            PeerRoute::Pending(connection) => Some(connection::service(
                connection,
                &peer.socket,
                state.events,
                state.now,
            )),
            _ => None,
        })
        .unwrap_or(false);
    if !service_ok {
        cleanup_route(state, index);
    }
}

fn handoff_failure(
    state: &mut WorkerState<'_>,
    id: ConnectionId,
    peer: std::net::SocketAddr,
    reason: &'static str,
) {
    let _ = send_event(
        state.events,
        IngestEvent::Disconnected {
            id,
            phase: "handoff",
            reason: reason.to_string(),
            had_error: true,
        },
    );
    let _ = state.release_sender.send(peer);
}

fn route_kind(peer: &ConnectedPeer) -> RouteKind {
    match &peer.route {
        PeerRoute::Pending(_) => RouteKind::Pending,
        PeerRoute::Single(_) => RouteKind::Single,
        PeerRoute::Group { key, member_id } => RouteKind::Group(key.clone(), *member_id),
        PeerRoute::Closed => RouteKind::Pending,
    }
}

fn service_packet(state: &mut WorkerState<'_>, index: usize, packet: &[u8]) -> bool {
    let Some(kind) = state
        .peers
        .get(index)
        .and_then(Option::as_ref)
        .map(route_kind)
    else {
        return false;
    };
    match kind {
        RouteKind::Pending => {
            let Some(Some(peer)) = state.peers.get_mut(index) else {
                return false;
            };
            let PeerRoute::Pending(connection) = &mut peer.route else {
                return false;
            };
            if connection.core.feed_recv_buf(packet, state.now).is_err() {
                return false;
            }
            connection::drain_core_outputs(
                &mut connection.core,
                &peer.socket,
                peer.peer,
                &mut connection.timers,
                state.now,
            )
            .is_ok()
        }
        RouteKind::Single => {
            let Some(Some(peer)) = state.peers.get_mut(index) else {
                return false;
            };
            let PeerRoute::Single(connection) = &mut peer.route else {
                return false;
            };
            if connection.core.feed_recv_buf(packet, state.now).is_err() {
                return false;
            }
            connection::service(connection, &peer.socket, state.events, state.now)
        }
        RouteKind::Group(key, member_id) => {
            let Some(Some(peer)) = state.peers.get(index) else {
                return false;
            };
            let result = state.groups.get_mut(&key).is_some_and(|group| {
                group.service_member(
                    member_id,
                    &peer.socket,
                    Some(packet),
                    state.events,
                    state.now,
                )
            });
            if !result && let Some(group) = state.groups.get_mut(&key) {
                group.core.mark_member_broken(member_id);
            }
            result
        }
    }
}

fn authorize_peer(state: &mut WorkerState<'_>, index: usize, logical_id: ConnectionId) {
    let Some(kind) = state
        .peers
        .get(index)
        .and_then(Option::as_ref)
        .map(route_kind)
    else {
        return;
    };
    if !matches!(kind, RouteKind::Pending) {
        return;
    }
    let Some(Some(peer)) = state.peers.get_mut(index) else {
        return;
    };
    let route = std::mem::replace(&mut peer.route, PeerRoute::Closed);
    let PeerRoute::Pending(mut connection) = route else {
        return;
    };
    if let Some(extension) = connection.core.peer_group_extension() {
        let key = GroupKey {
            group_id: extension.group_id,
            stream_id: connection
                .core
                .peer_stream_id()
                .map(|stream_id| stream_id.trim_matches('\0').trim().to_string()),
        };
        let group = match state.groups.entry(key.clone()) {
            std::collections::hash_map::Entry::Occupied(entry) => {
                let group = entry.into_mut();
                if !group.accepts(extension) {
                    let _ = send_event(
                        state.events,
                        IngestEvent::Disconnected {
                            id: connection.id,
                            phase: "group",
                            reason: "SRT GROUP metadata changed for an existing group".to_string(),
                            had_error: true,
                        },
                    );
                    peer.route = PeerRoute::Closed;
                    let peer_addr = peer.peer;
                    remove_peer(state, peer_addr);
                    return;
                }
                group
            }
            std::collections::hash_map::Entry::Vacant(entry) => {
                let Ok(group) = ConnectedGroup::new(extension, logical_id) else {
                    let _ = send_event(
                        state.events,
                        IngestEvent::Disconnected {
                            id: connection.id,
                            phase: "group",
                            reason: "invalid SRT GROUP metadata".to_string(),
                            had_error: true,
                        },
                    );
                    peer.route = PeerRoute::Closed;
                    let peer_addr = peer.peer;
                    remove_peer(state, peer_addr);
                    return;
                };
                entry.insert(group)
            }
        };
        let Ok(member_id) = group.add_member(connection, index) else {
            let _ = send_event(
                state.events,
                IngestEvent::Disconnected {
                    id: peer.id,
                    phase: "group",
                    reason: "SRT GROUP member admission failed".to_string(),
                    had_error: true,
                },
            );
            peer.route = PeerRoute::Closed;
            let peer_addr = peer.peer;
            remove_peer(state, peer_addr);
            return;
        };
        peer.route = PeerRoute::Group { key, member_id };
        if let RouteKind::Group(key, member_id) = route_kind(peer) {
            let _ = state.groups.get_mut(&key).is_some_and(|group| {
                group.service_member(member_id, &peer.socket, None, state.events, state.now)
            });
        }
        return;
    }
    connection.authorized = true;
    peer.route = PeerRoute::Single(connection);
    if let PeerRoute::Single(connection) = &mut peer.route
        && !connection::service(connection, &peer.socket, state.events, state.now)
    {
        peer.route = PeerRoute::Closed;
    }
}

fn service_timers(state: &mut WorkerState<'_>) {
    let indexes = state
        .peers
        .iter()
        .enumerate()
        .filter_map(|(index, peer)| peer.as_ref().map(|_| index))
        .collect::<Vec<_>>();
    for index in indexes {
        let Some(kind) = state
            .peers
            .get(index)
            .and_then(Option::as_ref)
            .map(route_kind)
        else {
            continue;
        };
        match kind {
            RouteKind::Pending => {
                let Some(Some(peer)) = state.peers.get_mut(index) else {
                    continue;
                };
                let PeerRoute::Pending(connection) = &mut peer.route else {
                    continue;
                };
                let due = connection
                    .timers
                    .iter()
                    .filter_map(|(id, deadline)| (state.now >= *deadline).then_some(*id))
                    .collect::<Vec<_>>();
                for id in due {
                    connection.timers.remove(&id);
                    let _ = connection.core.handle_timer(id, state.now);
                }
                let _ = connection::drain_core_outputs(
                    &mut connection.core,
                    &peer.socket,
                    peer.peer,
                    &mut connection.timers,
                    state.now,
                );
            }
            RouteKind::Single => {
                let Some(Some(peer)) = state.peers.get_mut(index) else {
                    continue;
                };
                let PeerRoute::Single(connection) = &mut peer.route else {
                    continue;
                };
                let due = connection
                    .timers
                    .iter()
                    .filter_map(|(id, deadline)| (state.now >= *deadline).then_some(*id))
                    .collect::<Vec<_>>();
                for id in due {
                    connection.timers.remove(&id);
                    let _ = connection.core.handle_timer(id, state.now);
                }
                if !connection::service(connection, &peer.socket, state.events, state.now) {
                    peer.route = PeerRoute::Closed;
                }
            }
            RouteKind::Group(key, member_id) => {
                let Some(Some(peer)) = state.peers.get(index) else {
                    continue;
                };
                let _ = state.groups.get_mut(&key).is_some_and(|group| {
                    group.service_timer(member_id, &peer.socket, state.events, state.now)
                });
            }
        }
    }
    cleanup_broken_groups(state);
}

fn cleanup_broken_groups(state: &mut WorkerState<'_>) {
    let groups = state.groups.keys().cloned().collect::<Vec<_>>();
    for key in groups {
        let broken = state
            .groups
            .get_mut(&key)
            .map_or_else(Vec::new, ConnectedGroup::broken_members);
        for member_id in broken {
            remove_group_member(state, &key, member_id);
        }
    }
    state.groups.retain(|_, group| !group.members.is_empty());
}

fn cleanup_route(state: &mut WorkerState<'_>, index: usize) {
    let Some(kind) = state
        .peers
        .get(index)
        .and_then(Option::as_ref)
        .map(route_kind)
    else {
        return;
    };
    match kind {
        RouteKind::Group(key, member_id) => {
            if let Some(group) = state.groups.get_mut(&key) {
                group.core.mark_member_broken(member_id);
            }
            remove_group_member(state, &key, member_id);
        }
        RouteKind::Pending | RouteKind::Single => {
            let peer = state.peers[index].as_ref().map(|peer| peer.peer);
            if let Some(peer) = peer {
                remove_peer(state, peer);
            }
        }
    }
}

fn remove_group_member(state: &mut WorkerState<'_>, key: &GroupKey, member_id: u32) {
    let Some(group) = state.groups.get_mut(key) else {
        return;
    };
    let Some(member) = group.remove_member(member_id) else {
        return;
    };
    let index = member.socket_index;
    if let Some(mut peer) = state.peers.get_mut(index).and_then(Option::take) {
        state.indexes.remove(&peer.peer);
        let _ = state.poll.registry().deregister(&mut peer.socket);
        let _ = state.release_sender.send(peer.peer);
    }
    let _ = send_event(
        state.events,
        IngestEvent::Disconnected {
            id: member.physical_id,
            phase: "group",
            reason: "SRT GROUP member disconnected".to_string(),
            had_error: false,
        },
    );
    if group.members.is_empty() {
        state.groups.remove(key);
    }
}

fn remove_peer(state: &mut WorkerState<'_>, peer: std::net::SocketAddr) {
    let Some(index) = state.indexes.remove(&peer) else {
        return;
    };
    if let Some(mut peer) = state.peers.get_mut(index).and_then(Option::take) {
        let _ = state.poll.registry().deregister(&mut peer.socket);
        let _ = state.release_sender.send(peer.peer);
    }
}

fn poll_wait(_peers: &[Option<ConnectedPeer>], _now: Timestamp) -> Duration {
    MAX_WAIT
}

fn send_event(events: &Sender<IngestEvent>, event: IngestEvent) -> bool {
    events.blocking_send(event).is_ok()
}

fn timestamp(start: Instant) -> Timestamp {
    Timestamp::from_micros(start.elapsed().as_micros() as u64)
}
