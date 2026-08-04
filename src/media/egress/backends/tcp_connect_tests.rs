use super::*;
use std::net::TcpListener;
use std::os::unix::io::AsRawFd;

#[test]
fn connects_and_switches_to_nonblocking_mode() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let stream = connect_fabric_tcp_egress_socket(TcpFabricConnectConfig {
        peer_addr: addr,
        connect_timeout: Duration::from_secs(2),
    })
    .unwrap();

    assert!(stream.as_raw_fd() >= 0);
    // A non-blocking read on a socket with no data available must return
    // WouldBlock immediately rather than hanging — proves set_nonblocking
    // took effect rather than silently failing.
    let mut buf = [0u8; 1];
    use std::io::Read;
    let mut stream_mut = &stream;
    let result = stream_mut.read(&mut buf);
    assert!(matches!(
        result,
        Err(ref error) if error.kind() == io::ErrorKind::WouldBlock
    ));

    drop(listener);
}

#[test]
fn connect_can_be_registered_with_the_tcp_poller_and_reports_writable() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();

    let stream = connect_fabric_tcp_egress_socket(TcpFabricConnectConfig {
        peer_addr: addr,
        connect_timeout: Duration::from_secs(2),
    })
    .unwrap();
    let fd = stream.as_raw_fd();

    let mut poller = super::super::tcp::TcpEgressPoller::new(4).unwrap();
    poller
        .register_leaf(
            fd,
            crate::media::egress::scheduler::LeafKey(0),
            1,
            super::super::tcp::TcpEgressInterest::WRITE,
        )
        .unwrap();

    let mut ready = Vec::new();
    let count = poller.poll_leaves(1_000, &mut ready).unwrap();

    assert_eq!(count, 1);
    assert_eq!(ready[0].fd, fd);
    assert!(ready[0].writable);

    poller.remove(fd).unwrap();
}

#[test]
fn connect_times_out_against_an_unroutable_address() {
    // TEST-NET-1 (RFC 5737): reserved for documentation, never routed —
    // connect_timeout must return an error rather than hang past the bound.
    let unroutable: SocketAddr = "192.0.2.1:9".parse().unwrap();

    let result = connect_fabric_tcp_egress_socket(TcpFabricConnectConfig {
        peer_addr: unroutable,
        connect_timeout: Duration::from_millis(200),
    });

    assert!(result.is_err());
    assert_eq!(result.unwrap_err().operation, "connect");
}
