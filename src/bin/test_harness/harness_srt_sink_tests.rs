//! Tests for `harness_srt_sink`, split out for file size.

use super::*;

fn free_udp_ports(count: usize) -> Vec<u16> {
    (0..count)
        .map(|_| {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("probe socket");
            socket.local_addr().expect("probe address").port()
        })
        .collect()
}

#[test]
fn owners_per_port_is_bounded() {
    assert_eq!(clamp_owners_per_port(0), 1);
    assert_eq!(clamp_owners_per_port(2), 2);
    assert_eq!(clamp_owners_per_port(usize::MAX), 4);
}

#[test]
fn sink_pool_starts_two_owners_per_port_and_stops() {
    let ports = free_udp_ports(2);
    let pool = HarnessSrtSinkPool::start(&ports, 0, 2).expect("start multi-owner sink pool");
    assert_eq!(pool.threads.len(), 4);
    let drops = pool.counters().drops();
    let sockets = drops.socket.expect("/proc/net/udp readable on Linux");
    for port in &ports {
        assert!(
            sockets.keys().any(|(socket_port, _)| socket_port == port),
            "no per-socket drop rows for port {port}: {sockets:?}"
        );
    }
    pool.stop();
}

const CALLER_TX: usize = 64;
const CALLER_PAYLOAD: usize = 1316;

/// One srt-rs caller on its own Owner (so its own UDP source port),
/// driven like the ingress tests' `run_caller`: connect to `sink`, send
/// `payloads` datagrams, then keep servicing until `done`. Returns whether
/// it connected and how many payloads it sent.
fn run_caller(sink: SocketAddr, payloads: usize, done: Arc<AtomicBool>) -> (bool, usize) {
    use srt_transport::advanced::caller::{LogicalCallerState, PoolOutcome};
    use srt_transport::{CallerConfig, SocketOwnership};

    let runtime = production_runtime(ProductionRuntimeConfig::for_owner(
        CALLER_TX,
        SRT_OWNER_WIRE_CEILING,
    ))
    .expect("io_uring runtime for the test caller");
    runtime.block_on(async {
        let mut owner = Owner::new_with_ceiling(CALLER_TX, SRT_OWNER_WIRE_CEILING);
        let epoch = Instant::now();
        let now = || Timestamp::from_micros(epoch.elapsed().as_micros() as u64);
        let config = CallerConfig::builder(sink)
            .ownership(SocketOwnership::Shared)
            .build()
            .expect("caller config");
        let outcome = owner.connect(&config, now()).expect("caller attaches");
        let PoolOutcome::Admitted(id) = outcome else {
            panic!("the caller was not admitted immediately: {outcome:?}");
        };
        let payload = bytes::Bytes::from(vec![0x47; CALLER_PAYLOAD]);
        let (mut connected, mut sent) = (false, 0);
        let mut events = Vec::new();
        while !done.load(Ordering::Acquire) && epoch.elapsed() < Duration::from_secs(20) {
            let at = now();
            let _ = owner.service(at, OwnerServiceBudget::default()).await;
            owner.poll_caller_events(&mut events);
            events.clear();
            if let Some(mut caller) = owner.logical_caller_mut(&id) {
                connected |= caller.state() == Some(LogicalCallerState::Connected);
                while connected
                    && sent < payloads
                    && caller.can_send()
                    && matches!(caller.send_shared(payload.clone(), at), Ok(legs) if legs > 0)
                {
                    sent += 1;
                }
            }
            owner.wait_for_activity(Duration::from_millis(2)).await;
        }
        if let Some(mut caller) = owner.logical_caller_mut(&id) {
            caller.disconnect(now());
        }
        let _ = owner.shutdown_and_drain(Duration::from_secs(2)).await;
        (connected, sent)
    })
}

