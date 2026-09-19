use super::SharedSrtEgress;

#[test]
fn shared_outbound_flush_supports_ipv6() {
    let sink = match std::net::UdpSocket::bind("[::1]:0") {
        Ok(sink) => sink,
        Err(_) => return,
    };
    sink.set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .expect("set sink timeout");
    let peer = sink.local_addr().expect("sink address");
    let mut shared = SharedSrtEgress::bind(peer).expect("bind shared SRT socket");
    assert!(shared.enqueue_test_datagram(peer, vec![1, 2, 3, 4]));

    assert!(!shared.flush_outbound().expect("submit IPv6 datagram"));
    for _ in 0..8 {
        shared
            .drive(srt_proto::Timestamp::default())
            .expect("drive IPv6 datagram");
        if shared.flush_outbound().expect("complete IPv6 datagram") {
            break;
        }
    }
    assert!(shared.outbound_empty());
    let mut received = [0u8; 4];
    sink.recv(&mut received).expect("receive IPv6 datagram");
    assert_eq!(received, [1, 2, 3, 4]);
}

#[test]
fn shared_outbound_flush_sends_without_entering_the_runtime() {
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind UDP sink");
    sink.set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .expect("set sink timeout");
    let peer = sink.local_addr().expect("sink address");
    let mut shared = SharedSrtEgress::bind(peer).expect("bind shared SRT socket");
    assert!(shared.enqueue_test_datagram(peer, vec![1, 2, 3, 4]));

    for _ in 0..8 {
        shared
            .drive(srt_proto::Timestamp::default())
            .expect("drive native UDP readiness");
        if shared.flush_outbound().expect("complete datagram") {
            break;
        }
    }
    assert!(shared.outbound_empty());
    let mut received = [0_u8; 4];
    let (size, _) = sink.recv_from(&mut received).expect("receive datagram");
    assert_eq!(size, received.len());
    assert_eq!(received, [1, 2, 3, 4]);
}

#[test]
fn shared_outbound_flush_sends_an_ipv4_batch_and_clears_leftover() {
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind UDP sink");
    sink.set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .expect("set sink timeout");
    let peer = sink.local_addr().expect("sink address");
    let mut shared = SharedSrtEgress::bind(peer).expect("bind shared SRT socket");
    for packet in [vec![1, 2, 3, 4], vec![5, 6, 7, 8], vec![9, 10, 11, 12]] {
        assert!(shared.enqueue_test_datagram(peer, packet));
    }

    assert!(!shared.flush_outbound().expect("submit outbound batch"));
    for _ in 0..16 {
        shared
            .drive(srt_proto::Timestamp::default())
            .expect("drive outbound batch");
        if shared.flush_outbound().expect("complete outbound batch") {
            break;
        }
    }
    assert!(shared.outbound_empty());
    for expected in [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12]] {
        let mut received = [0_u8; 4];
        let (size, _) = sink.recv_from(&mut received).expect("receive datagram");
        assert_eq!(size, received.len());
        assert_eq!(received, expected);
    }
    let metrics = shared.native_metrics();
    assert_eq!(metrics.sqes, 3);
    assert_eq!(metrics.tx_packets, 3);
    assert_eq!(metrics.tx_bytes, 12);
}
#[test]
fn late_ipv6_family_registers_while_ipv4_traffic_is_in_flight() {
    let ipv4_sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind IPv4 sink");
    let ipv6_sink = match std::net::UdpSocket::bind("[::1]:0") {
        Ok(sink) => sink,
        Err(_) => return,
    };
    ipv4_sink
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();
    ipv6_sink
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();
    let ipv4_peer = ipv4_sink.local_addr().unwrap();
    let ipv6_peer = ipv6_sink.local_addr().unwrap();

    // 1. Initial setup with IPv4 only:
    let mut shared = SharedSrtEgress::bind(ipv4_peer).expect("bind shared with IPv4");
    assert!(shared.enqueue_test_datagram(ipv4_peer, vec![1, 2, 3, 4]));
    // Submit IPv4 send into the driver (now in flight):
    assert!(!shared.flush_outbound().expect("submit IPv4 datagram"));

    // 2. Late appearance of IPv6 while IPv4 is in flight:
    shared
        .ensure_for_peers(&[ipv4_peer, ipv6_peer])
        .expect("late IPv6 registration succeeds without dropping IPv4 in flight");

    // 3. Enqueue IPv6 datagram as well:
    assert!(shared.enqueue_test_datagram(ipv6_peer, vec![5, 6, 7, 8]));

    // 4. Drive both to completion:
    for _ in 0..16 {
        shared
            .drive(srt_proto::Timestamp::default())
            .expect("drive dual-family UDP driver");
        if shared
            .flush_outbound()
            .expect("flush dual-family datagrams")
        {
            break;
        }
    }
    assert!(shared.outbound_empty());

    let mut v4_buf = [0u8; 4];
    ipv4_sink.recv(&mut v4_buf).expect("receive IPv4 datagram");
    assert_eq!(v4_buf, [1, 2, 3, 4]);

    let mut v6_buf = [0u8; 4];
    ipv6_sink.recv(&mut v6_buf).expect("receive IPv6 datagram");
    assert_eq!(v6_buf, [5, 6, 7, 8]);
}

#[test]
fn late_ipv4_family_registers_while_ipv6_traffic_is_in_flight() {
    let ipv6_sink = match std::net::UdpSocket::bind("[::1]:0") {
        Ok(sink) => sink,
        Err(_) => return,
    };
    let ipv4_sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind IPv4 sink");
    ipv4_sink
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();
    ipv6_sink
        .set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .unwrap();
    let ipv4_peer = ipv4_sink.local_addr().unwrap();
    let ipv6_peer = ipv6_sink.local_addr().unwrap();

    // 1. Initial setup with IPv6 only:
    let mut shared = SharedSrtEgress::bind(ipv6_peer).expect("bind shared with IPv6");
    assert!(shared.enqueue_test_datagram(ipv6_peer, vec![10, 20, 30, 40]));
    // Submit IPv6 send into the driver (now in flight):
    assert!(!shared.flush_outbound().expect("submit IPv6 datagram"));

    // 2. Late appearance of IPv4 while IPv6 is in flight:
    shared
        .ensure_for_peers(&[ipv6_peer, ipv4_peer])
        .expect("late IPv4 registration succeeds without dropping IPv6 in flight");

    // 3. Enqueue IPv4 datagram as well:
    assert!(shared.enqueue_test_datagram(ipv4_peer, vec![50, 60, 70, 80]));

    // 4. Drive both to completion:
    for _ in 0..16 {
        shared
            .drive(srt_proto::Timestamp::default())
            .expect("drive dual-family UDP driver");
        if shared
            .flush_outbound()
            .expect("flush dual-family datagrams")
        {
            break;
        }
    }
    assert!(shared.outbound_empty());

    let mut v6_buf = [0u8; 4];
    ipv6_sink.recv(&mut v6_buf).expect("receive IPv6 datagram");
    assert_eq!(v6_buf, [10, 20, 30, 40]);

    let mut v4_buf = [0u8; 4];
    ipv4_sink.recv(&mut v4_buf).expect("receive IPv4 datagram");
    assert_eq!(v4_buf, [50, 60, 70, 80]);
}
