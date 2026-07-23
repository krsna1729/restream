use super::super::*;
use super::support::{
    FakeConnectCall, FakeReadinessPoller, FakeResolveCompletionSource, FakeSocketConfigurator,
    FakeSocketConnector, common, feed, shared_sender, shared_sender_recording,
};
use crate::media::egress::command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::policy::{LeafPolicy, WorkBudget};
use crate::media::egress::scheduler::LeafKey;
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::srt::{
    SrtEgressInterest, SrtEgressSendMode, SrtFabricEgressConnectConfig, SrtReadyLeaf,
};
use bytes::Bytes;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn fabric_connect_config(peer_addrs: &[std::net::SocketAddr]) -> SrtFabricEgressConnectConfig<'_> {
    SrtFabricEgressConnectConfig::plaintext(peer_addrs, "publish:key", 1500, None)
}

fn peer_addrs() -> Vec<std::net::SocketAddr> {
    vec![
        "127.0.0.1:9000".parse().unwrap(),
        "127.0.0.2:9001".parse().unwrap(),
    ]
}

fn output_spec(id: &str, generation: u64, protocol: ProtocolSpec) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation,
        feed: FeedId::new("feed-srt"),
        protocol,
        policy: LeafPolicy::default(),
    }
}

#[test]
fn srt_shard_backend_configures_connected_socket_for_fabric_nonblocking() {
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::default();
    let configurator_handle = configurator.clone();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
    );

    let key = backend.add_connected_socket(common(7), 42).unwrap();

    assert_eq!(key, LeafKey(0));
    assert_eq!(
        configurator_handle.calls(),
        vec![(42, SrtEgressSendMode::FabricNonblocking)]
    );
    assert_eq!(
        poller_handle.registered(),
        vec![(42, LeafKey(0), 7, SrtEgressInterest::WRITE)]
    );
}

#[test]
fn srt_shard_backend_add_resolved_socket_connects_and_registers_leaf() {
    let peer_addrs = peer_addrs();
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::default();
    let configurator_handle = configurator.clone();
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
    );

    let key = backend
        .add_resolved_socket_with(common(7), fabric_connect_config(&peer_addrs), connector)
        .unwrap();

    assert_eq!(key, LeafKey(0));
    assert_eq!(
        connector_handle.calls(),
        vec![FakeConnectCall {
            peer_addrs,
            stream_id: "publish:key".to_string(),
            connect_timeout_ms: 1500,
        }]
    );
    assert_eq!(
        configurator_handle.calls(),
        vec![(42, SrtEgressSendMode::FabricNonblocking)]
    );
    assert_eq!(
        poller_handle.registered(),
        vec![(42, LeafKey(0), 7, SrtEgressInterest::WRITE)]
    );
}

#[test]
fn srt_shard_backend_add_resolved_socket_returns_connect_error_before_registering() {
    let peer_addrs = peer_addrs();
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::default();
    let configurator_handle = configurator.clone();
    let connector = FakeSocketConnector::failing("connect failed");
    let connector_handle = connector.clone();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
    );

    let result =
        backend.add_resolved_socket_with(common(7), fabric_connect_config(&peer_addrs), connector);

    assert_eq!(
        result,
        Err(SrtBackendConnectError::Connect(
            "connect failed".to_string()
        ))
    );
    assert_eq!(connector_handle.calls().len(), 1);
    assert!(configurator_handle.calls().is_empty());
    assert!(poller_handle.registered().is_empty());
}

#[test]
fn srt_shard_backend_add_resolved_socket_returns_add_error_after_connect() {
    let peer_addrs = peer_addrs();
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::failing();
    let configurator_handle = configurator.clone();
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
    );

    let result =
        backend.add_resolved_socket_with(common(7), fabric_connect_config(&peer_addrs), connector);

    assert!(matches!(result, Err(SrtBackendConnectError::Add(_))));
    assert_eq!(connector_handle.calls().len(), 1);
    assert_eq!(
        configurator_handle.calls(),
        vec![(42, SrtEgressSendMode::FabricNonblocking)]
    );
    assert!(poller_handle.registered().is_empty());
}

