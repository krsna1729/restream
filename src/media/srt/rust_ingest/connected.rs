use std::collections::HashMap;
use std::io;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Receiver;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mio::net::UdpSocket;
use mio::{Events, Interest, Poll, Token};
use shiguredo_srt::{ConnectionState, GroupExtensionData, SRTGROUP_MASK};
use tokio::sync::mpsc::{self, Sender};

use super::connected_worker;
use super::connection::{self, RustConnection};
use super::routing::{GroupAffinity, RoutingMode, WorkerRouter, handshake_route};
use super::socket;
use super::types::{ConnectionId, IngestEvent, WorkerCommand};
use super::worker::WorkerOptions;

const COMMAND_CHANNEL_CAPACITY: usize = 1024;
const LISTENER_POLL_WAIT: Duration = Duration::from_millis(20);

type CommandSenders = Vec<Sender<WorkerCommand>>;
type ThreadHandles = Vec<JoinHandle<()>>;

struct ListenerContext {
    port: u16,
    udp_buffer: usize,
    workers: usize,
    commands: CommandSenders,
    releases: Receiver<SocketAddr>,
    stop: Arc<AtomicBool>,
    options: WorkerOptions,
}

pub(super) fn start(
    port: u16,
    workers: usize,
    udp_buffer: usize,
    options: WorkerOptions,
    events: Sender<IngestEvent>,
    stop: Arc<AtomicBool>,
) -> Result<(CommandSenders, ThreadHandles), String> {
    let (release_sender, release_receiver) = std::sync::mpsc::channel();
    let listener_options = options.clone();
    let mut commands = Vec::with_capacity(workers);
    let mut handles = Vec::with_capacity(workers + 1);
    for worker_index in 0..workers {
        let (command_sender, command_receiver) = mpsc::channel(COMMAND_CHANNEL_CAPACITY);
        let handle = connected_worker::spawn(
            worker_index,
            port,
            udp_buffer,
            stop.clone(),
            command_receiver,
            events.clone(),
            release_sender.clone(),
        )
        .map_err(|error| format!("spawn connected Rust ingest worker {worker_index}: {error}"))?;
        commands.push(command_sender);
        handles.push(handle);
    }

    let listener_commands = commands.clone();
    let listener = std::thread::Builder::new()
        .name("restream-srt-rust-ingest-listener".to_string())
        .spawn(move || {
            run_listener(ListenerContext {
                port,
                udp_buffer,
                workers,
                commands: listener_commands,
                releases: release_receiver,
                stop,
                options: listener_options,
            });
        })
        .map_err(|error| format!("spawn connected Rust ingest listener: {error}"))?;
    handles.push(listener);
    Ok((commands, handles))
}

fn run_listener(context: ListenerContext) {
    let ListenerContext {
        port,
        udp_buffer,
        workers,
        commands,
        releases,
        stop,
        options,
    } = context;
    let std_socket = match socket::bind_reuseport(port, udp_buffer) {
        Ok(socket) => socket,
        Err(error) => {
            tracing::error!(%error, "connected Rust ingest listener bind failed");
            stop.store(true, Ordering::Release);
            return;
        }
    };
    let mut socket = UdpSocket::from_std(std_socket);
    let mut poll = match Poll::new() {
        Ok(poll) => poll,
        Err(error) => {
            tracing::error!(%error, "connected Rust ingest listener poll creation failed");
            return;
        }
    };
    if let Err(error) = poll
        .registry()
        .register(&mut socket, Token(0), Interest::READABLE)
    {
        tracing::error!(%error, "connected Rust ingest listener registration failed");
        return;
    }

    let routing_mode = if crate::config::rust_srt_ingest_round_robin() {
        RoutingMode::RoundRobin
    } else {
        RoutingMode::LeastTuples
    };
    let start = Instant::now();
    let mut poll_events = Events::with_capacity(1);
    let mut packet = vec![0u8; 64 * 1024];
    let mut pending = HashMap::<SocketAddr, RustConnection>::new();
    let mut routes = HashMap::<SocketAddr, usize>::new();
    let mut router = WorkerRouter::new(workers);
    let mut next_serial = u64::from(std::process::id()) << 32;
    let mut local_groups = HashMap::<u32, GroupExtensionData>::new();
    let mut next_group_id = (std::process::id() & 0x3FFF_FFFF).max(1);

    while !stop.load(Ordering::Acquire) {
        while let Ok(peer) = releases.try_recv() {
            routes.remove(&peer);
            release_route(&mut router, &mut local_groups, peer);
        }
        if let Err(error) = poll.poll(&mut poll_events, Some(LISTENER_POLL_WAIT))
            && error.kind() != io::ErrorKind::Interrupted
        {
            tracing::error!(%error, "connected Rust ingest listener poll failed");
            break;
        }
        for event in &poll_events {
            if event.token() != Token(0) {
                continue;
            }
            let mut state = ListenerState {
                socket: &mut socket,
                pending: &mut pending,
                routes: &mut routes,
                router: &mut router,
                routing_mode,
                commands: &commands,
                next_serial: &mut next_serial,
                options: &options,
                local_groups: &mut local_groups,
                next_group_id: &mut next_group_id,
                start,
            };
            receive_packets(&mut state, &mut packet);
        }
    }
}