/// Real callers from distinct source ports reach a 2-Owner reuseport
/// group: every one is accepted and its bytes counted, and both Owners
/// publish their own RX counters.
#[test]
fn multi_owner_sink_accepts_and_counts_real_callers() {
    const CALLERS: usize = 8;
    const PAYLOADS: usize = 50;
    let port = free_udp_ports(1)[0];
    let pool = HarnessSrtSinkPool::start(&[port], 0, 2).expect("start multi-owner sink pool");
    let sink = SocketAddr::from(([127, 0, 0, 1], port));
    let done = Arc::new(AtomicBool::new(false));
    let callers: Vec<_> = (0..CALLERS)
        .map(|_| {
            let done = done.clone();
            std::thread::spawn(move || run_caller(sink, PAYLOADS, done))
        })
        .collect();
    let handle = pool.counters();
    let expected = (PAYLOADS * CALLER_PAYLOAD) as u64;
    let delivered = |handle: &SrtSinkCountersHandle| {
        let peers = handle.per_peer_bytes();
        handle.snapshot().accepted == CALLERS as u64
            && peers.len() == CALLERS
            && peers.iter().all(|(_, bytes)| *bytes == expected)
    };
    let owner_rx_entries = || {
        lock(&pool.counters.owner_rx)
            .keys()
            .filter(|(owner_port, _)| *owner_port == port)
            .count()
    };
    // Checked while the callers are connected: a disconnect retires a
    // peer's byte row.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !delivered(&handle) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let peers = handle.per_peer_bytes();
    let all_delivered = delivered(&handle);
    done.store(true, Ordering::Release);
    let reports: Vec<_> = callers
        .into_iter()
        .map(|caller| caller.join().expect("caller thread"))
        .collect();
    assert!(
        reports.iter().all(|&report| report == (true, PAYLOADS)),
        "{reports:?}"
    );
    let snapshot = handle.snapshot();
    assert!(
        all_delivered,
        "every caller accepted with all its bytes: {snapshot:?} {peers:?}"
    );
    assert_eq!(snapshot.accepted, CALLERS as u64, "{snapshot:?}");
    assert_eq!(snapshot.discarded_bytes, CALLERS as u64 * expected);
    // Both Owners publish within `PUBLISH_EVERY`, traffic or not.
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline && owner_rx_entries() < 2 {
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(owner_rx_entries(), 2, "each Owner publishes its own RX row");
    assert_eq!(handle.drops().owner_faults, 0);
    pool.stop();
}

/// A raw Broadcast publisher whose two legs dial from separate sockets,
/// so the kernel can hash them to different reuseport members (the
/// srt-rs relocation tests' publisher; theirs is crate-private).
struct RawBondedPublisher {
    legs: Vec<(
        srt_proto::SrtConnection,
        std::net::UdpSocket,
        srt_transport::advanced::driver::ManualTimerStore,
    )>,
}

impl RawBondedPublisher {
    fn new(index: u32, sink: SocketAddr, now: Timestamp) -> Self {
        use srt_proto::handshake::{GroupExtensionData, GroupType, SRTGROUP_MASK};
        let extension = GroupExtensionData {
            group_id: SRTGROUP_MASK | (100 + index),
            group_type: GroupType::Broadcast,
            flags: 0,
            weight: 1,
        };
        let legs = (0..2)
            .map(|leg| {
                let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("leg socket");
                socket.set_nonblocking(true).expect("nonblocking");
                socket.connect(sink).expect("leg connects");
                let mut conn = srt_proto::SrtConnection::new_caller(srt_proto::ConnectionOptions {
                    socket_id: 5_000 + index * 2 + leg,
                    initial_seq: Some(700),
                    group_extension: Some(extension),
                    ..srt_proto::ConnectionOptions::default()
                });
                conn.connect(now).expect("caller starts handshake");
                (
                    conn,
                    socket,
                    srt_transport::advanced::driver::ManualTimerStore::new(),
                )
            })
            .collect();
        Self { legs }
    }

    /// One visit: fire timers, receive, send.
    fn pump(&mut self, now: Timestamp) {
        let mut buf = [0u8; 2048];
        for (conn, socket, timers) in &mut self.legs {
            timers.fire_expired(now, conn);
            while let Ok(len) = socket.recv(&mut buf) {
                let _ = conn.feed_recv_buf(&buf[..len], now);
            }
            while let Ok(Some(output)) = conn.poll_output() {
                match output {
                    srt_proto::ConnectionOutput::SendPacket(bytes) => {
                        let _ = socket.send(&bytes);
                    }
                    other => timers.apply_output(&other, now),
                }
            }
        }
    }

    fn connected(&self) -> bool {
        self.legs
            .iter()
            .all(|(conn, ..)| conn.state() == srt_proto::ConnectionState::Connected)
    }

    /// Send on one leg only, so the group's delivery depends on it.
    fn send_on(&mut self, leg: usize, payload: &[u8], now: Timestamp) {
        let _ = self.legs[leg].0.send(payload, now);
    }
}

/// Bonded legs the kernel hashes away from their group's member relocate
/// there with their timers. The second member starts `STAGGER` late and
/// each publisher sends on one leg only (alternating which), so every
/// relocated leg must deliver by itself. Only one pool-wide timestamp
/// origin does that: with per-Owner clocks a relocated leg's TSBPD
/// schedule is `STAGGER` off, so its data is held back or dropped late.
#[test]
fn relocated_bonded_legs_deliver_across_staggered_owners() {
    const GROUPS: u32 = 32;
    const PAYLOADS: u32 = 20;
    const STAGGER: Duration = Duration::from_secs(2);
    const DELIVER_WITHIN: Duration = Duration::from_secs(1);
    let port = free_udp_ports(1)[0];
    let pool = HarnessSrtSinkPool::start_staggered(&[port], 0, 2, STAGGER)
        .expect("start staggered sink pool");
    let handle = pool.counters();
    let sink = SocketAddr::from(([127, 0, 0, 1], port));
    let epoch = Instant::now();
    let at = || Timestamp::from_micros(epoch.elapsed().as_micros() as u64);
    let mut publishers: Vec<_> = (0..GROUPS)
        .map(|index| RawBondedPublisher::new(index, sink, at()))
        .collect();
    let expected = u64::from(PAYLOADS) * 4;
    let delivered = || {
        let peers = handle.per_peer_bytes();
        peers.len() == GROUPS as usize && peers.iter().all(|(_, bytes)| *bytes == expected)
    };
    let connect_deadline = Instant::now() + Duration::from_secs(5);
    let mut sent = 0;
    let mut sent_all_at: Option<Instant> = None;
    loop {
        for publisher in &mut publishers {
            publisher.pump(at());
        }
        if publishers.iter().all(RawBondedPublisher::connected) {
            if sent < PAYLOADS {
                for (index, publisher) in publishers.iter_mut().enumerate() {
                    publisher.send_on(index % 2, &sent.to_be_bytes(), at());
                }
                sent += 1;
            } else if delivered()
                || sent_all_at.get_or_insert_with(Instant::now).elapsed() > DELIVER_WITHIN
            {
                break;
            }
        } else if Instant::now() > connect_deadline {
            break;
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    let connected = publishers
        .iter()
        .filter(|publisher| publisher.connected())
        .count();
    let all_delivered = delivered();
    let peers = handle.per_peer_bytes();
    let snapshot = pool.snapshot();
    pool.stop();
    assert_eq!(
        connected, GROUPS as usize,
        "every bonded publisher connects"
    );
    assert!(
        all_delivered,
        "every group delivers {expected} B within {DELIVER_WITHIN:?} of its last send: {peers:?}"
    );
    assert_eq!(
        snapshot.accepted,
        u64::from(GROUPS),
        "one stream per bonded publisher: {snapshot:?}"
    );
}

/// A same-user socket already in a port's reuseport group (a leftover
/// sink) refuses the pool instead of being joined.
#[test]
fn sink_pool_refuses_to_join_a_foreign_reuseport_socket() {
    let port = free_udp_ports(1)[0];
    let foreign =
        crate::udp_drain::bind_drain_socket(port, 1 << 20, Duration::from_millis(100), true)
            .expect("foreign SO_REUSEPORT socket");
    match HarnessSrtSinkPool::start(&[port], 0, 2) {
        Err(error) => assert!(error.contains("already have a UDP socket"), "{error}"),
        Ok(pool) => {
            pool.stop();
            panic!("the sink joined a foreign reuseport group on port {port}");
        }
    }
    drop(foreign);
}

#[test]
fn sink_pool_rejects_duplicate_ports() {
    let port = free_udp_ports(1)[0];
    match HarnessSrtSinkPool::start(&[port, port], 0, 2) {
        Err(error) => assert!(error.contains("duplicate port"), "{error}"),
        Ok(pool) => {
            pool.stop();
            panic!("duplicate sink ports unexpectedly started");
        }
    }
}

#[test]
fn sink_owner_rx_drops_aggregate_per_owner() {
    let counters = Arc::new(SinkCounters::default());
    lock(&counters.owner_rx).extend([
        (
            (9000, 0),
            OwnerRxDrops {
                dropped: 3,
                buffer_exhaustions: 2,
                truncated: 1,
            },
        ),
        (
            (9000, 1),
            OwnerRxDrops {
                dropped: 5,
                buffer_exhaustions: 4,
                truncated: 2,
            },
        ),
    ]);
    counters.owner_faults.store(1, Ordering::Release);
    let handle = SrtSinkCountersHandle {
        counters,
        ports: Arc::from([9000]),
    };
    let drops = handle.drops();
    assert_eq!(
        drops.owner,
        OwnerRxDrops {
            dropped: 8,
            buffer_exhaustions: 6,
            truncated: 3,
        }
    );
    assert_eq!(drops.owner_faults, 1);
    assert_eq!(drops.json()["ownerFaults"], 1);
    assert_eq!(
        drops.since(&drops).owner_faults,
        1,
        "a fault stays visible in every later window"
    );
}

#[test]
fn sink_pool_rejects_a_port_already_bound() {
    let ports = free_udp_ports(1);
    let pool = HarnessSrtSinkPool::start(&ports, 0, 1).expect("start sink pool");
    assert!(HarnessSrtSinkPool::start(&ports, 0, 1).is_err());
    pool.stop();
}

#[test]
fn udp_table_rows_yield_port_inode_and_drops() {
    let table = "  sl  local_address rem_address   st tx_queue rx_queue tr tm->when retrnsmt   uid  timeout inode ref pointer drops\n\
          1: 00000000:1F90 00000000:0000 07 00000000:00000000 00:00000000 00000000  1000        0 12345 2 0000000000000000 17\n\
          2: 0100007F:1F90 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 12346 2 0000000000000000 3\n\
          3: 0100007F:0035 00000000:0000 07 00000000:00000000 00:00000000 00000000     0        0 23456 2 0000000000000000 0\n";
    let rows: Vec<_> = parse_udp_socket_drops(table).collect();
    assert_eq!(rows, [(8080, 12345, 17), (8080, 12346, 3), (53, 23456, 0)]);
    let v6 = "header\n  0: 00000000000000000000000000000000:1F91 00000000000000000000000000000000:0000 07 00000000:00000000 00:00000000 00000000 1000 0 1 2 0000000000000000 5\n";
    assert_eq!(
        parse_udp_socket_drops(v6).collect::<Vec<_>>(),
        [(8081, 1, 5)]
    );
}

#[test]
fn window_drops_subtract_the_start_reading_per_socket() {
    let start = SrtSinkDrops {
        owner: OwnerRxDrops {
            dropped: 10,
            buffer_exhaustions: 1,
            truncated: 0,
        },
        owner_faults: 0,
        socket: Some(BTreeMap::from([((1, 11), 100), ((1, 12), 5), ((2, 21), 5)])),
    };
    let end = SrtSinkDrops {
        owner: OwnerRxDrops {
            dropped: 25,
            buffer_exhaustions: 1,
            truncated: 2,
        },
        owner_faults: 0,
        socket: Some(BTreeMap::from([
            ((1, 11), 160),
            ((1, 12), 5),
            ((2, 21), 5),
            ((3, 31), 9),
        ])),
    };
    let window = end.since(&start);
    assert_eq!(window.owner.dropped, 15);
    assert_eq!(window.owner.truncated, 2);
    assert_eq!(
        window.socket,
        Some(BTreeMap::from([
            ((1, 11), 60),
            ((1, 12), 0),
            ((2, 21), 0),
            ((3, 31), 9),
        ]))
    );
    let unknown = SrtSinkDrops {
        socket: None,
        ..end.clone()
    };
    assert_eq!(unknown.since(&start).socket, None);
}

#[test]
fn sink_drop_json_identifies_each_socket() {
    let drops = SrtSinkDrops {
        socket: Some(BTreeMap::from([((9000, 11), 3), ((9000, 12), 5)])),
        ..SrtSinkDrops::default()
    }
    .json();
    assert_eq!(
        drops["socketDrops"],
        serde_json::json!([
            {"port": 9000, "inode": 11, "drops": 3},
            {"port": 9000, "inode": 12, "drops": 5}
        ])
    );
    assert_eq!(drops["socketDropsTotal"], 8);
}

proptest::proptest! {
    /// Any well-formed row retains its port, inode and drop count.
    #[test]
    fn udp_rows_round_trip(port in proptest::num::u16::ANY, drops in proptest::num::u64::ANY, addr in "[0-9A-F]{8}") {
        let table = format!(
            "header\n  0: {addr}:{port:04X} 00000000:0000 07 00000000:00000000 00:00000000 00000000 0 0 1 2 0000000000000000 {drops}\n"
        );
        let rows: Vec<_> = parse_udp_socket_drops(&table).collect();
        proptest::prop_assert_eq!(rows, vec![(port, 1, drops)]);
    }
}
