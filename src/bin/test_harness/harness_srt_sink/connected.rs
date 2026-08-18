use super::*;

#[derive(Default)]
struct Trace {
    listener_packets: AtomicU64,
    connected_socket_packets: AtomicU64,
    handoffs: AtomicU64,
    tuple_count: AtomicU64,
    group_packets: AtomicU64,
    group_worker_reuses: AtomicU64,
    source_worker_reuses: AtomicU64,
}

enum Command {
    AddPeer { peer: SocketAddr, packet: Vec<u8> },
    ForwardPacket { peer: SocketAddr, packet: Vec<u8> },
}

struct ConnectedPeer {
    peer: SocketAddr,
    socket: MioUdpSocket,
    connections: RustSinkConnections,
    routes: RustSinkRouteMap,
    groups: group::RustSinkGroups,
    group_routes: group::RustSinkGroupRoutes,
    last_activity: Instant,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct GroupAffinity {
    group_id: u32,
    stream_id: Option<String>,
}

const CONNECTED_PEER_IDLE_TIMEOUT: Duration = Duration::from_secs(5);

pub(super) fn start(
    ports: &[u16],
    thread_count: usize,
    udp_buffer: i32,
    routing: RustConnectedRouting,
    stop: Arc<AtomicBool>,
    crypto: RustSinkCrypto,
) -> Result<RustHarnessSrtSinkPool, String> {
    if ports.len() != 1 {
        return Err(format!(
            "Rust connected sink needs exactly one public port (got {})",
            ports.len()
        ));
    }

    let worker_count = thread_count.max(1);
    let trace = std::env::var_os("HARNESS_SRT_SINK_TRACE_HANDOFF")
        .is_some()
        .then(|| Arc::new(Trace::default()));
    let (release_sender, release_receiver) = mpsc::channel();
    let mut senders = Vec::with_capacity(worker_count);
    let mut threads: Vec<JoinHandle<()>> = Vec::with_capacity(worker_count + 1);
    for worker_index in 0..worker_count {
        let (sender, receiver) = mpsc::channel();
        let thread_stop = stop.clone();
        let sink_crypto = crypto.clone();
        let port = ports[0];
        let worker_udp_buffer = udp_buffer;
        let worker_release_sender = release_sender.clone();
        let worker_trace = trace.clone();
        let thread = match std::thread::Builder::new()
            .name(format!("harness-srt-connected-worker-{worker_index}"))
            .spawn(move || {
                run_worker(
                    port,
                    worker_udp_buffer,
                    receiver,
                    worker_release_sender,
                    thread_stop,
                    sink_crypto,
                    worker_trace,
                )
            }) {
            Ok(thread) => thread,
            Err(error) => {
                stop.store(true, Ordering::Relaxed);
                for thread in threads {
                    let _ = thread.join();
                }
                return Err(format!(
                    "spawn Rust connected sink worker {worker_index}: {error}"
                ));
            }
        };
        senders.push(sender);
        threads.push(thread);
    }

    let listener_senders = senders.clone();
    let listener_stop = stop.clone();
    let listener_port = ports[0];
    let listener_udp_buffer = udp_buffer;
    let listener_release_receiver = release_receiver;
    let listener_trace = trace.clone();
    let listener_routing = routing;
    let listener = match std::thread::Builder::new()
        .name("harness-srt-connected-listener".to_string())
        .spawn(move || {
            run_listener(
                listener_port,
                listener_udp_buffer,
                listener_senders,
                listener_release_receiver,
                listener_stop,
                listener_trace,
                listener_routing,
            )
        }) {
        Ok(thread) => thread,
        Err(error) => {
            stop.store(true, Ordering::Relaxed);
            for thread in threads {
                let _ = thread.join();
            }
            return Err(format!("spawn Rust connected sink listener: {error}"));
        }
    };
    threads.push(listener);

    tracing::info!(
        "[harness-srt-sink] listening with Rust connected datagrams on port {} across {} worker(s)",
        ports[0],
        worker_count
    );
    Ok(RustHarnessSrtSinkPool { stop, threads })
}

fn run_listener(
    port: u16,
    udp_buffer: i32,
    workers: Vec<Sender<Command>>,
    release_receiver: Receiver<SocketAddr>,
    stop: Arc<AtomicBool>,
    trace: Option<Arc<Trace>>,
    routing: RustConnectedRouting,
) {
    let std_socket = match bind_reuseport_socket(port, udp_buffer) {
        Ok(socket) => socket,
        Err(error) => {
            tracing::error!(%error, "Rust connected sink listener bind failed");
            stop.store(true, Ordering::Relaxed);
            return;
        }
    };
    let mut socket = MioUdpSocket::from_std(std_socket);
    let mut poll = match Poll::new() {
        Ok(poll) => poll,
        Err(error) => {
            tracing::error!(%error, "Rust connected sink listener poll creation failed");
            stop.store(true, Ordering::Relaxed);
            return;
        }
    };
    if let Err(error) = poll
        .registry()
        .register(&mut socket, Token(0), Interest::READABLE)
    {
        tracing::error!(%error, "Rust connected sink listener registration failed");
        stop.store(true, Ordering::Relaxed);
        return;
    }

    let mut events = Events::with_capacity(1);
    let mut packet = [0u8; 64 * 1024];
    let mut tuple_workers = HashMap::<SocketAddr, usize>::new();
    let mut group_workers = HashMap::<GroupAffinity, usize>::new();
    // Native libsrt's first induction datagram has no GROUP extension. Keep a
    // provisional source-IP owner until the group metadata appears so bonded
    // legs cannot be split between connected workers during admission.
    let mut source_workers = HashMap::<std::net::IpAddr, usize>::new();
    let mut source_tuple_counts = HashMap::<std::net::IpAddr, usize>::new();
    let mut worker_tuple_counts = vec![0usize; workers.len()];
    let mut worker_assignment_counts = vec![0usize; workers.len()];
    let mut next_worker = 0usize;
    while !stop.load(Ordering::Relaxed) {
        while let Ok(peer) = release_receiver.try_recv() {
            if let Some(worker) = tuple_workers.remove(&peer) {
                worker_tuple_counts[worker] = worker_tuple_counts[worker].saturating_sub(1);
            }
            let source = peer.ip();
            if let Some(count) = source_tuple_counts.get_mut(&source) {
                *count = count.saturating_sub(1);
                if *count == 0 {
                    source_tuple_counts.remove(&source);
                    source_workers.remove(&source);
                }
            }
        }
        if let Err(error) = poll.poll(&mut events, Some(Duration::from_millis(20)))
            && error.kind() != std::io::ErrorKind::Interrupted
        {
            tracing::error!(%error, "Rust connected sink listener poll failed");
            break;
        }
        if events.is_empty() {
            continue;
        }

        loop {
            let (size, peer) = match socket.recv_from(&mut packet) {
                Ok(received) => received,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(error) => {
                    tracing::debug!(%error, "Rust connected sink listener receive failed");
                    break;
                }
            };
            if let Some(trace) = &trace {
                trace.listener_packets.fetch_add(1, Ordering::Relaxed);
            }
            let group_affinity = group::group_extension_from_packet(&packet[..size]).map(
                |(extension, stream_id)| GroupAffinity {
                    group_id: extension.group_id,
                    stream_id: group::normalize_stream_id(stream_id),
                },
            );
            if group_affinity.is_some()
                && let Some(trace) = &trace
            {
                trace.group_packets.fetch_add(1, Ordering::Relaxed);
            }
            let (worker, first_packet) = if let Some(worker) = tuple_workers.get(&peer).copied() {
                if group_affinity.is_some()
                    && let Some(trace) = &trace
                {
                    trace.group_worker_reuses.fetch_add(1, Ordering::Relaxed);
                }
                if let Some(affinity) = group_affinity.as_ref() {
                    group_workers.entry(affinity.clone()).or_insert(worker);
                }
                (worker, false)
            } else {
                let affinity_worker = group_affinity.as_ref().and_then(|affinity| {
                    group_workers.get(affinity).copied().or_else(|| {
                        affinity.stream_id.as_ref().and_then(|_| {
                            group_workers
                                .get(&GroupAffinity {
                                    group_id: affinity.group_id,
                                    stream_id: None,
                                })
                                .copied()
                        })
                    })
                });
                let source_worker = source_workers.get(&peer.ip()).copied();
                if affinity_worker.is_none()
                    && source_worker.is_some()
                    && let Some(trace) = &trace
                {
                    trace.source_worker_reuses.fetch_add(1, Ordering::Relaxed);
                }
                if affinity_worker.is_some()
                    && let Some(trace) = &trace
                {
                    trace.group_worker_reuses.fetch_add(1, Ordering::Relaxed);
                }
                let worker = affinity_worker
                    .or(source_worker)
                    .unwrap_or_else(|| match routing {
                        RustConnectedRouting::RoundRobin => {
                            let worker = next_worker % workers.len();
                            next_worker = next_worker.wrapping_add(1);
                            worker
                        }
                        RustConnectedRouting::LeastTuples => {
                            let mut selected = next_worker % workers.len();
                            for offset in 1..workers.len() {
                                let candidate = (next_worker + offset) % workers.len();
                                if worker_tuple_counts[candidate] < worker_tuple_counts[selected] {
                                    selected = candidate;
                                }
                            }
                            next_worker = selected.wrapping_add(1);
                            selected
                        }
                    });
                tuple_workers.insert(peer, worker);
                worker_tuple_counts[worker] += 1;
                source_workers.entry(peer.ip()).or_insert(worker);
                *source_tuple_counts.entry(peer.ip()).or_default() += 1;
                if let Some(affinity) = group_affinity {
                    group_workers.entry(affinity).or_insert(worker);
                }
                if let Some(trace) = &trace {
                    trace.handoffs.fetch_add(1, Ordering::Relaxed);
                    trace.tuple_count.fetch_add(1, Ordering::Relaxed);
                }
                worker_assignment_counts[worker] += 1;
                (worker, true)
            };
            let command = if first_packet {
                Command::AddPeer {
                    peer,
                    packet: packet[..size].to_vec(),
                }
            } else {
                Command::ForwardPacket {
                    peer,
                    packet: packet[..size].to_vec(),
                }
            };
            if workers[worker].send(command).is_err() {
                tuple_workers.remove(&peer);
                let source = peer.ip();
                if let Some(count) = source_tuple_counts.get_mut(&source) {
                    *count = count.saturating_sub(1);
                    if *count == 0 {
                        source_tuple_counts.remove(&source);
                        source_workers.remove(&source);
                    }
                }
                if first_packet {
                    worker_tuple_counts[worker] = worker_tuple_counts[worker].saturating_sub(1);
                    worker_assignment_counts[worker] =
                        worker_assignment_counts[worker].saturating_sub(1);
                }
                tracing::debug!(%peer, worker, "Rust connected sink worker channel closed");
            }
        }
    }
    if let Some(trace) = trace {
        eprintln!(
            "[rust-sink-handoff] listener_packets={} connected_socket_packets={} handoffs={} tuples={} group_packets={} group_worker_reuses={} source_worker_reuses={} worker_assignments={:?}",
            trace.listener_packets.load(Ordering::Relaxed),
            trace.connected_socket_packets.load(Ordering::Relaxed),
            trace.handoffs.load(Ordering::Relaxed),
            trace.tuple_count.load(Ordering::Relaxed),
            trace.group_packets.load(Ordering::Relaxed),
            trace.group_worker_reuses.load(Ordering::Relaxed),
            trace.source_worker_reuses.load(Ordering::Relaxed),
            worker_assignment_counts,
        );
    }
}

fn run_worker(
    port: u16,
    udp_buffer: i32,
    receiver: Receiver<Command>,
    release_sender: Sender<SocketAddr>,
    stop: Arc<AtomicBool>,
    crypto: RustSinkCrypto,
    trace: Option<Arc<Trace>>,
) {
    let mut poll = match Poll::new() {
        Ok(poll) => poll,
        Err(error) => {
            tracing::error!(%error, "Rust connected sink worker poll creation failed");
            return;
        }
    };
    let mut peers = Vec::<Option<ConnectedPeer>>::new();
    let mut peer_indexes = HashMap::<SocketAddr, usize>::new();
    let mut events = Events::with_capacity(1024);
    let mut packet = [0u8; 64 * 1024];
    let start = Instant::now();
    let mut next_socket_id = std::process::id().wrapping_add(1);
    let mut next_group_id = std::process::id().wrapping_add(1);
    let mut observed_socket_ids = HashMap::<SocketAddr, HashSet<u32>>::new();

    while !stop.load(Ordering::Relaxed) {
        while let Ok(command) = receiver.try_recv() {
            let (peer, packet) = match command {
                Command::AddPeer { peer, packet } | Command::ForwardPacket { peer, packet } => {
                    (peer, packet)
                }
            };
            let peer_index = match ensure_connected_peer(
                port,
                udp_buffer,
                &mut poll,
                &mut peers,
                &mut peer_indexes,
                peer,
            ) {
                Ok(index) => index,
                Err(error) => {
                    tracing::debug!(%error, %peer, "Rust connected sink peer setup failed");
                    continue;
                }
            };
            let Some(Some(connected_peer)) = peers.get_mut(peer_index) else {
                continue;
            };
            connected_peer.last_activity = Instant::now();
            observe_socket_id(&mut observed_socket_ids, peer, &packet);
            receive_connected_packet(
                peer,
                &packet,
                connected_peer,
                &crypto,
                start,
                &mut next_socket_id,
                &mut next_group_id,
            );
        }

        let wait = connected_poll_wait(&peers, timestamp(start));
        if let Err(error) = poll.poll(&mut events, Some(wait))
            && error.kind() != std::io::ErrorKind::Interrupted
        {
            tracing::error!(%error, "Rust connected sink worker poll failed");
            break;
        }
        for event in &events {
            let Some(peer_index) = event.token().0.checked_sub(1) else {
                continue;
            };
            let Some(Some(connected_peer)) = peers.get_mut(peer_index) else {
                continue;
            };
            loop {
                let size = match connected_peer.socket.recv(&mut packet) {
                    Ok(size) => size,
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => break,
                    Err(error) => {
                        tracing::debug!(%error, %connected_peer.peer, "Rust connected sink receive failed");
                        break;
                    }
                };
                if let Some(trace) = &trace {
                    trace
                        .connected_socket_packets
                        .fetch_add(1, Ordering::Relaxed);
                }
                connected_peer.last_activity = Instant::now();
                observe_socket_id(
                    &mut observed_socket_ids,
                    connected_peer.peer,
                    &packet[..size],
                );
                receive_connected_packet(
                    connected_peer.peer,
                    &packet[..size],
                    connected_peer,
                    &crypto,
                    start,
                    &mut next_socket_id,
                    &mut next_group_id,
                );
            }
        }

        let now = timestamp(start);
        let mut released = Vec::new();
        for index in 0..peers.len() {
            let Some(Some(connected_peer)) = peers.get_mut(index) else {
                continue;
            };
            process_rust_connections_mode(
                &connected_peer.socket,
                &mut connected_peer.connections,
                &mut connected_peer.routes,
                now,
                true,
            );
            group::process_connected(
                &mut connected_peer.groups,
                &mut connected_peer.group_routes,
                &connected_peer.socket,
                now,
            );
            if connected_peer.connections.is_empty()
                && connected_peer.groups.is_empty()
                && connected_peer.last_activity.elapsed() >= CONNECTED_PEER_IDLE_TIMEOUT
            {
                let peer = connected_peer.peer;
                let Some(mut removed) = peers[index].take() else {
                    continue;
                };
                let _ = poll.registry().deregister(&mut removed.socket);
                peer_indexes.remove(&peer);
                released.push(peer);
            }
        }
        for peer in released {
            let _ = release_sender.send(peer);
        }
    }
    if trace.is_some() {
        let unique_socket_ids: usize = observed_socket_ids.values().map(HashSet::len).sum();
        let max_socket_ids_per_tuple = observed_socket_ids
            .values()
            .map(HashSet::len)
            .max()
            .unwrap_or(0);
        eprintln!(
            "[rust-sink-socket-ids] tuples={} unique_socket_ids={} max_socket_ids_per_tuple={}",
            observed_socket_ids.len(),
            unique_socket_ids,
            max_socket_ids_per_tuple,
        );
    }
}

fn receive_connected_packet(
    peer: SocketAddr,
    packet: &[u8],
    connected_peer: &mut ConnectedPeer,
    crypto: &RustSinkCrypto,
    start: Instant,
    next_socket_id: &mut u32,
    next_group_id: &mut u32,
) {
    let socket = &connected_peer.socket;
    let mut state = RustSinkGroupPoolState {
        connections: &mut connected_peer.connections,
        routes: &mut connected_peer.routes,
        groups: &mut connected_peer.groups,
        group_routes: &mut connected_peer.group_routes,
        crypto,
        start,
        next_socket_id,
        next_group_id,
    };
    group::receive(
        peer,
        packet,
        &mut state,
        RustSinkOutput::Connected { socket },
    );
}

fn observe_socket_id(
    observed_socket_ids: &mut HashMap<SocketAddr, HashSet<u32>>,
    peer: SocketAddr,
    packet: &[u8],
) {
    let socket_id = rust_sink_connection_key(peer, packet).socket_id;
    observed_socket_ids
        .entry(peer)
        .or_default()
        .insert(socket_id);
}

fn ensure_connected_peer(
    port: u16,
    udp_buffer: i32,
    poll: &mut Poll,
    peers: &mut Vec<Option<ConnectedPeer>>,
    peer_indexes: &mut HashMap<SocketAddr, usize>,
    peer: SocketAddr,
) -> Result<usize, String> {
    if let Some(index) = peer_indexes.get(&peer).copied() {
        return Ok(index);
    }

    let std_socket = connect_reuseport_socket(port, peer, udp_buffer)?;
    let mut socket = MioUdpSocket::from_std(std_socket);
    let index = peers
        .iter()
        .position(|slot| slot.is_none())
        .unwrap_or(peers.len());
    poll.registry()
        .register(&mut socket, Token(index + 1), Interest::READABLE)
        .map_err(|error| format!("register Rust connected sink peer {peer}: {error}"))?;
    let connected_peer = ConnectedPeer {
        peer,
        socket,
        connections: HashMap::new(),
        routes: HashMap::new(),
        groups: HashMap::new(),
        group_routes: HashMap::new(),
        last_activity: Instant::now(),
    };
    if index == peers.len() {
        peers.push(Some(connected_peer));
    } else {
        peers[index] = Some(connected_peer);
    }
    peer_indexes.insert(peer, index);
    Ok(index)
}

fn connected_poll_wait(peers: &[Option<ConnectedPeer>], now: Timestamp) -> Duration {
    let connection_micros = peers
        .iter()
        .filter_map(Option::as_ref)
        .flat_map(|peer| peer.connections.values())
        .flat_map(|connection| connection.timers.values())
        .map(|deadline| deadline.as_micros().saturating_sub(now.as_micros()))
        .min()
        .unwrap_or(20_000)
        .clamp(1, 20_000);
    let group_micros = peers
        .iter()
        .filter_map(Option::as_ref)
        .map(|peer| group::poll_wait(&peer.groups, now).as_micros() as u64)
        .min()
        .unwrap_or(20_000);
    let micros = connection_micros.min(group_micros).clamp(1, 20_000);
    Duration::from_micros(micros)
}

fn connect_reuseport_socket(
    port: u16,
    peer: SocketAddr,
    udp_buffer: i32,
) -> Result<StdUdpSocket, String> {
    let socket = bind_reuseport_socket(port, udp_buffer)?;
    socket
        .connect(peer)
        .map_err(|error| format!("connect Rust sink datagram {port} to {peer}: {error}"))?;
    Ok(socket)
}