#[test]
fn srt_shard_backend_add_srt_command_queues_pending_connect() {
    let mut backend = SrtShardBackend::new(
        FakeReadinessPoller::default(),
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );

    let effect = backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://primary:9000?streamid=publish%3Akey&bond=backup:9001".to_string(),
        },
    )));

    let pending = backend
        .pending_connect(&OutputId::new("out-a"))
        .expect("SRT Add must queue a pending connect");
    assert_eq!(effect, EgressShardCommandEffect::Continue);
    assert_eq!(pending.common.output_id.as_str(), "out-a");
    assert_eq!(pending.common.generation, 7);
    assert_eq!(
        pending.connect_spec.peer_hosts(),
        &["primary:9000".to_string(), "backup:9001".to_string()]
    );
    assert_eq!(pending.connect_spec.stream_id(), "publish:key");
}

#[test]
fn srt_shard_backend_ignores_non_srt_add_command() {
    let mut backend = SrtShardBackend::new(
        FakeReadinessPoller::default(),
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );

    let effect = backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Sink,
    )));

    assert_eq!(effect, EgressShardCommandEffect::Continue);
    assert!(backend.pending_connect(&OutputId::new("out-a")).is_none());
}

#[test]
fn srt_shard_backend_update_srt_command_replaces_pending_connect() {
    let mut backend = SrtShardBackend::new(
        FakeReadinessPoller::default(),
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://old:9000?streamid=publish:old".to_string(),
        },
    )));

    backend.on_command(EgressCommand::Update(output_spec(
        "out-a",
        8,
        ProtocolSpec::Srt {
            url: "srt://new:9000?streamid=publish:new".to_string(),
        },
    )));

    let pending = backend
        .pending_connect(&OutputId::new("out-a"))
        .expect("SRT Update must replace pending connect");
    assert_eq!(pending.common.generation, 8);
    assert_eq!(pending.connect_spec.peer_hosts(), &["new:9000".to_string()]);
    assert_eq!(pending.connect_spec.stream_id(), "publish:new");
}

#[test]
fn srt_shard_backend_remove_command_clears_pending_connect() {
    let mut backend = SrtShardBackend::new(
        FakeReadinessPoller::default(),
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://primary:9000?streamid=publish:key".to_string(),
        },
    )));

    let effect = backend.on_command(EgressCommand::Remove(OutputId::new("out-a")));

    assert_eq!(effect, EgressShardCommandEffect::Continue);
    assert!(backend.pending_connect(&OutputId::new("out-a")).is_none());
}

#[test]
fn srt_shard_backend_complete_pending_connect_registers_resolved_socket() {
    let peer_addrs = peer_addrs();
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::default();
    let configurator_handle = configurator.clone();
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
    );
    backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://primary:9000?streamid=publish%3Akey".to_string(),
        },
    )));

    let key = backend
        .complete_pending_connect_with(&OutputId::new("out-a"), 7, &peer_addrs, connector)
        .unwrap();

    assert_eq!(key, LeafKey(0));
    assert!(backend.pending_connect(&OutputId::new("out-a")).is_none());
    assert_eq!(
        connector_handle.calls(),
        vec![FakeConnectCall {
            peer_addrs,
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
        vec![(42, LeafKey(0), 7, SrtEgressInterest::WRITE)]
    );
}

#[test]
fn srt_shard_backend_complete_pending_connect_rejects_stale_generation() {
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let mut backend = SrtShardBackend::new(
        FakeReadinessPoller::default(),
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://primary:9000?streamid=publish:key".to_string(),
        },
    )));

    let result =
        backend.complete_pending_connect_with(&OutputId::new("out-a"), 6, &peer_addrs(), connector);

    assert_eq!(result, Err(SrtPendingConnectError::Stale));
    assert!(backend.pending_connect(&OutputId::new("out-a")).is_some());
    assert!(connector_handle.calls().is_empty());
}

#[test]
fn srt_shard_backend_complete_pending_connect_rejects_missing_output() {
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let mut backend = SrtShardBackend::new(
        FakeReadinessPoller::default(),
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );

    let result = backend.complete_pending_connect_with(
        &OutputId::new("out-missing"),
        7,
        &peer_addrs(),
        connector,
    );

    assert_eq!(result, Err(SrtPendingConnectError::Missing));
    assert!(connector_handle.calls().is_empty());
}

