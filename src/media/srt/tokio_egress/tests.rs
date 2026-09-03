use std::sync::mpsc;

use super::{
    Conn, Mutex, RustSrtSocket, SharedRustSrtSocket, SharedSrtEgress, Timestamp, registry,
    should_use_shared_srt_egress_state, with_socket,
};

fn disconnected_test_socket() -> SharedRustSrtSocket {
    let runtime = super::srt_runtime().expect("test Tokio runtime");
    let _guard = runtime.enter();
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind test UDP socket");
    socket
        .set_nonblocking(true)
        .expect("make test UDP socket nonblocking");
    let socket = tokio::net::UdpSocket::from_std(socket).expect("wrap test UDP socket");
    let connection = srt_transport::SessionConfig::default()
        .caller(Timestamp::from_micros(0))
        .expect("build test SRT connection");
    std::sync::Arc::new(Mutex::new(RustSrtSocket::Direct(Box::new(Conn::new(
        connection, socket,
    )))))
}

#[test]
fn shared_srt_egress_state_selection_is_single_peer_only() {
    assert!(!should_use_shared_srt_egress_state(0, true));
    assert!(should_use_shared_srt_egress_state(1, true));
    assert!(
        !should_use_shared_srt_egress_state(2, true),
        "multi-peer outputs must route through bonded TokioGroupConn setup"
    );
    assert!(!should_use_shared_srt_egress_state(1, false));
}

#[test]
fn socket_work_does_not_hold_the_registry_lock() {
    let (first, second) = {
        let mut next = super::NEXT_SOCKET
            .get_or_init(|| Mutex::new(10))
            .lock()
            .expect("socket id lock");
        let first = *next;
        let second = first.saturating_add(1);
        *next = second.saturating_add(1);
        (first, second)
    };
    registry().lock().expect("registry lock").extend([
        (first, disconnected_test_socket()),
        (second, disconnected_test_socket()),
    ]);

    let (entered_tx, entered_rx) = mpsc::sync_channel(0);
    let (release_tx, release_rx) = mpsc::sync_channel(0);
    let first_worker = std::thread::spawn(move || {
        with_socket(first, |_| {
            entered_tx.send(()).expect("signal first socket lock");
            release_rx.recv().expect("release first socket lock");
        })
        .expect("first socket present");
    });
    entered_rx.recv().expect("first socket lock entered");

    let (completed_tx, completed_rx) = mpsc::sync_channel(0);
    let second_worker = std::thread::spawn(move || {
        with_socket(second, |_| ()).expect("second socket present");
        completed_tx.send(()).expect("signal second socket work");
    });
    completed_rx
        .recv_timeout(std::time::Duration::from_secs(1))
        .expect("independent socket work must not wait for the first socket");

    release_tx.send(()).expect("release first socket");
    first_worker.join().expect("first socket worker");
    second_worker.join().expect("second socket worker");
    let mut sockets = registry().lock().expect("registry cleanup lock");
    sockets.remove(&first);
    sockets.remove(&second);
}

#[test]
fn shared_outbound_flush_sends_without_entering_the_runtime() {
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind UDP sink");
    sink.set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .expect("set sink timeout");
    let peer = sink.local_addr().expect("sink address");
    let mut shared = {
        let runtime = super::srt_runtime().expect("test Tokio runtime");
        let _guard = runtime.enter();
        SharedSrtEgress::bind(peer, &runtime).expect("bind shared SRT socket")
    };
    shared.outbound.push((peer, vec![1, 2, 3, 4]));

    assert!(shared.flush_outbound().expect("flush outbound datagram"));
    assert!(shared.outbound.is_empty());
    let mut received = [0_u8; 4];
    let (size, _) = sink.recv_from(&mut received).expect("receive datagram");
    assert_eq!(size, received.len());
    assert_eq!(received, [1, 2, 3, 4]);
}
