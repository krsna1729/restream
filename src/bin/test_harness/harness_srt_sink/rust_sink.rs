use std::collections::{HashMap, HashSet};
use std::net::{SocketAddr, UdpSocket as StdUdpSocket};
use std::os::fd::{AsRawFd, FromRawFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mio::net::UdpSocket as MioUdpSocket;
use mio::{Events, Interest, Poll, Token};
use shiguredo_srt::{
    ConnectionEvent, ConnectionOptions, ConnectionOutput, KeyLength, SrtConnection, TimerId,
    Timestamp,
};

use super::{HarnessSrtCrypto, RustConnectedRouting, RustSinkScaling, SockaddrIn, c_int, c_void};

const RUST_SINK_FLOW_WINDOW_PACKETS: u32 = 32_768;
const RUST_SINK_RECEIVE_BUFFER_PACKETS: u32 = (12 * 1024 * 1024 / 1472) as u32;

pub(crate) struct RustHarnessSrtSinkPool {
    stop: Arc<AtomicBool>,
    pub(super) threads: Vec<JoinHandle<()>>,
}

#[derive(Clone)]
struct RustSinkCrypto {
    passphrase: Option<String>,
    key_length: KeyLength,
}

impl RustHarnessSrtSinkPool {
    pub(crate) fn start(
        ports: &[u16],
        _udp_buffer: i32,
        thread_count: usize,
        scaling: RustSinkScaling,
        crypto: &HarnessSrtCrypto,
    ) -> Result<Self, String> {
        if ports.is_empty() {
            return Err("Rust harness SRT sink pool needs at least one port".to_string());
        }

        let key_length = match crypto.pbkeylen.as_deref() {
            None | Some("16") => KeyLength::Aes128,
            Some("24") => KeyLength::Aes192,
            Some("32") => KeyLength::Aes256,
            Some(other) => {
                return Err(format!(
                    "Rust harness SRT sink supports pbkeylen 16, 24, or 32 (got {other})"
                ));
            }
        };

        let stop = Arc::new(AtomicBool::new(false));
        let sink_crypto = RustSinkCrypto {
            passphrase: crypto.passphrase.clone(),
            key_length,
        };
        if scaling == RustSinkScaling::Connected {
            let routing = RustConnectedRouting::from_env()?;
            return connected::start(ports, thread_count, _udp_buffer, routing, stop, sink_crypto);
        }

        let (worker_sockets, worker_count) = match scaling {
            RustSinkScaling::Ports | RustSinkScaling::PerStreamPort => {
                let sockets = bind_distinct_sockets(ports, _udp_buffer)?;
                let worker_count = thread_count.clamp(1, sockets.len());
                let mut worker_sockets: Vec<Vec<StdUdpSocket>> =
                    (0..worker_count).map(|_| Vec::new()).collect();
                for (index, socket) in sockets.into_iter().enumerate() {
                    worker_sockets[index % worker_count].push(socket);
                }
                (worker_sockets, worker_count)
            }
            RustSinkScaling::ReusePort => {
                if ports.len() != 1 {
                    return Err(format!(
                        "Rust reuseport sink needs exactly one public port (got {})",
                        ports.len()
                    ));
                }
                let worker_count = thread_count.max(1);
                let mut worker_sockets = Vec::with_capacity(worker_count);
                for _ in 0..worker_count {
                    worker_sockets.push(vec![bind_reuseport_socket(ports[0], _udp_buffer)?]);
                }
                (worker_sockets, worker_count)
            }
            RustSinkScaling::Connected => unreachable!(),
        };
        let threads = spawn_rust_sink_workers(worker_sockets, stop.clone(), sink_crypto)?;

        tracing::info!(
            scaling = ?scaling,
            "[harness-srt-sink] listening with Rust Core on {} port(s) across {} worker(s)",
            ports.len(),
            worker_count
        );
        Ok(Self { stop, threads })
    }

    pub(crate) fn stop(self) {
        self.stop.store(true, Ordering::Relaxed);
        for thread in self.threads {
            let _ = thread.join();
        }
        tracing::info!("[harness-srt-sink] stopped Rust Core pool");
    }
}

fn bind_distinct_sockets(ports: &[u16], udp_buffer: i32) -> Result<Vec<StdUdpSocket>, String> {
    let mut sockets = Vec::with_capacity(ports.len());
    for &port in ports {
        let socket = StdUdpSocket::bind(("0.0.0.0", port))
            .map_err(|error| format!("bind Rust harness SRT sink on {port}: {error}"))?;
        set_udp_socket_buffers(&socket, udp_buffer, port)?;
        socket
            .set_nonblocking(true)
            .map_err(|error| format!("set Rust harness SRT sink nonblocking on {port}: {error}"))?;
        sockets.push(socket);
    }
    Ok(sockets)
}

fn bind_reuseport_socket(port: u16, udp_buffer: i32) -> Result<StdUdpSocket, String> {
    let fd = unsafe {
        libc::socket(
            libc::AF_INET,
            libc::SOCK_DGRAM | libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK,
            0,
        )
    };
    if fd < 0 {
        return Err(format!(
            "create Rust reuseport sink socket on {port}: {}",
            std::io::Error::last_os_error()
        ));
    }

    let option: c_int = 1;
    let reuse_result = unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEPORT,
            &option as *const c_int as *const c_void,
            std::mem::size_of_val(&option) as libc::socklen_t,
        )
    };
    if reuse_result != 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(format!("enable SO_REUSEPORT on {port}: {error}"));
    }

    if let Err(error) = set_udp_fd_buffers(fd, udp_buffer, port) {
        unsafe {
            libc::close(fd);
        }
        return Err(error);
    }

    let address = SockaddrIn {
        sin_family: libc::AF_INET as u16,
        sin_port: port.to_be(),
        sin_addr: 0,
        sin_zero: [0; 8],
    };
    let bind_result = unsafe {
        libc::bind(
            fd,
            &address as *const SockaddrIn as *const libc::sockaddr,
            std::mem::size_of::<SockaddrIn>() as libc::socklen_t,
        )
    };
    if bind_result != 0 {
        let error = std::io::Error::last_os_error();
        unsafe {
            libc::close(fd);
        }
        return Err(format!("bind Rust reuseport sink on {port}: {error}"));
    }

    Ok(unsafe { StdUdpSocket::from_raw_fd(fd) })
}

