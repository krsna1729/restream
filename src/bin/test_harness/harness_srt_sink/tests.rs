use super::rust_sink::{RustHarnessSrtSinkPool, rust_sink_connection_key};
use super::*;
use shiguredo_srt::HandshakePacket;

#[test]
fn parses_the_rust_sink_backend_for_rust_stack_runs() {
    assert_eq!(
        HarnessSrtSinkBackend::parse("rust").expect("rust backend"),
        HarnessSrtSinkBackend::Rust
    );
    assert_eq!(
        HarnessSrtSinkBackend::parse("libsrt").expect("libsrt backend"),
        HarnessSrtSinkBackend::Libsrt
    );
    assert!(HarnessSrtSinkBackend::parse("mixed").is_err());
    assert_eq!(
        RustSinkScaling::parse("distinct-ports").expect("ports scaling"),
        RustSinkScaling::Ports
    );
    assert_eq!(
        RustSinkScaling::parse("one-port-per-stream").expect("per-stream scaling"),
        RustSinkScaling::PerStreamPort
    );
    assert_eq!(
        RustSinkScaling::parse("reuse-port").expect("reuseport scaling"),
        RustSinkScaling::ReusePort
    );
    assert_eq!(
        RustSinkScaling::parse("connected-dgram").expect("connected scaling"),
        RustSinkScaling::Connected
    );
    assert!(RustSinkScaling::parse("shared").is_err());
    assert_eq!(
        RustConnectedRouting::parse("round-robin").expect("round-robin routing"),
        RustConnectedRouting::RoundRobin
    );
    assert_eq!(
        RustConnectedRouting::parse("least-loaded").expect("least-tuples routing"),
        RustConnectedRouting::LeastTuples
    );
    assert!(RustConnectedRouting::parse("random").is_err());
}

#[test]
fn rust_harness_srt_sink_pool_starts_and_stops_without_connections() {
    let ports = free_udp_ports(2);
    let pool = RustHarnessSrtSinkPool::start(
        &ports,
        8 * 1024 * 1024,
        1,
        RustSinkScaling::Ports,
        &HarnessSrtCrypto::plaintext(),
    )
    .expect("start Rust harness sink pool");
    assert_eq!(pool.threads.len(), 1);
    pool.stop();
}

#[test]
fn rust_harness_srt_sink_pool_uses_requested_worker_count() {
    let ports = free_udp_ports(4);
    let pool = RustHarnessSrtSinkPool::start(
        &ports,
        8 * 1024 * 1024,
        2,
        RustSinkScaling::Ports,
        &HarnessSrtCrypto::plaintext(),
    )
    .expect("start Rust harness sink pool");
    assert_eq!(pool.threads.len(), 2);
    pool.stop();
}

#[test]
fn rust_harness_srt_sink_pool_starts_each_scaling_mode() {
    for scaling in [
        RustSinkScaling::Ports,
        RustSinkScaling::PerStreamPort,
        RustSinkScaling::ReusePort,
        RustSinkScaling::Connected,
    ] {
        let ports = free_udp_ports(1);
        let pool = RustHarnessSrtSinkPool::start(
            &ports,
            8 * 1024 * 1024,
            2,
            scaling,
            &HarnessSrtCrypto::plaintext(),
        )
        .expect("start Rust harness scaling mode");
        let expected_threads = match scaling {
            RustSinkScaling::Ports | RustSinkScaling::PerStreamPort => 1,
            RustSinkScaling::ReusePort => 2,
            RustSinkScaling::Connected => 3,
        };
        assert_eq!(pool.threads.len(), expected_threads);
        pool.stop();
    }
}

#[test]
fn rust_sink_connection_key_separates_shared_udp_mux_connections() {
    let peer = "127.0.0.1:40000".parse().expect("test peer address");
    let mut first_bytes = Vec::new();
    HandshakePacket::new_induction_request(101)
        .encode(0, 0)
        .encode(&mut first_bytes);
    let mut second_bytes = Vec::new();
    HandshakePacket::new_induction_request(202)
        .encode(0, 0)
        .encode(&mut second_bytes);

    assert_ne!(
        rust_sink_connection_key(peer, &first_bytes),
        rust_sink_connection_key(peer, &second_bytes)
    );
}

fn free_udp_ports(count: usize) -> Vec<u16> {
    (0..count)
        .map(|_| {
            let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind probe socket");
            socket.local_addr().expect("probe socket addr").port()
        })
        .collect()
}

#[test]
fn harness_srt_sink_pool_starts_and_stops_without_connections() {
    let ports = free_udp_ports(1);
    let pool =
        HarnessSrtSinkPool::start(&ports, 8 * 1024 * 1024, 1).expect("start harness sink pool");
    pool.stop();
}

#[test]
fn harness_srt_sink_pool_rejects_a_port_already_bound() {
    let ports = free_udp_ports(1);
    let pool =
        HarnessSrtSinkPool::start(&ports, 8 * 1024 * 1024, 1).expect("start harness sink pool");
    let conflict = HarnessSrtSinkPool::start(&ports, 8 * 1024 * 1024, 1);
    assert!(
        conflict.is_err(),
        "second pool on {ports:?} unexpectedly bound"
    );
    pool.stop();
}

#[test]
fn harness_srt_sink_pool_clamps_thread_count_to_port_count() {
    let ports = free_udp_ports(2);
    // Requesting more threads than ports must not panic or leave a
    // port unowned; it clamps to one thread per port.
    let pool =
        HarnessSrtSinkPool::start(&ports, 8 * 1024 * 1024, 8).expect("start harness sink pool");
    assert_eq!(pool.threads.len(), 2);
    pool.stop();
}

#[test]
fn harness_srt_sink_pool_partitions_ports_across_fewer_threads() {
    let ports = free_udp_ports(4);
    let pool =
        HarnessSrtSinkPool::start(&ports, 8 * 1024 * 1024, 2).expect("start harness sink pool");
    assert_eq!(pool.threads.len(), 2);
    assert_eq!(pool.listeners.len(), 4);
    pool.stop();
}
