use super::*;
use std::cell::RefCell;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    CreateGroup,
    Crypto(SRTSOCKET),
    StreamId(SRTSOCKET, String),
    Prepare(SocketAddr, bool),
    ConnectGroup(SRTSOCKET, Vec<FakeMember>),
    Configure(SRTSOCKET, SrtEgressSendMode),
    EgressOpts(SRTSOCKET, EgressBufferOpts),
    Log(SRTSOCKET),
    Close(SRTSOCKET),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailStep {
    CreateGroup,
    Crypto,
    StreamId,
    ConnectGroup,
    Configure,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FakeMember {
    peer_addr: SocketAddr,
    primary: bool,
}

struct FakeBondedConnectOps {
    events: RefCell<Vec<Event>>,
    fail_step: Option<FailStep>,
}

impl FakeBondedConnectOps {
    fn new() -> Self {
        Self {
            events: RefCell::new(Vec::new()),
            fail_step: None,
        }
    }

    fn failing(fail_step: FailStep) -> Self {
        Self {
            fail_step: Some(fail_step),
            ..Self::new()
        }
    }
}

impl SrtBondedConnectOps for &FakeBondedConnectOps {
    type Member = FakeMember;

    fn create_group(&mut self) -> Result<SRTSOCKET, String> {
        self.events.borrow_mut().push(Event::CreateGroup);
        if self.fail_step == Some(FailStep::CreateGroup) {
            return Err("failed to create bonding group".to_string());
        }
        Ok(42)
    }

    fn close(&mut self, socket: SRTSOCKET) {
        self.events.borrow_mut().push(Event::Close(socket));
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

    fn prepare_member(&mut self, peer_addr: SocketAddr, primary: bool) -> Self::Member {
        self.events
            .borrow_mut()
            .push(Event::Prepare(peer_addr, primary));
        FakeMember { peer_addr, primary }
    }

    fn connect_group(
        &mut self,
        socket: SRTSOCKET,
        members: &mut [Self::Member],
    ) -> Result<(), String> {
        self.events
            .borrow_mut()
            .push(Event::ConnectGroup(socket, members.to_vec()));
        if self.fail_step == Some(FailStep::ConnectGroup) {
            return Err("bonded connection failed: unavailable".to_string());
        }
        Ok(())
    }

    fn configure_connected_socket(
        &mut self,
        socket: SRTSOCKET,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError> {
        self.events
            .borrow_mut()
            .push(Event::Configure(socket, mode));
        if self.fail_step == Some(FailStep::Configure) {
            return Err(SrtEgressSocketError {
                option: "SRTO_SNDSYN",
                code: 1234,
                message: "configure failed".to_string(),
            });
        }
        Ok(())
    }

    fn set_egress_opts(&mut self, socket: SRTSOCKET, opts: &EgressBufferOpts) {
        self.events
            .borrow_mut()
            .push(Event::EgressOpts(socket, *opts));
    }

    fn log_effective_opts(&mut self, socket: SRTSOCKET) {
        self.events.borrow_mut().push(Event::Log(socket));
    }
}

fn test_buffer_opts() -> EgressBufferOpts {
    EgressBufferOpts::defaults(None).with_overrides(Some(6_250_000), None, None, None, None)
}

fn peer_addrs() -> Vec<SocketAddr> {
    vec![
        "127.0.0.1:9000".parse().unwrap(),
        "127.0.0.2:9001".parse().unwrap(),
    ]
}

fn connect_config<'a>(
    peer_addrs: &'a [SocketAddr],
    crypto: Option<&'a SrtCryptoConfig>,
) -> SrtBondedEgressConnectConfig<'a> {
    SrtBondedEgressConnectConfig {
        peer_addrs,
        stream_id: "publish:key",
        crypto,
        send_mode: SrtEgressSendMode::LegacyBlocking,
        buffer_opts: test_buffer_opts(),
    }
}

fn fabric_connect_config(peer_addrs: &[SocketAddr]) -> SrtBondedEgressConnectConfig<'_> {
    SrtBondedEgressConnectConfig {
        send_mode: SrtEgressSendMode::FabricNonblocking,
        ..connect_config(peer_addrs, None)
    }
}

