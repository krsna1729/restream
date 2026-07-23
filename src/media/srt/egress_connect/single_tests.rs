use super::*;
use std::cell::RefCell;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Create,
    Timeout(SRTSOCKET, u64),
    HighBitrate(SRTSOCKET),
    ReuseAddr(SRTSOCKET),
    Crypto(SRTSOCKET),
    StreamId(SRTSOCKET, String),
    Bind(SRTSOCKET, u16),
    Connect(SRTSOCKET, SocketAddr),
    LocalPort(SRTSOCKET),
    Log(SRTSOCKET),
    Close(SRTSOCKET),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailStep {
    ReuseAddr,
    Crypto,
    StreamId,
    Bind,
    Connect,
}

struct FakeSingleConnectOps {
    events: RefCell<Vec<Event>>,
    fail_step: Option<FailStep>,
    socket: SRTSOCKET,
    local_port: u16,
}

impl FakeSingleConnectOps {
    fn new() -> Self {
        Self {
            events: RefCell::new(Vec::new()),
            fail_step: None,
            socket: 42,
            local_port: 41000,
        }
    }

    fn failing(fail_step: FailStep) -> Self {
        Self {
            fail_step: Some(fail_step),
            ..Self::new()
        }
    }
}

impl SrtSingleConnectOps for &FakeSingleConnectOps {
    fn create_socket(&mut self) -> Result<SRTSOCKET, String> {
        self.events.borrow_mut().push(Event::Create);
        Ok(self.socket)
    }

    fn close(&mut self, socket: SRTSOCKET) {
        self.events.borrow_mut().push(Event::Close(socket));
    }

    fn set_connect_timeout(&mut self, socket: SRTSOCKET, timeout_ms: u64) {
        self.events
            .borrow_mut()
            .push(Event::Timeout(socket, timeout_ms));
    }

    fn set_highbitrate_opts(&mut self, socket: SRTSOCKET) {
        self.events.borrow_mut().push(Event::HighBitrate(socket));
    }

    fn set_reuseaddr(&mut self, socket: SRTSOCKET) -> Result<(), String> {
        self.events.borrow_mut().push(Event::ReuseAddr(socket));
        if self.fail_step == Some(FailStep::ReuseAddr) {
            return Err("reuseaddr failed".to_string());
        }
        Ok(())
    }

    fn apply_crypto(&mut self, socket: SRTSOCKET, _crypto: &SrtCryptoConfig) -> Result<(), String> {
        self.events.borrow_mut().push(Event::Crypto(socket));
        if self.fail_step == Some(FailStep::Crypto) {
            return Err("crypto failed".to_string());
        }
        Ok(())
    }

    fn apply_stream_id(&mut self, socket: SRTSOCKET, stream_id: &str) -> Result<(), String> {
        self.events
            .borrow_mut()
            .push(Event::StreamId(socket, stream_id.to_string()));
        if self.fail_step == Some(FailStep::StreamId) {
            return Err("stream id failed".to_string());
        }
        Ok(())
    }

    fn bind_muxer_port(&mut self, socket: SRTSOCKET, port: u16) -> Result<(), String> {
        self.events.borrow_mut().push(Event::Bind(socket, port));
        if self.fail_step == Some(FailStep::Bind) {
            return Err("bind failed".to_string());
        }
        Ok(())
    }

    fn connect(&mut self, socket: SRTSOCKET, peer_addr: SocketAddr) -> Result<(), String> {
        self.events
            .borrow_mut()
            .push(Event::Connect(socket, peer_addr));
        if self.fail_step == Some(FailStep::Connect) {
            return Err("connect failed".to_string());
        }
        Ok(())
    }

    fn connected_local_port(&mut self, socket: SRTSOCKET) -> Result<u16, String> {
        self.events.borrow_mut().push(Event::LocalPort(socket));
        Ok(self.local_port)
    }

    fn log_effective_opts(&mut self, socket: SRTSOCKET) {
        self.events.borrow_mut().push(Event::Log(socket));
    }
}

fn peer_addr() -> SocketAddr {
    "127.0.0.1:9000".parse().unwrap()
}

fn connect_config<'a>(
    muxer_port_claim: Option<SrtEgressMuxerPortClaim<'a>>,
) -> SrtSingleEgressConnectConfig<'a> {
    SrtSingleEgressConnectConfig {
        peer_addr: peer_addr(),
        stream_id: "publish:key",
        crypto: None,
        connect_timeout_ms: 1500,
        muxer_port_claim,
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
            Event::HighBitrate(42),
            Event::ReuseAddr(42),
            Event::StreamId(42, "publish:key".to_string()),
            Event::Connect(42, peer_addr()),
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
            Event::HighBitrate(42),
            Event::ReuseAddr(42),
            Event::StreamId(42, "publish:key".to_string()),
            Event::Bind(42, 40000),
            Event::Connect(42, peer_addr()),
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
            Event::HighBitrate(42),
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
            Event::HighBitrate(42),
            Event::ReuseAddr(42),
            Event::Crypto(42),
            Event::Close(42)
        ]
    );
}

#[test]
fn single_socket_connect_closes_socket_when_muxer_port_bind_fails() {
    let muxer_port = Mutex::new(Some(40000));
    let claim = super::super::claim_srt_egress_muxer_port(&muxer_port);
    let ops = FakeSingleConnectOps::failing(FailStep::Bind);

    let result = connect_single_srt_egress_socket_with(connect_config(Some(claim)), &ops);

    assert_eq!(result, Err("bind failed".to_string()));
    assert_eq!(*muxer_port.lock().unwrap(), Some(40000));
    let events = ops.events.borrow();
    assert_eq!(events.last(), Some(&Event::Close(42)));
    assert!(!events.contains(&Event::Connect(42, peer_addr())));
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