fn set_udp_socket_buffers(socket: &StdUdpSocket, udp_buffer: i32, port: u16) -> Result<(), String> {
    set_udp_fd_buffers(socket.as_raw_fd(), udp_buffer, port)
}

fn set_udp_fd_buffers(fd: c_int, udp_buffer: i32, port: u16) -> Result<(), String> {
    if udp_buffer <= 0 {
        return Ok(());
    }

    for (option, name) in [
        (libc::SO_RCVBUF, "SO_RCVBUF"),
        (libc::SO_SNDBUF, "SO_SNDBUF"),
    ] {
        let result = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                option,
                &udp_buffer as *const i32 as *const c_void,
                std::mem::size_of_val(&udp_buffer) as libc::socklen_t,
            )
        };
        if result != 0 {
            return Err(format!(
                "set Rust harness SRT sink {name}={udp_buffer} on {port}: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    Ok(())
}

fn spawn_rust_sink_workers(
    worker_sockets: Vec<Vec<StdUdpSocket>>,
    stop: Arc<AtomicBool>,
    crypto: RustSinkCrypto,
) -> Result<Vec<JoinHandle<()>>, String> {
    let mut threads: Vec<JoinHandle<()>> = Vec::with_capacity(worker_sockets.len());
    for (worker_index, sockets) in worker_sockets.into_iter().enumerate() {
        let thread_stop = stop.clone();
        let sink_crypto = crypto.clone();
        let thread = match std::thread::Builder::new()
            .name(format!("harness-srt-rust-sink-{worker_index}"))
            .spawn(move || {
                run_rust_sink_pool(sockets, thread_stop, sink_crypto);
            }) {
            Ok(thread) => thread,
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                for thread in threads {
                    let _ = thread.join();
                }
                return Err(format!(
                    "spawn Rust harness SRT sink worker {worker_index}: {error}"
                ));
            }
        };
        threads.push(thread);
    }
    Ok(threads)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub(super) struct RustSinkConnectionKey {
    peer: SocketAddr,
    socket_id: u32,
}

struct RustSinkConnection {
    conn: SrtConnection,
    timers: HashMap<TimerId, Timestamp>,
}

type RustSinkConnections = HashMap<RustSinkConnectionKey, RustSinkConnection>;
type RustSinkRouteMap = HashMap<RustSinkConnectionKey, RustSinkConnectionKey>;

pub(super) fn rust_sink_connection_key(peer: SocketAddr, packet: &[u8]) -> RustSinkConnectionKey {
    let mut socket_id = packet_u32(packet, 12).unwrap_or(0);
    let is_handshake = packet_u32(packet, 0).is_some_and(|first_word| {
        first_word & 0x8000_0000 != 0 && ((first_word >> 16) & 0x7FFF) == 0
    });
    if socket_id == 0 && is_handshake {
        // Handshake control info starts after the 16-byte SRT header. The
        // socket ID is the eighth 32-bit field in that block.
        socket_id = packet_u32(packet, 40).unwrap_or(0);
    }
    RustSinkConnectionKey { peer, socket_id }
}

fn packet_u32(packet: &[u8], start: usize) -> Option<u32> {
    let bytes = packet.get(start..start + 4)?;
    Some(u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn run_rust_sink_pool(sockets: Vec<StdUdpSocket>, stop: Arc<AtomicBool>, crypto: RustSinkCrypto) {
    let mut sockets = sockets
        .into_iter()
        .map(MioUdpSocket::from_std)
        .collect::<Vec<_>>();
    let mut poll = match Poll::new() {
        Ok(poll) => poll,
        Err(error) => {
            tracing::error!(%error, "failed to create Rust harness SRT sink poller");
            return;
        }
    };
    for (index, socket) in sockets.iter_mut().enumerate() {
        if let Err(error) = poll
            .registry()
            .register(socket, Token(index), Interest::READABLE)
        {
            tracing::error!(%error, index, "failed to register Rust harness SRT sink socket");
            return;
        }
    }

    let mut slots: Vec<RustSinkConnections> = std::iter::repeat_with(HashMap::new)
        .take(sockets.len())
        .collect();
    let mut routes: Vec<RustSinkRouteMap> = std::iter::repeat_with(HashMap::new)
        .take(sockets.len())
        .collect();
    let mut groups: Vec<group::RustSinkGroups> = std::iter::repeat_with(HashMap::new)
        .take(sockets.len())
        .collect();
    let mut group_routes: Vec<group::RustSinkGroupRoutes> = std::iter::repeat_with(HashMap::new)
        .take(sockets.len())
        .collect();
    let mut events = Events::with_capacity(sockets.len().max(1));
    let mut packet = [0u8; 64 * 1024];
    let start = Instant::now();
    let mut next_socket_id = std::process::id().wrapping_add(1);
    let mut next_group_id = std::process::id().wrapping_add(1);
    while !stop.load(Ordering::Relaxed) {
        let now = timestamp(start);
        let wait = rust_sink_poll_wait(&slots, &groups, now);
        if let Err(error) = poll.poll(&mut events, Some(wait))
            && error.kind() != std::io::ErrorKind::Interrupted
        {
            tracing::error!(%error, "Rust harness SRT sink poll failed");
            break;
        }

        for event in &events {
            let index = event.token().0;
            if index >= sockets.len() {
                continue;
            }
            let mut state = RustSinkGroupPoolState {
                connections: &mut slots[index],
                routes: &mut routes[index],
                groups: &mut groups[index],
                group_routes: &mut group_routes[index],
                crypto: &crypto,
                start,
                next_socket_id: &mut next_socket_id,
                next_group_id: &mut next_group_id,
            };
            receive_rust_packets(&mut sockets[index], &mut state, &mut packet);
        }

        let now = timestamp(start);
        for index in 0..slots.len() {
            process_rust_connections(&sockets[index], &mut slots[index], &mut routes[index], now);
            group::process(
                &mut groups[index],
                &mut group_routes[index],
                &sockets[index],
                now,
            );
        }
    }
}

fn receive_rust_packets(
    socket: &mut MioUdpSocket,
    state: &mut RustSinkGroupPoolState<'_>,
    packet: &mut [u8],
) {
    loop {
        let (size, peer) = match socket.recv_from(packet) {
            Ok(received) => received,
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return,
            Err(error) => {
                tracing::debug!(%error, "Rust harness SRT sink receive failed");
                return;
            }
        };

        group::receive(
            peer,
            &packet[..size],
            state,
            RustSinkOutput::Datagram { socket, peer },
        );
    }
}

#[derive(Clone, Copy)]
enum RustSinkOutput<'a> {
    Datagram {
        socket: &'a MioUdpSocket,
        peer: SocketAddr,
    },
    Connected {
        socket: &'a MioUdpSocket,
    },
}

struct RustSinkGroupPoolState<'a> {
    connections: &'a mut RustSinkConnections,
    routes: &'a mut RustSinkRouteMap,
    groups: &'a mut group::RustSinkGroups,
    group_routes: &'a mut group::RustSinkGroupRoutes,
    crypto: &'a RustSinkCrypto,
    start: Instant,
    next_socket_id: &'a mut u32,
    next_group_id: &'a mut u32,
}

fn new_rust_sink_connection(
    crypto: &RustSinkCrypto,
    socket_id: u32,
    group_extension: Option<shiguredo_srt::GroupExtensionData>,
) -> RustSinkConnection {
    RustSinkConnection {
        conn: SrtConnection::new_listener(ConnectionOptions {
            socket_id,
            passphrase: crypto.passphrase.clone(),
            key_length: crypto.key_length,
            tsbpd_delay: 250,
            group_extension,
            flow_window_packets: RUST_SINK_FLOW_WINDOW_PACKETS,
            receive_buffer_packets: RUST_SINK_RECEIVE_BUFFER_PACKETS,
            ..ConnectionOptions::default()
        }),
        timers: HashMap::new(),
    }
}

fn timestamp(start: Instant) -> Timestamp {
    Timestamp::from_micros(start.elapsed().as_micros() as u64)
}

fn rust_sink_poll_wait(
    slots: &[RustSinkConnections],
    groups: &[group::RustSinkGroups],
    now: Timestamp,
) -> Duration {
    let connection_micros = slots
        .iter()
        .flat_map(|connections| connections.values())
        .flat_map(|connection| connection.timers.values())
        .map(|deadline| deadline.as_micros().saturating_sub(now.as_micros()))
        .min()
        .unwrap_or(20_000)
        .clamp(1, 20_000);
    let group_micros = groups
        .iter()
        .map(|groups| group::poll_wait(groups, now).as_micros() as u64)
        .min()
        .unwrap_or(20_000);
    Duration::from_micros(connection_micros.min(group_micros).clamp(1, 20_000))
}

fn process_rust_connections(
    socket: &MioUdpSocket,
    connections: &mut RustSinkConnections,
    routes: &mut RustSinkRouteMap,
    now: Timestamp,
) {
    process_rust_connections_mode(socket, connections, routes, now, false);
}

fn process_rust_connections_mode(
    socket: &MioUdpSocket,
    connections: &mut RustSinkConnections,
    routes: &mut RustSinkRouteMap,
    now: Timestamp,
    connected: bool,
) {
    let peers: Vec<RustSinkConnectionKey> = connections.keys().copied().collect();
    let mut disconnected = Vec::new();
    for peer in peers {
        let Some(connection) = connections.get_mut(&peer) else {
            continue;
        };
        let due: Vec<TimerId> = connection
            .timers
            .iter()
            .filter(|(_, deadline)| now.as_micros() >= deadline.as_micros())
            .map(|(id, _)| *id)
            .collect();
        for id in due {
            connection.timers.remove(&id);
            if let Err(error) = connection.conn.handle_timer(id, now) {
                tracing::debug!(%error, "Rust harness SRT sink timer failed");
            }
        }

        let output = if connected {
            RustSinkOutput::Connected { socket }
        } else {
            RustSinkOutput::Datagram {
                socket,
                peer: peer.peer,
            }
        };
        if let Err(error) =
            drain_rust_outputs_mode(&mut connection.conn, output, &mut connection.timers, now)
        {
            tracing::debug!(%error, "Rust harness SRT sink output failed");
            disconnected.push(peer);
            continue;
        }

        while let Some(event) = connection.conn.poll_event() {
            match event {
                ConnectionEvent::DataReceived { .. }
                | ConnectionEvent::Connected
                | ConnectionEvent::StateChanged(_) => {}
                ConnectionEvent::Disconnected { .. } => disconnected.push(peer),
                ConnectionEvent::Error(error) => {
                    tracing::debug!(%error, "Rust harness SRT sink connection error");
                }
                ConnectionEvent::KeyRefreshNeeded { .. } => {}
            }
        }
    }
    for peer in disconnected {
        connections.remove(&peer);
        routes.retain(|_, mapped| *mapped != peer);
    }
}

fn drain_rust_outputs_mode(
    conn: &mut SrtConnection,
    output: RustSinkOutput<'_>,
    timers: &mut HashMap<TimerId, Timestamp>,
    now: Timestamp,
) -> Result<(), String> {
    while let Some(connection_output) = conn.poll_output() {
        match connection_output {
            ConnectionOutput::SendPacket(bytes) => match output {
                RustSinkOutput::Datagram { socket, peer } => socket
                    .send_to(&bytes, peer)
                    .map(|_| ())
                    .map_err(|error| error.to_string())?,
                RustSinkOutput::Connected { socket } => socket
                    .send(&bytes)
                    .map(|_| ())
                    .map_err(|error| error.to_string())?,
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

mod connected;
mod group;
