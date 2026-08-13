use super::test_support::{Event, FailStep, FakeSingleConnectOps};
use super::*;
use std::sync::Mutex;

fn peer_addr() -> SocketAddr {
    "127.0.0.1:9000".parse().unwrap()
}

fn test_buffer_opts() -> EgressBufferOpts {
    EgressBufferOpts::defaults(None).with_overrides(Some(6_250_000), None, None, None, None)
}

fn connect_config<'a>(
    muxer_port_claim: Option<SrtEgressMuxerPortClaim<'a>>,
) -> SrtSingleEgressConnectConfig<'a> {
    SrtSingleEgressConnectConfig {
        peer_addr: peer_addr(),
        stream_id: "publish:key",
        crypto: None,
        connect_timeout_ms: 1500,
        send_mode: SrtEgressSendMode::LegacyBlocking,
        muxer_port_claim,
        buffer_opts: test_buffer_opts(),
    }
}

fn fabric_connect_config() -> SrtSingleEgressConnectConfig<'static> {
    SrtSingleEgressConnectConfig {
        send_mode: SrtEgressSendMode::FabricNonblocking,
        ..connect_config(None)
    }
}

fn encrypted_connect_config<'a>(crypto: &'a SrtCryptoConfig) -> SrtSingleEgressConnectConfig<'a> {
    SrtSingleEgressConnectConfig {
        crypto: Some(crypto),
        ..connect_config(None)
    }
}

#[test]
fn single_socket_connect_records_first_local_port_after_connect() {
    let muxer_port = Mutex::new(None);
    let claim = super::super::claim_srt_egress_muxer_port(&muxer_port);
    let ops = FakeSingleConnectOps::new();

    let socket = connect_single_srt_egress_socket_with(connect_config(Some(claim)), &ops).unwrap();

    assert_eq!(socket, 42);
    assert_eq!(*muxer_port.lock().unwrap(), Some(41000));
    assert_eq!(
        ops.events.borrow().as_slice(),
        &[
            Event::Create,
            Event::Timeout(42, 1500),
            Event::EgressOpts(42, test_buffer_opts()),
            Event::ReuseAddr(42),
            Event::StreamId(42, "publish:key".to_string()),
            Event::Connect(42, peer_addr()),
            Event::Configure(42, SrtEgressSendMode::LegacyBlocking),
            Event::LocalPort(42),
            Event::Log(42),
        ]
    );
}

#[test]
fn single_socket_connect_binds_reused_muxer_port_before_connecting() {
    let muxer_port = Mutex::new(Some(40000));
    let claim = super::super::claim_srt_egress_muxer_port(&muxer_port);
    let ops = FakeSingleConnectOps::new();

    let socket = connect_single_srt_egress_socket_with(connect_config(Some(claim)), &ops).unwrap();

    assert_eq!(socket, 42);
    assert_eq!(*muxer_port.lock().unwrap(), Some(40000));
    assert_eq!(
        ops.events.borrow().as_slice(),
        &[
            Event::Create,
            Event::Timeout(42, 1500),
            Event::EgressOpts(42, test_buffer_opts()),
            Event::ReuseAddr(42),
            Event::StreamId(42, "publish:key".to_string()),
            Event::Bind(42, 40000),
            Event::Connect(42, peer_addr()),
            Event::Configure(42, SrtEgressSendMode::LegacyBlocking),
            Event::LocalPort(42),
            Event::Log(42),
        ]
    );
}

#[test]
fn single_socket_connect_closes_socket_when_reuseaddr_setup_fails() {
    let ops = FakeSingleConnectOps::failing(FailStep::ReuseAddr);

    let result = connect_single_srt_egress_socket_with(connect_config(None), &ops);

    assert_eq!(result, Err("reuseaddr failed".to_string()));
    let events = ops.events.borrow();
    assert_eq!(
        events.as_slice(),
        &[
            Event::Create,
            Event::Timeout(42, 1500),
            Event::EgressOpts(42, test_buffer_opts()),
            Event::ReuseAddr(42),
            Event::Close(42)
        ]
    );
}

#[test]
fn single_socket_connect_closes_socket_when_stream_id_setup_fails() {
    let ops = FakeSingleConnectOps::failing(FailStep::StreamId);

    let result = connect_single_srt_egress_socket_with(connect_config(None), &ops);

    assert_eq!(result, Err("stream id failed".to_string()));
    let events = ops.events.borrow();
    assert_eq!(events.last(), Some(&Event::Close(42)));
    assert!(!events.contains(&Event::Connect(42, peer_addr())));
}

