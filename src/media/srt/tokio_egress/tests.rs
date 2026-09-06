use super::{SharedSrtEgress, should_use_shared_srt_egress_state};

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

#[test]
fn shared_outbound_flush_sends_an_ipv4_batch_and_clears_leftover() {
    let sink = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind UDP sink");
    sink.set_read_timeout(Some(std::time::Duration::from_secs(1)))
        .expect("set sink timeout");
    let peer = sink.local_addr().expect("sink address");
    let mut shared = {
        let runtime = super::srt_runtime().expect("test Tokio runtime");
        let _guard = runtime.enter();
        SharedSrtEgress::bind(peer, &runtime).expect("bind shared SRT socket")
    };
    shared.outbound.extend([
        (peer, vec![1, 2, 3, 4]),
        (peer, vec![5, 6, 7, 8]),
        (peer, vec![9, 10, 11, 12]),
    ]);

    assert!(shared.flush_outbound().expect("flush outbound batch"));
    assert!(shared.outbound.is_empty());
    for expected in [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10, 11, 12]] {
        let mut received = [0_u8; 4];
        let (size, _) = sink.recv_from(&mut received).expect("receive datagram");
        assert_eq!(size, received.len());
        assert_eq!(received, expected);
    }
}
