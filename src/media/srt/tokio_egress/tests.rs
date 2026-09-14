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
            .drive(shiguredo_srt::Timestamp::default())
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
            .drive(shiguredo_srt::Timestamp::default())
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
            .drive(shiguredo_srt::Timestamp::default())
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
