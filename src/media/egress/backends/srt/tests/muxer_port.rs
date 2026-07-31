//! Regression coverage for `SrtShardBackend::with_srt_egress_muxer_port_reuse`
//! (`docs/egress-implementation.md` Phase 4 status): the fabric path used to
//! always pass `None` for the libsrt egress-multiplexer local-port claim,
//! losing the ~1.25-core saving the legacy path gets from sharing one local
//! UDP port across compatible SRT egress sockets. These tests prove the
//! claim actually reaches `connect_config` once reuse is enabled, and that
//! it stays absent when reuse is left off (the default for every other
//! backend constructor/test).

use super::super::*;
use super::support::{FakeReadinessPoller, FakeSocketConfigurator, FakeSocketConnector, feed};
use crate::media::egress::command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::policy::{LeafPolicy, WorkBudget};
use bytes::Bytes;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn peer_addrs() -> Vec<std::net::SocketAddr> {
    vec!["127.0.0.1:9000".parse().unwrap()]
}

fn output_spec(id: &str, generation: u64) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation,
        feed: FeedId::new("feed-srt"),
        protocol: ProtocolSpec::Srt {
            url: "srt://primary:9000?streamid=publish%3Akey".to_string(),
        },
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

fn backend_with_pending_connect(
    reuse_state: Option<(Arc<Mutex<Option<u16>>>, bool)>,
) -> SrtShardBackend<FakeReadinessPoller, FakeSocketConfigurator> {
    let poller = FakeReadinessPoller::default();
    let configurator = FakeSocketConfigurator::default();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
    );
    if let Some((state, enabled)) = reuse_state {
        backend = backend.with_srt_egress_muxer_port_reuse(state, enabled);
    }
    backend.on_command(EgressCommand::Add(output_spec("out-a", 7)));
    backend
}

#[test]
fn complete_pending_connect_with_reuse_disabled_passes_no_muxer_port_claim() {
    let peer_addrs = peer_addrs();
    let mut backend = backend_with_pending_connect(None);
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();

    backend
        .complete_pending_connect_with(&OutputId::new("out-a"), 7, &peer_addrs, connector)
        .unwrap();

    let claims = connector_handle.muxer_port_claims();
    assert_eq!(claims, vec![(false, None)]);
}

#[test]
fn complete_pending_connect_with_reuse_enabled_and_empty_state_claims_first() {
    let peer_addrs = peer_addrs();
    let state = Arc::new(Mutex::new(None));
    let mut backend = backend_with_pending_connect(Some((state, true)));
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();

    backend
        .complete_pending_connect_with(&OutputId::new("out-a"), 7, &peer_addrs, connector)
        .unwrap();

    // A claim is present, but with nothing recorded yet in the shared state
    // this is the `First` variant: there is no port to bind to yet (that
    // only happens once a real connect records the port it landed on, in
    // `connect_single_srt_egress_socket_with` — outside what a fake
    // connector exercises).
    let claims = connector_handle.muxer_port_claims();
    assert_eq!(claims, vec![(true, None)]);
}

#[test]
fn complete_pending_connect_with_reuse_enabled_and_recorded_port_claims_reuse() {
    let peer_addrs = peer_addrs();
    let state = Arc::new(Mutex::new(Some(9000)));
    let mut backend = backend_with_pending_connect(Some((state, true)));
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();

    backend
        .complete_pending_connect_with(&OutputId::new("out-a"), 7, &peer_addrs, connector)
        .unwrap();

    let claims = connector_handle.muxer_port_claims();
    assert_eq!(claims, vec![(true, Some(9000))]);
}

#[test]
fn complete_pending_connect_uses_the_socket_connector_field_and_the_same_claim_wiring() {
    // `complete_pending_connect` (the non-`_with` sibling, driving
    // `self.socket_connector` instead of an injected connector) has its own
    // copy of the same claim-construction logic — covered separately since
    // it is a distinct call site in `src/media/egress/backends/srt.rs`.
    let peer_addrs = peer_addrs();
    let poller = FakeReadinessPoller::default();
    let configurator = FakeSocketConfigurator::default();
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let state = Arc::new(Mutex::new(Some(9100)));
    let mut backend = SrtShardBackend::with_runtime_components(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
        connector,
        NoopSrtResolveCompletionSource,
    )
    .with_srt_egress_muxer_port_reuse(state, true);
    backend.on_command(EgressCommand::Add(output_spec("out-a", 7)));

    backend
        .complete_pending_connect(&OutputId::new("out-a"), 7, &peer_addrs)
        .unwrap();

    let claims = connector_handle.muxer_port_claims();
    assert_eq!(claims, vec![(true, Some(9100))]);
}