#[test]
fn bonded_connect_prepares_primary_and_backup_members_before_connect() {
    let peer_addrs = peer_addrs();
    let ops = FakeBondedConnectOps::new();

    let socket =
        connect_bonded_srt_egress_socket_with(connect_config(&peer_addrs, None), &ops).unwrap();

    assert_eq!(socket, 42);
    assert_eq!(
        ops.events.borrow().as_slice(),
        &[
            Event::CreateGroup,
            Event::StreamId(42, "publish:key".to_string()),
            Event::Prepare(peer_addrs[0], true),
            Event::Prepare(peer_addrs[1], false),
            Event::ConnectGroup(
                42,
                vec![
                    FakeMember {
                        peer_addr: peer_addrs[0],
                        primary: true,
                    },
                    FakeMember {
                        peer_addr: peer_addrs[1],
                        primary: false,
                    },
                ],
            ),
            Event::Configure(42, SrtEgressSendMode::LegacyBlocking),
            Event::EgressOpts(42, test_buffer_opts()),
            Event::Log(42),
        ]
    );
}

#[test]
fn bonded_connect_applies_crypto_before_stream_id() {
    let peer_addrs = peer_addrs();
    let crypto =
        super::super::super::srt_crypto::srt_crypto_from_url("secret".to_string(), None).unwrap();
    let ops = FakeBondedConnectOps::new();

    let socket =
        connect_bonded_srt_egress_socket_with(connect_config(&peer_addrs, Some(&crypto)), &ops)
            .unwrap();

    assert_eq!(socket, 42);
    assert_eq!(
        &ops.events.borrow()[..3],
        &[
            Event::CreateGroup,
            Event::Crypto(42),
            Event::StreamId(42, "publish:key".to_string()),
        ]
    );
}

#[test]
fn bonded_connect_returns_create_error_without_close_when_group_creation_fails() {
    let peer_addrs = peer_addrs();
    let ops = FakeBondedConnectOps::failing(FailStep::CreateGroup);

    let result = connect_bonded_srt_egress_socket_with(connect_config(&peer_addrs, None), &ops);

    assert_eq!(result, Err("failed to create bonding group".to_string()));
    assert_eq!(ops.events.borrow().as_slice(), &[Event::CreateGroup]);
}

#[test]
fn bonded_connect_closes_group_when_crypto_fails() {
    let peer_addrs = peer_addrs();
    let crypto =
        super::super::super::srt_crypto::srt_crypto_from_url("secret".to_string(), None).unwrap();
    let ops = FakeBondedConnectOps::failing(FailStep::Crypto);

    let result =
        connect_bonded_srt_egress_socket_with(connect_config(&peer_addrs, Some(&crypto)), &ops);

    assert_eq!(result, Err("crypto failed".to_string()));
    assert_eq!(ops.events.borrow().last(), Some(&Event::Close(42)));
    assert!(
        !ops.events
            .borrow()
            .iter()
            .any(|event| matches!(event, Event::StreamId(..)))
    );
}

#[test]
fn bonded_connect_closes_group_when_stream_id_fails() {
    let peer_addrs = peer_addrs();
    let ops = FakeBondedConnectOps::failing(FailStep::StreamId);

    let result = connect_bonded_srt_egress_socket_with(connect_config(&peer_addrs, None), &ops);

    assert_eq!(result, Err("stream id failed".to_string()));
    assert_eq!(ops.events.borrow().last(), Some(&Event::Close(42)));
    assert!(
        !ops.events
            .borrow()
            .iter()
            .any(|event| matches!(event, Event::ConnectGroup(..)))
    );
}

#[test]
fn bonded_connect_closes_group_when_connect_fails() {
    let peer_addrs = peer_addrs();
    let ops = FakeBondedConnectOps::failing(FailStep::ConnectGroup);

    let result = connect_bonded_srt_egress_socket_with(connect_config(&peer_addrs, None), &ops);

    assert_eq!(
        result,
        Err("bonded connection failed: unavailable".to_string())
    );
    assert_eq!(ops.events.borrow().last(), Some(&Event::Close(42)));
}

#[test]
fn bonded_connect_applies_fabric_send_mode_after_connect_and_closes_on_failure() {
    let peer_addrs = peer_addrs();
    let ops = FakeBondedConnectOps::failing(FailStep::Configure);

    let result = connect_bonded_srt_egress_socket_with(fabric_connect_config(&peer_addrs), &ops);

    assert_eq!(
        result,
        Err("failed to set SRTO_SNDSYN: configure failed (1234)".to_string())
    );
    assert_eq!(ops.events.borrow().last(), Some(&Event::Close(42)));
    assert!(
        ops.events
            .borrow()
            .contains(&Event::Configure(42, SrtEgressSendMode::FabricNonblocking))
    );
}
