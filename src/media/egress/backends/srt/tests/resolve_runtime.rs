use super::super::resolve_runtime::{ResolvingSrtShardBackend, SrtResolveWorkerSet};
use super::super::*;
use super::support::{
    FakeConnectCall, FakeReadinessPoller, FakeSocketConfigurator, FakeSocketConnector, feed,
};
use crate::media::egress::command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::policy::{LeafPolicy, WorkBudget};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::srt::{SrtEgressInterest, SrtEgressSendMode};
use bytes::Bytes;
use std::thread;
use std::time::Duration;

fn output_spec(id: &str, generation: u64, protocol: ProtocolSpec) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation,
        feed: FeedId::new("feed-srt"),
        protocol,
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

#[test]
fn resolving_srt_backend_spawns_resolver_and_completes_add() {
    let (completion_sender, completion_queue) = srt_resolve_completion_queue(4);
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::default();
    let configurator_handle = configurator.clone();
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let inner = SrtShardBackend::with_runtime_components(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
        connector,
        completion_queue,
    );
    let mut backend =
        ResolvingSrtShardBackend::new(inner, SrtResolveWorkerSet::new(completion_sender));

    let effect = backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://127.0.0.1:9000?streamid=publish%3Akey&bond=127.0.0.2:9001".to_string(),
        },
    )));

    assert_eq!(effect, EgressShardCommandEffect::Continue);
    for _ in 0..50 {
        backend.on_media_tick();
        if !connector_handle.calls().is_empty() {
            break;
        }
        thread::sleep(Duration::from_millis(1));
    }
    assert_eq!(
        connector_handle.calls(),
        vec![FakeConnectCall {
            peer_addrs: vec![
                "127.0.0.1:9000".parse().unwrap(),
                "127.0.0.2:9001".parse().unwrap(),
            ],
            stream_id: "publish:key".to_string(),
            connect_timeout_ms: 10000,
        }]
    );
    assert_eq!(
        configurator_handle.calls(),
        vec![(42, SrtEgressSendMode::FabricNonblocking)]
    );
    assert_eq!(
        poller_handle.registered(),
        vec![(
            42,
            crate::media::egress::scheduler::LeafKey(0),
            7,
            SrtEgressInterest::WRITE
        )]
    );
    assert_eq!(backend.worker_count(), 0);
}

#[test]
fn resolving_srt_backend_does_not_spawn_for_non_srt_add() {
    let (completion_sender, completion_queue) = srt_resolve_completion_queue(4);
    let inner = SrtShardBackend::with_runtime_components(
        FakeReadinessPoller::default(),
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
        FakeSocketConnector::returning(42),
        completion_queue,
    );
    let mut backend =
        ResolvingSrtShardBackend::new(inner, SrtResolveWorkerSet::new(completion_sender));

    let effect = backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Sink,
    )));

    assert_eq!(effect, EgressShardCommandEffect::Continue);
    backend.on_media_tick();
    assert_eq!(backend.worker_count(), 0);
}
