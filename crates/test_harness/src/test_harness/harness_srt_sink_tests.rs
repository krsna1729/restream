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
fn ports_per_peer_is_bounded() {
    assert_eq!(clamp_ports_per_peer(0), 1);
    assert_eq!(clamp_ports_per_peer(2), 2);
    assert_eq!(clamp_ports_per_peer(usize::MAX), 4);
}

#[test]
fn sink_pool_starts_one_owner_per_port_and_stops() {
    let ports = free_udp_ports(2);
    let pool = HarnessSrtSinkPool::start(&ports, 0).expect("start sink pool");
    assert_eq!(pool.threads.len(), 2);
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

/// One srt-rs caller Owner with one shared UDP socket, as an egress shard
/// sends: it opens one session to each of `sinks`, sends `payloads`
/// datagrams on each, then keeps servicing until `done`. Returns whether each
/// session connected and how many payloads it sent.
fn run_shared_caller(
    sinks: Vec<SocketAddr>,
    payloads: usize,
    done: Arc<AtomicBool>,
) -> Vec<(bool, usize)> {
    use srt_transport::advanced::caller::{LogicalCallerState, PoolOutcome};
    use srt_transport::{CallerConfig, SocketOwnership};

    let runtime = production_runtime(ProductionRuntimeConfig::for_owner(
        CALLER_TX,
        SRT_OWNER_WIRE_CEILING,
    ))
    .expect("io_uring runtime for the test caller");
    runtime.block_on(async {
        let mut owner = Owner::new_with_ceiling(CALLER_TX, SRT_OWNER_WIRE_CEILING);
        // Every session handshakes at once, as an egress Owner's pool allows.
        owner
            .set_caller_pool_capacity(NonZeroUsize::new(sinks.len()).expect("at least one sink"))
            .expect("caller pool capacity");
        let epoch = Instant::now();
        let now = || Timestamp::from_micros(epoch.elapsed().as_micros() as u64);
        let ids: Vec<_> = sinks
            .iter()
            .map(|&sink| {
                let config = CallerConfig::builder(sink)
                    .ownership(SocketOwnership::Shared)
                    .build()
                    .expect("caller config");
                match owner.connect(&config, now()).expect("caller attaches") {
                    PoolOutcome::Admitted(id) => id,
                    outcome => panic!("the caller was not admitted immediately: {outcome:?}"),
                }
            })
            .collect();
        let payload = bytes::Bytes::from(vec![0x47; CALLER_PAYLOAD]);
        let mut reports = vec![(false, 0); ids.len()];
        let mut events = Vec::new();
        while !done.load(Ordering::Acquire) && epoch.elapsed() < Duration::from_secs(20) {
            let at = now();
            let _ = owner.service(at, OwnerServiceBudget::default()).await;
            owner.poll_caller_events(&mut events);
            events.clear();
            for (id, (connected, sent)) in ids.iter().zip(&mut reports) {
                let Some(mut caller) = owner.logical_caller_mut(id) else {
                    continue;
                };
                *connected |= caller.state() == Some(LogicalCallerState::Connected);
                while *connected
                    && *sent < payloads
                    && caller.can_send()
                    && matches!(caller.send_shared(payload.clone(), at), Ok(legs) if legs > 0)
                {
                    *sent += 1;
                }
            }
            owner.wait_for_activity(Duration::from_millis(2)).await;
        }
        for id in &ids {
            if let Some(mut caller) = owner.logical_caller_mut(id) {
                caller.disconnect(now());
            }
        }
        let _ = owner.shutdown_and_drain(Duration::from_secs(2)).await;
        reports
    })
}

/// An egress shard's sessions all leave one UDP socket, so they reach
/// different sink Owners only through the ports `sink_slot` spreads them
/// over (Q-027). Every port gets its share of the sessions, and every
/// session's bytes are counted on its port's Owner.
#[test]
fn shared_socket_caller_sessions_spread_across_sink_owners() {
    const PORTS: usize = 2;
    const SESSIONS: u64 = 8;
    const PAYLOADS: usize = 50;
    let ports = free_udp_ports(PORTS);
    let pool = HarnessSrtSinkPool::start(&ports, 0).expect("start sink pool");
    let sinks = (0..SESSIONS)
        .map(|slot| {
            let (instance, offset) = sink_slot(slot, 1, PORTS);
            assert_eq!(instance, 0, "one sink instance");
            SocketAddr::from(([127, 0, 0, 1], ports[usize::from(offset)]))
        })
        .collect();
    let done = Arc::new(AtomicBool::new(false));
    let caller = std::thread::spawn({
        let done = done.clone();
        move || run_shared_caller(sinks, PAYLOADS, done)
    });
    let handle = pool.counters();
    let expected = (PAYLOADS * CALLER_PAYLOAD) as u64;
    let delivered = |handle: &SrtSinkCountersHandle| {
        let peers = handle.per_peer_bytes();
        handle.snapshot().accepted == SESSIONS
            && peers.len() == SESSIONS as usize
            && peers.iter().all(|(_, bytes)| *bytes == expected)
    };
    // Checked while the sessions are connected: a disconnect retires a
    // session's byte row. Sink counters publish from the Owner threads.
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !delivered(&handle) {
        std::thread::sleep(Duration::from_millis(20));
    }
    let peers = handle.per_peer_bytes();
    let all_delivered = delivered(&handle);
    done.store(true, Ordering::Release);
    let reports = caller.join().expect("caller thread");
    let owner_rx_ports: BTreeMap<u16, OwnerRxDrops> = lock(&pool.counters.owner_rx)
        .iter()
        .map(|(port, rx)| (*port, *rx))
        .collect();
    let snapshot = handle.snapshot();
    let faults = handle.drops().owner_faults;
    pool.stop();
    assert!(
        reports.iter().all(|&report| report == (true, PAYLOADS)),
        "{reports:?}"
    );
    for port in &ports {
        let sessions = peers.iter().filter(|((peer_port, _), _)| peer_port == port);
        assert_eq!(
            sessions.count(),
            SESSIONS as usize / PORTS,
            "port {port} Owner carries its share of the sessions: {peers:?}"
        );
        assert!(
            owner_rx_ports.contains_key(port),
            "port {port} Owner publishes its RX row"
        );
    }
    assert!(
        all_delivered,
        "every session accepted with all its bytes: {snapshot:?} {peers:?}"
    );
    assert_eq!(snapshot.discarded_bytes, SESSIONS * expected);
    assert_eq!(faults, 0);
}

/// A same-user `SO_REUSEPORT` socket already on a port (a leftover sink)
/// refuses the pool instead of sharing the port's datagrams with it.
#[test]
fn sink_pool_refuses_to_join_a_foreign_reuseport_socket() {
    let port = free_udp_ports(1)[0];
    let foreign =
        crate::udp_drain::bind_drain_socket(port, 1 << 20, Duration::from_millis(100), true)
            .expect("foreign SO_REUSEPORT socket");
    if let Ok(pool) = HarnessSrtSinkPool::start(&[port], 0) {
        pool.stop();
        panic!("the sink shared port {port} with a foreign reuseport socket");
    }
    drop(foreign);
}

#[test]
fn sink_pool_rejects_duplicate_ports() {
    let port = free_udp_ports(1)[0];
    match HarnessSrtSinkPool::start(&[port, port], 0) {
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
            9000,
            OwnerRxDrops {
                dropped: 3,
                buffer_exhaustions: 2,
                truncated: 1,
            },
        ),
        (
            9001,
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
        ports: Arc::from([9000, 9001]),
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
    let pool = HarnessSrtSinkPool::start(&ports, 0).expect("start sink pool");
    assert!(HarnessSrtSinkPool::start(&ports, 0).is_err());
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