#[test]
fn srt_shard_backend_media_tick_completes_resolved_connect() {
    let peer_addrs = peer_addrs();
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::default();
    let configurator_handle = configurator.clone();
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let mut backend = SrtShardBackend::with_runtime_components(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
        connector,
        FakeResolveCompletionSource::with(vec![SrtResolvedConnect {
            output_id: OutputId::new("out-a"),
            generation: 7,
            peer_addrs: peer_addrs.clone(),
        }]),
    );
    backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://primary:9000?streamid=publish%3Akey".to_string(),
        },
    )));

    backend.on_media_tick();

    assert!(backend.pending_connect(&OutputId::new("out-a")).is_none());
    assert_eq!(
        connector_handle.calls(),
        vec![FakeConnectCall {
            peer_addrs,
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
        vec![(42, LeafKey(0), 7, SrtEgressInterest::WRITE)]
    );
}

#[test]
fn srt_resolve_completion_queue_drains_ready_results_without_waiting() {
    let (sender, mut queue) = srt_resolve_completion_queue(4);
    sender
        .try_send(SrtResolvedConnect {
            output_id: OutputId::new("out-a"),
            generation: 7,
            peer_addrs: peer_addrs(),
        })
        .unwrap();
    sender
        .try_send(SrtResolvedConnect {
            output_id: OutputId::new("out-b"),
            generation: 8,
            peer_addrs: vec!["127.0.0.3:9002".parse().unwrap()],
        })
        .unwrap();

    let mut resolved = Vec::new();
    queue.drain_resolved(&mut resolved);
    queue.drain_resolved(&mut resolved);

    assert_eq!(
        resolved
            .iter()
            .map(|completion| completion.output_id.as_str())
            .collect::<Vec<_>>(),
        vec!["out-a", "out-b"]
    );
}

#[test]
fn srt_shard_backend_media_tick_drains_resolve_completion_queue() {
    let peer_addrs = peer_addrs();
    let (sender, queue) = srt_resolve_completion_queue(4);
    sender
        .try_send(SrtResolvedConnect {
            output_id: OutputId::new("out-a"),
            generation: 7,
            peer_addrs: peer_addrs.clone(),
        })
        .unwrap();
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let mut backend = SrtShardBackend::with_runtime_components(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
        connector,
        queue,
    );
    backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://primary:9000?streamid=publish%3Akey".to_string(),
        },
    )));

    backend.on_media_tick();

    assert!(backend.pending_connect(&OutputId::new("out-a")).is_none());
    assert_eq!(
        connector_handle.calls(),
        vec![FakeConnectCall {
            peer_addrs,
            stream_id: "publish:key".to_string(),
            connect_timeout_ms: 10000,
        }]
    );
    assert_eq!(
        poller_handle.registered(),
        vec![(42, LeafKey(0), 7, SrtEgressInterest::WRITE)]
    );
}

#[test]
fn srt_shard_backend_media_tick_ignores_stale_resolved_connect() {
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let mut backend = SrtShardBackend::with_runtime_components(
        FakeReadinessPoller::default(),
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
        connector,
        FakeResolveCompletionSource::with(vec![SrtResolvedConnect {
            output_id: OutputId::new("out-a"),
            generation: 6,
            peer_addrs: peer_addrs(),
        }]),
    );
    backend.on_command(EgressCommand::Add(output_spec(
        "out-a",
        7,
        ProtocolSpec::Srt {
            url: "srt://primary:9000?streamid=publish:key".to_string(),
        },
    )));

    backend.on_media_tick();

    assert!(backend.pending_connect(&OutputId::new("out-a")).is_some());
    assert!(connector_handle.calls().is_empty());
}

#[test]
fn srt_shard_backend_rejects_socket_setup_failure_before_registering_leaf() {
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::failing();
    let configurator_handle = configurator.clone();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
    );

    let result = backend.add_connected_socket(common(7), 42);

    assert!(matches!(result, Err(SrtBackendAddError::Socket(_))));
    assert_eq!(
        configurator_handle.calls(),
        vec![(42, SrtEgressSendMode::FabricNonblocking)]
    );
    assert!(poller_handle.registered().is_empty());
}

#[test]
fn srt_shard_backend_socket_setup_failure_preserves_existing_leaf() {
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::failing();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
    );
    let probe = shared_sender();
    let key = backend
        .add_leaf(42, SrtFabricLeaf::new(common(7), Box::new(probe.sender)))
        .unwrap();

    let result = backend.add_connected_socket(common(8), 99);
    poller_handle.push_ready(SrtReadyLeaf {
        socket: 42,
        key,
        generation: 7,
        writable: true,
    });

    assert!(matches!(result, Err(SrtBackendAddError::Socket(_))));
    assert_eq!(
        backend.on_ready(),
        EgressShardCommandEffect::ScheduleReady { count: 1 }
    );
    assert_eq!(
        probe.sends.lock().unwrap().as_slice(),
        &[Bytes::from_static(b"abc")]
    );
    assert_eq!(*probe.closed.lock().unwrap(), 0);
}

