//! Regression coverage for `SrtFabricEgressConnectSpec::connect_config`
//! (`docs/archive/egress/implementation.md` Phase 4 status): shared callers
//! receive the same per-shard application-owned UDP socket/table state.

use super::super::*;
use crate::media::egress::policy::WorkBudget;
use bytes::Bytes;
use std::time::Duration;

fn peer_addrs() -> Vec<std::net::SocketAddr> {
    vec!["127.0.0.1:9000".parse().unwrap()]
}

fn connect_spec() -> SrtFabricEgressConnectSpec {
    SrtFabricEgressConnectSpec::from_url("srt://primary:9000?streamid=publish%3Akey", 30000)
}

#[test]
fn connect_config_with_reuse_disabled_passes_no_shared_state() {
    let peer_addrs = peer_addrs();
    let config = connect_spec().connect_config(&peer_addrs, None);

    assert!(!config.has_muxer_port_claim());
    assert_eq!(config.muxer_port_claim_bind_port(), None);
}

#[test]
fn connect_config_with_reuse_enabled_passes_shared_state() {
    let peer_addrs = peer_addrs();
    let state = std::sync::Arc::new(std::sync::Mutex::new(None));
    let config = connect_spec().connect_config(&peer_addrs, Some(state.clone()));

    assert!(config.has_muxer_port_claim());
    assert_eq!(config.muxer_port_claim_bind_port(), None);
    assert!(state.lock().unwrap().is_none());
}

#[test]
fn complete_pending_connect_with_reuse_enabled_keeps_state_lazy() {
    let peer_addrs = peer_addrs();
    let state = std::sync::Arc::new(std::sync::Mutex::new(None));
    let mut backend = SrtShardBackend::new(
        super::support::feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    )
    .with_srt_egress_muxer_port_reuse(state.clone(), true);
    backend.on_command(crate::media::egress::command::EgressCommand::Add(
        crate::media::egress::command::OutputSpec {
            id: crate::media::egress::command::OutputId::new("out-a"),
            generation: 7,
            feed: crate::media::egress::command::FeedId::new("feed-srt"),
            protocol: crate::media::egress::command::ProtocolSpec::Srt {
                url: "srt://primary:9000?streamid=publish%3Akey".to_string(),
            },
            policy: crate::media::egress::policy::LeafPolicy::default(),
            progress: Default::default(),
        },
    ));

    backend
        .complete_pending_connect(
            &crate::media::egress::command::OutputId::new("out-a"),
            7,
            &peer_addrs,
        )
        .unwrap();

    assert!(state.lock().unwrap().is_none());
}