#[test]
fn single_socket_connect_applies_crypto_before_stream_id_and_closes_when_crypto_fails() {
    let crypto =
        super::super::super::srt_crypto::srt_crypto_from_url("secret".to_string(), None).unwrap();
    let ops = FakeSingleConnectOps::failing(FailStep::Crypto);

    let result = connect_single_srt_egress_socket_with(encrypted_connect_config(&crypto), &ops);

    assert_eq!(result, Err("crypto failed".to_string()));
    let events = ops.events.borrow();
    assert_eq!(
        events.as_slice(),
        &[
            Event::Create,
            Event::Timeout(42, 1500),
            Event::EgressOpts(42, test_buffer_opts()),
            Event::ReuseAddr(42),
            Event::Crypto(42),
            Event::Close(42)
        ]
    );
}

#[test]
fn single_socket_connect_closes_socket_and_forgets_port_when_muxer_port_bind_fails() {
    let muxer_port = Mutex::new(Some(40000));
    let claim = super::super::claim_srt_egress_muxer_port(&muxer_port);
    let ops = FakeSingleConnectOps::failing(FailStep::Bind);

    let result = connect_single_srt_egress_socket_with(connect_config(Some(claim)), &ops);

    assert_eq!(result, Err("bind failed".to_string()));
    // libsrt frees a multiplexer's local UDP port once its last socket
    // closes, so a recorded port can stop being bindable while the shard
    // that learned it is still alive. Dropping the recording lets the next
    // attempt autoselect instead of failing this shard's connects forever.
    assert_eq!(
        *muxer_port.lock().unwrap(),
        None,
        "an unbindable recorded muxer port must not be retained"
    );
    let events = ops.events.borrow();
    assert_eq!(events.last(), Some(&Event::Close(42)));
    assert!(!events.contains(&Event::Connect(42, peer_addr())));
}

#[test]
fn single_socket_connect_after_a_failed_muxer_port_bind_records_a_freshly_autoselected_port() {
    // The recovery half of the test above, driven through the same
    // production entry point: with the stale recording cleared, the next
    // connect on this shard claims `First`, binds nothing, and records
    // whatever local port libsrt autoselected.
    let muxer_port = Mutex::new(Some(40000));
    let failing_claim = super::super::claim_srt_egress_muxer_port(&muxer_port);
    let failing_ops = FakeSingleConnectOps::failing(FailStep::Bind);
    let _ =
        connect_single_srt_egress_socket_with(connect_config(Some(failing_claim)), &failing_ops);

    let retry_claim = super::super::claim_srt_egress_muxer_port(&muxer_port);
    assert_eq!(retry_claim.bind_port(), None);
    let ops = FakeSingleConnectOps::new();
    let socket = connect_single_srt_egress_socket_with(connect_config(Some(retry_claim)), &ops);

    assert_eq!(socket, Ok(42));
    assert_eq!(*muxer_port.lock().unwrap(), Some(41000));
    assert!(
        !ops.events
            .borrow()
            .iter()
            .any(|event| matches!(event, Event::Bind(_, _)))
    );
}

#[test]
fn single_socket_connect_closes_socket_and_does_not_record_port_when_connect_fails() {
    let muxer_port = Mutex::new(None);
    let claim = super::super::claim_srt_egress_muxer_port(&muxer_port);
    let ops = FakeSingleConnectOps::failing(FailStep::Connect);

    let result = connect_single_srt_egress_socket_with(connect_config(Some(claim)), &ops);

    assert_eq!(result, Err("connect failed".to_string()));
    assert_eq!(*muxer_port.lock().unwrap(), None);
    let events = ops.events.borrow();
    assert!(events.contains(&Event::Connect(42, peer_addr())));
    assert_eq!(events.last(), Some(&Event::Close(42)));
}

#[test]
fn single_socket_connect_applies_fabric_send_mode_after_connect_and_closes_on_failure() {
    let ops = FakeSingleConnectOps::failing(FailStep::Configure);

    let result = connect_single_srt_egress_socket_with(fabric_connect_config(), &ops);

    assert_eq!(
        result,
        Err("failed to set SRTO_SNDSYN: configure failed (1234)".to_string())
    );
    let events = ops.events.borrow();
    assert_eq!(events.last(), Some(&Event::Close(42)));
    assert!(events.contains(&Event::Configure(42, SrtEgressSendMode::FabricNonblocking)));
    assert!(!events.contains(&Event::LocalPort(42)));
}