#[test]
fn srt_shard_backend_ready_event_visits_registered_leaf() {
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let mut backend = SrtShardBackend::new(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    let probe = shared_sender();
    let key = backend
        .add_leaf(42, SrtFabricLeaf::new(common(7), Box::new(probe.sender)))
        .unwrap();
    poller_handle.push_ready(SrtReadyLeaf {
        socket: 42,
        key,
        generation: 7,
        writable: true,
    });

    let effect = backend.on_ready();

    assert_eq!(effect, EgressShardCommandEffect::ScheduleReady { count: 1 });
    assert_eq!(
        probe.sends.lock().unwrap().as_slice(),
        &[Bytes::from_static(b"abc")]
    );
}

#[test]
fn srt_shard_backend_ignores_unregistered_ready_leaf() {
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let mut backend = SrtShardBackend::new(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    let probe = shared_sender();
    backend
        .add_leaf(42, SrtFabricLeaf::new(common(7), Box::new(probe.sender)))
        .unwrap();
    poller_handle.push_ready(SrtReadyLeaf {
        socket: 99,
        key: LeafKey(9),
        generation: 7,
        writable: true,
    });

    let effect = backend.on_ready();

    assert_eq!(effect, EgressShardCommandEffect::Continue);
    assert!(probe.sends.lock().unwrap().is_empty());
}

#[test]
fn srt_shard_backend_remove_command_deregisters_before_closing_leaf() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let poller = FakeReadinessPoller::with_events(Arc::clone(&events));
    let poller_handle = poller.clone();
    let mut backend = SrtShardBackend::new(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    let probe = shared_sender_recording(Arc::clone(&events));
    backend
        .add_leaf(42, SrtFabricLeaf::new(common(7), Box::new(probe.sender)))
        .unwrap();

    let effect = backend.on_command(EgressCommand::Remove(OutputId::new("out-srt")));

    assert_eq!(effect, EgressShardCommandEffect::Continue);
    assert_eq!(poller_handle.removed(), vec![42]);
    assert_eq!(*probe.closed.lock().unwrap(), 1);
    assert_eq!(events.lock().unwrap().as_slice(), &["remove", "close"]);
}

#[test]
fn srt_shard_backend_removed_leaf_ignores_late_readiness() {
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let mut backend = SrtShardBackend::new(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    let probe = shared_sender();
    let key = backend
        .add_leaf(42, SrtFabricLeaf::new(common(7), Box::new(probe.sender)))
        .unwrap();

    backend.on_command(EgressCommand::Remove(OutputId::new("out-srt")));
    poller_handle.push_ready(SrtReadyLeaf {
        socket: 42,
        key,
        generation: 7,
        writable: true,
    });

    assert_eq!(backend.on_ready(), EgressShardCommandEffect::Continue);
    assert!(probe.sends.lock().unwrap().is_empty());
    assert_eq!(*probe.closed.lock().unwrap(), 1);
}

#[test]
fn srt_shard_backend_shutdown_closes_registered_leaves() {
    let poller = FakeReadinessPoller::default();
    let mut backend = SrtShardBackend::new(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    let probe = shared_sender();
    backend
        .add_leaf(42, SrtFabricLeaf::new(common(7), Box::new(probe.sender)))
        .unwrap();

    backend.on_shutdown();

    assert_eq!(*probe.closed.lock().unwrap(), 1);
}

#[test]
fn srt_shard_backend_shutdown_deregisters_before_closing_leaves() {
    let events = Arc::new(Mutex::new(Vec::new()));
    let poller = FakeReadinessPoller::with_events(Arc::clone(&events));
    let poller_handle = poller.clone();
    let mut backend = SrtShardBackend::new(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    let probe = shared_sender_recording(Arc::clone(&events));
    backend
        .add_leaf(42, SrtFabricLeaf::new(common(7), Box::new(probe.sender)))
        .unwrap();

    backend.on_shutdown();

    assert_eq!(poller_handle.removed(), vec![42]);
    assert_eq!(*probe.closed.lock().unwrap(), 1);
    assert_eq!(events.lock().unwrap().as_slice(), &["remove", "close"]);
}
