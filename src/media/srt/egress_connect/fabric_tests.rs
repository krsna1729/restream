use super::*;
use std::cell::RefCell;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq)]
enum Event {
    Single {
        peer_addr: SocketAddr,
        stream_id: String,
        has_crypto: bool,
        connect_timeout_ms: u64,
        send_mode: SrtEgressSendMode,
        has_muxer_port_claim: bool,
    },
    Bonded {
        peer_addrs: Vec<SocketAddr>,
        stream_id: String,
        has_crypto: bool,
        send_mode: SrtEgressSendMode,
    },
}

#[derive(Default)]
struct FakeFabricConnectOps {
    events: RefCell<Vec<Event>>,
    single_error: Option<String>,
    bonded_error: Option<String>,
}

impl FakeFabricConnectOps {
    fn fail_single(error: &str) -> Self {
        Self {
            single_error: Some(error.to_string()),
            ..Self::default()
        }
    }

    fn fail_bonded(error: &str) -> Self {
        Self {
            bonded_error: Some(error.to_string()),
            ..Self::default()
        }
    }
}

impl SrtFabricConnectOps for &FakeFabricConnectOps {
    fn connect_single(
        &mut self,
        config: SrtSingleEgressConnectConfig<'_>,
    ) -> Result<SRTSOCKET, String> {
        self.events.borrow_mut().push(Event::Single {
            peer_addr: config.peer_addr,
            stream_id: config.stream_id.to_string(),
            has_crypto: config.crypto.is_some(),
            connect_timeout_ms: config.connect_timeout_ms,
            send_mode: config.send_mode,
            has_muxer_port_claim: config.muxer_port_claim.is_some(),
        });
        self.single_error.clone().map_or(Ok(42), Err)
    }

    fn connect_bonded(
        &mut self,
        config: SrtBondedEgressConnectConfig<'_>,
    ) -> Result<SRTSOCKET, String> {
        self.events.borrow_mut().push(Event::Bonded {
            peer_addrs: config.peer_addrs.to_vec(),
            stream_id: config.stream_id.to_string(),
            has_crypto: config.crypto.is_some(),
            send_mode: config.send_mode,
        });
        self.bonded_error.clone().map_or(Ok(84), Err)
    }
}

fn peer_addrs() -> Vec<SocketAddr> {
    vec![
        "127.0.0.1:9000".parse().unwrap(),
        "127.0.0.2:9001".parse().unwrap(),
    ]
}

fn fabric_config<'a>(
    peer_addrs: &'a [SocketAddr],
    crypto: Option<&'a SrtCryptoConfig>,
    muxer_port_claim: Option<SrtEgressMuxerPortClaim<'a>>,
) -> SrtFabricEgressConnectConfig<'a> {
    SrtFabricEgressConnectConfig::new(
        peer_addrs,
        "publish:key",
        crypto,
        1500,
        muxer_port_claim,
        EgressBufferOpts::defaults(None),
    )
}

#[test]
fn fabric_connect_uses_single_nonblocking_socket_for_one_peer() {
    let peer_addrs = peer_addrs();
    let crypto =
        super::super::super::srt_crypto::srt_crypto_from_url("secret".to_string(), None).unwrap();
    let muxer_port = Mutex::new(None);
    let muxer_port_claim = super::super::claim_srt_egress_muxer_port(&muxer_port);
    let ops = FakeFabricConnectOps::default();

    let socket = connect_fabric_srt_egress_socket_with(
        fabric_config(&peer_addrs[..1], Some(&crypto), Some(muxer_port_claim)),
        &ops,
    )
    .unwrap();

    assert_eq!(socket, 42);
    assert_eq!(
        ops.events.borrow().as_slice(),
        &[Event::Single {
            peer_addr: peer_addrs[0],
            stream_id: "publish:key".to_string(),
            has_crypto: true,
            connect_timeout_ms: 1500,
            send_mode: SrtEgressSendMode::FabricNonblocking,
            has_muxer_port_claim: true,
        }]
    );
}

#[test]
fn fabric_connect_uses_bonded_nonblocking_socket_for_multiple_peers() {
    let peer_addrs = peer_addrs();
    let ops = FakeFabricConnectOps::default();

    let socket =
        connect_fabric_srt_egress_socket_with(fabric_config(&peer_addrs, None, None), &ops)
            .unwrap();

    assert_eq!(socket, 84);
    assert_eq!(
        ops.events.borrow().as_slice(),
        &[Event::Bonded {
            peer_addrs,
            stream_id: "publish:key".to_string(),
            has_crypto: false,
            send_mode: SrtEgressSendMode::FabricNonblocking,
        }]
    );
}

#[test]
fn fabric_connect_rejects_empty_peer_list_without_opening_socket() {
    let ops = FakeFabricConnectOps::default();

    let result = connect_fabric_srt_egress_socket_with(fabric_config(&[], None, None), &ops);

    assert_eq!(
        result,
        Err("fabric SRT connect requires at least one peer address".to_string())
    );
    assert!(ops.events.borrow().is_empty());
}

#[test]
fn fabric_connect_returns_single_connect_error() {
    let peer_addrs = peer_addrs();
    let ops = FakeFabricConnectOps::fail_single("single failed");

    let result =
        connect_fabric_srt_egress_socket_with(fabric_config(&peer_addrs[..1], None, None), &ops);

    assert_eq!(result, Err("single failed".to_string()));
    assert!(matches!(
        ops.events.borrow().as_slice(),
        [Event::Single { .. }]
    ));
}

#[test]
fn fabric_connect_returns_bonded_connect_error() {
    let peer_addrs = peer_addrs();
    let ops = FakeFabricConnectOps::fail_bonded("bonded failed");

    let result =
        connect_fabric_srt_egress_socket_with(fabric_config(&peer_addrs, None, None), &ops);

    assert_eq!(result, Err("bonded failed".to_string()));
    assert!(matches!(
        ops.events.borrow().as_slice(),
        [Event::Bonded { .. }]
    ));
}
