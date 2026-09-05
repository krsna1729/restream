//! Regression coverage for `SrtShardBackend::with_srt_egress_muxer_port_reuse`
//! (`docs/egress-implementation.md` Phase 4 status): shared callers receive
//! the same per-shard application-owned UDP socket/table state.
//!
//! These drive `complete_pending_connect` — the same function production
//! uses — with a fake supplied through the backend's own connector type
//! parameter, so there is no second claim-construction path to cover.

use super::super::*;
use super::support::{FakeSocketConnector, feed};
use crate::media::egress::command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::policy::{LeafPolicy, WorkBudget};
use bytes::Bytes;
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
    connector: FakeSocketConnector,
    reuse_state: Option<(muxer_ports::SrtEgressMuxerPortState, bool)>,
) -> SrtShardBackend<FakeSocketConnector, NoopSrtResolveCompletionSource> {
    let mut backend = SrtShardBackend::with_runtime_components(
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        connector,
        NoopSrtResolveCompletionSource,
    );
    if let Some((state, enabled)) = reuse_state {
        backend = backend.with_srt_egress_muxer_port_reuse(state, enabled);
    }
    backend.on_command(EgressCommand::Add(output_spec("out-a", 7)));
    backend
}

#[test]
fn complete_pending_connect_with_reuse_disabled_passes_no_shared_state() {
    let peer_addrs = peer_addrs();
    let connector = FakeSocketConnector::returning();
    let connector_handle = connector.clone();
    let mut backend = backend_with_pending_connect(connector, None);

    backend
        .complete_pending_connect(&OutputId::new("out-a"), 7, &peer_addrs)
        .unwrap();

    let claims = connector_handle.muxer_port_claims();
    assert_eq!(claims, vec![(false, None)]);
}

#[test]
fn complete_pending_connect_with_reuse_enabled_passes_shared_state() {
    let peer_addrs = peer_addrs();
    let state = std::sync::Arc::new(std::sync::Mutex::new(None));
    let connector = FakeSocketConnector::returning();
    let connector_handle = connector.clone();
    let mut backend = backend_with_pending_connect(connector, Some((state.clone(), true)));

    backend
        .complete_pending_connect(&OutputId::new("out-a"), 7, &peer_addrs)
        .unwrap();

    let claims = connector_handle.muxer_port_claims();
    assert_eq!(claims, vec![(true, None)]);
}

#[test]
fn complete_pending_connect_with_reuse_enabled_keeps_state_lazy() {
    let peer_addrs = peer_addrs();
    let state = std::sync::Arc::new(std::sync::Mutex::new(None));
    let connector = FakeSocketConnector::returning();
    let connector_handle = connector.clone();
    let mut backend = backend_with_pending_connect(connector, Some((state.clone(), true)));

    backend
        .complete_pending_connect(&OutputId::new("out-a"), 7, &peer_addrs)
        .unwrap();

    let claims = connector_handle.muxer_port_claims();
    assert_eq!(claims, vec![(true, None)]);
    assert!(state.lock().unwrap().is_none());
}