struct ListenerState<'a> {
    socket: &'a mut UdpSocket,
    pending: &'a mut HashMap<SocketAddr, RustConnection>,
    routes: &'a mut HashMap<SocketAddr, usize>,
    router: &'a mut WorkerRouter,
    routing_mode: RoutingMode,
    commands: &'a [Sender<WorkerCommand>],
    next_serial: &'a mut u64,
    options: &'a WorkerOptions,
    local_groups: &'a mut HashMap<u32, GroupExtensionData>,
    next_group_id: &'a mut u32,
    start: Instant,
}

fn receive_packets(state: &mut ListenerState<'_>, packet: &mut [u8]) {
    loop {
        let (size, peer) = match state.socket.recv_from(packet) {
            Ok(received) => received,
            Err(error) if error.kind() == io::ErrorKind::WouldBlock => return,
            Err(error) => {
                tracing::debug!(%error, "connected Rust ingest listener receive failed");
                return;
            }
        };
        if let Some(worker) = state.routes.get(&peer).copied() {
            if state.commands[worker]
                .blocking_send(WorkerCommand::ForwardPacket {
                    peer,
                    packet: packet[..size].to_vec(),
                })
                .is_err()
            {
                state.routes.remove(&peer);
                release_route(state.router, state.local_groups, peer);
            }
            continue;
        }

        if !state.pending.contains_key(&peer) {
            let id = ConnectionId {
                worker: usize::MAX,
                serial: *state.next_serial,
            };
            *state.next_serial = state.next_serial.wrapping_add(1).max(1);
            state
                .pending
                .insert(peer, connection::new(id, peer, state.options));
        }
        let (is_conclusion, group) = handshake_route(&packet[..size]).unwrap_or((false, None));
        let local_group = group
            .as_ref()
            .map(|affinity| local_group_extension(state, affinity));
        let connection = state
            .pending
            .get_mut(&peer)
            .expect("pending connection inserted above");
        if let Some(local_group) = local_group {
            connection.core.set_group_extension(local_group);
        }
        if (is_conclusion || group.is_some()) && connection.id.worker == usize::MAX {
            connection.id.worker = state.router.assign(peer, group.clone(), state.routing_mode);
        }
        let now = timestamp(state.start);
        if connection.core.feed_recv_buf(&packet[..size], now).is_err() {
            state.pending.remove(&peer);
            release_route(state.router, state.local_groups, peer);
            continue;
        }
        if connection::drain_core_outputs(
            &mut connection.core,
            state.socket,
            connection.peer,
            &mut connection.timers,
            now,
        )
        .is_err()
        {
            state.pending.remove(&peer);
            release_route(state.router, state.local_groups, peer);
            continue;
        }
        if connection.core.state() != ConnectionState::Connected {
            continue;
        }
        let Some(connection) = state.pending.remove(&peer) else {
            continue;
        };
        let worker = if connection.id.worker == usize::MAX {
            let group = connection
                .core
                .peer_group_extension()
                .map(|extension| GroupAffinity {
                    group_id: extension.group_id,
                    stream_id: connection.core.peer_stream_id().map(str::to_owned),
                    extension,
                });
            state.router.assign(peer, group, state.routing_mode)
        } else {
            connection.id.worker
        };
        state.routes.insert(peer, worker);
        if state.commands[worker]
            .blocking_send(WorkerCommand::Handoff {
                connection: Box::new(connection),
            })
            .is_err()
        {
            state.routes.remove(&peer);
            release_route(state.router, state.local_groups, peer);
        }
    }
}

fn release_route(
    router: &mut WorkerRouter,
    local_groups: &mut HashMap<u32, GroupExtensionData>,
    peer: SocketAddr,
) {
    if let Some(group_id) = router.release(peer) {
        local_groups.remove(&group_id);
    }
}

fn timestamp(start: Instant) -> shiguredo_srt::Timestamp {
    shiguredo_srt::Timestamp::from_micros(start.elapsed().as_micros() as u64)
}

fn local_group_extension(
    state: &mut ListenerState<'_>,
    affinity: &GroupAffinity,
) -> GroupExtensionData {
    if let Some(extension) = state.local_groups.get(&affinity.group_id).copied() {
        return extension;
    }
    let group_id = SRTGROUP_MASK | (*state.next_group_id & 0x3FFF_FFFF).max(1);
    *state.next_group_id = state.next_group_id.wrapping_add(1).max(1);
    let extension = GroupExtensionData {
        group_id,
        group_type: affinity.extension.group_type,
        flags: affinity.extension.flags,
        weight: 0,
    };
    state.local_groups.insert(affinity.group_id, extension);
    extension
}
