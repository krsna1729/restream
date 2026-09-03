use super::super::*;
use super::support::{
    FakeConnectCall, FakeReadinessPoller, FakeResolveCompletionSource, FakeSocketConfigurator,
    FakeSocketConnector, common, feed, shared_sender, shared_sender_recording,
};
use crate::media::egress::command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::policy::{LeafPolicy, WorkBudget};
use crate::media::egress::scheduler::LeafKey;
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::srt::{SrtEgressInterest, SrtEgressSendMode, SrtReadyLeaf};
use bytes::Bytes;
use std::sync::{Arc, Mutex};
use std::time::Duration;

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
        progress: Default::default(),
    }
}

fn srt_output_spec(id: &str, generation: u64) -> OutputSpec {
    output_spec(
        id,
        generation,
        ProtocolSpec::Srt {
            url: "srt://primary:9000?streamid=publish%3Akey".to_string(),
        },
    )
}

/// Backend wired to an injected connector plus one queued pending connect,
/// so tests can drive the production `complete_pending_connect` path
/// end-to-end. `SrtShardBackend` is generic over its connector, so this
/// needs no test-only entry point on the backend itself.
fn backend_with_pending_connect(
    poller: FakeReadinessPoller,
    configurator: FakeSocketConfigurator,
    connector: FakeSocketConnector,
    spec: OutputSpec,
) -> SrtShardBackend<
    FakeReadinessPoller,
    FakeSocketConfigurator,
    FakeSocketConnector,
    NoopSrtResolveCompletionSource,
> {
    let mut backend = SrtShardBackend::with_runtime_components(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        configurator,
        connector,
        NoopSrtResolveCompletionSource,
    );
    backend.on_command(EgressCommand::Add(spec));
    backend
}

/// An `OutputSpec` whose progress sink exposes the unexpected-termination
/// flag, so failure-path tests can assert the bookkeeping that only
/// `complete_pending_connect` performs.
fn srt_output_spec_with_termination_flag(
    id: &str,
    generation: u64,
) -> (OutputSpec, Arc<std::sync::atomic::AtomicBool>) {
    let flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut spec = srt_output_spec(id, generation);
    spec.progress.terminated_unexpectedly = Some(Arc::clone(&flag));
    (spec, flag)
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
fn srt_shard_backend_complete_pending_connect_returns_connect_error_before_registering() {
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::default();
    let configurator_handle = configurator.clone();
    let connector = FakeSocketConnector::failing("connect failed");
    let connector_handle = connector.clone();
    let (spec, terminated) = srt_output_spec_with_termination_flag("out-a", 7);
    let mut backend = backend_with_pending_connect(poller, configurator, connector, spec);

    let result = backend.complete_pending_connect(&OutputId::new("out-a"), 7, &peer_addrs());

    assert_eq!(
        result,
        Err(SrtPendingConnectError::Connect(
            SrtBackendConnectError::Connect("connect failed".to_string())
        ))
    );
    assert_eq!(connector_handle.calls().len(), 1);
    assert!(configurator_handle.calls().is_empty());
    assert!(poller_handle.registered().is_empty());
    // The application never sees a leaf for a connect that failed outright,
    // so nothing else would report the attempt died — only this path marks
    // it. Previously unexercised, because the test-only sibling this test
    // used to call skipped the progress-sink bookkeeping entirely.
    assert!(terminated.load(std::sync::atomic::Ordering::Relaxed));
}

#[test]
fn srt_shard_backend_complete_pending_connect_returns_add_error_after_connect() {
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let configurator = FakeSocketConfigurator::failing();
    let configurator_handle = configurator.clone();
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let (spec, terminated) = srt_output_spec_with_termination_flag("out-a", 7);
    let mut backend = backend_with_pending_connect(poller, configurator, connector, spec);

    let result = backend.complete_pending_connect(&OutputId::new("out-a"), 7, &peer_addrs());

    assert!(matches!(
        result,
        Err(SrtPendingConnectError::Connect(
            SrtBackendConnectError::Add(_)
        ))
    ));
    assert_eq!(connector_handle.calls().len(), 1);
    assert_eq!(
        configurator_handle.calls(),
        vec![(42, SrtEgressSendMode::FabricNonblocking)]
    );
    assert!(poller_handle.registered().is_empty());
    // A socket that connected but could not be adopted is the same
    // application-visible outcome as a failed connect.
    assert!(terminated.load(std::sync::atomic::Ordering::Relaxed));
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
    assert_eq!(
        pending.connect_spec.bond_type(),
        shiguredo_srt::GroupType::Backup,
        "bonded URLs retain the historical Backup default"
    );
}

#[test]
fn srt_shard_backend_parses_broadcast_bond_mode() {
    let spec = SrtFabricEgressConnectSpec::from_url(
        "srt://primary:9000?bond=backup:9001&type=broadcast",
        10_000,
    );

    assert_eq!(spec.bond_type(), shiguredo_srt::GroupType::Broadcast);
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
    let mut backend =
        backend_with_pending_connect(poller, configurator, connector, srt_output_spec("out-a", 7));

    let key = backend
        .complete_pending_connect(&OutputId::new("out-a"), 7, &peer_addrs)
        .unwrap();

    assert_eq!(key, LeafKey(0));
    assert!(backend.pending_connect(&OutputId::new("out-a")).is_none());
    assert_eq!(
        connector_handle.calls(),
        vec![FakeConnectCall {
            peer_addrs,
            stream_id: "publish:key".to_string(),
            connect_timeout_ms: 30000,
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
    let mut backend = backend_with_pending_connect(
        FakeReadinessPoller::default(),
        FakeSocketConfigurator::default(),
        connector,
        srt_output_spec("out-a", 7),
    );

    let result = backend.complete_pending_connect(&OutputId::new("out-a"), 6, &peer_addrs());

    assert_eq!(result, Err(SrtPendingConnectError::Stale));
    assert!(backend.pending_connect(&OutputId::new("out-a")).is_some());
    assert!(connector_handle.calls().is_empty());
}

#[test]
fn srt_shard_backend_complete_pending_connect_rejects_missing_output() {
    let connector = FakeSocketConnector::returning(42);
    let connector_handle = connector.clone();
    let mut backend = backend_with_pending_connect(
        FakeReadinessPoller::default(),
        FakeSocketConfigurator::default(),
        connector,
        srt_output_spec("out-a", 7),
    );

    let result = backend.complete_pending_connect(&OutputId::new("out-missing"), 7, &peer_addrs());

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
            connect_timeout_ms: 30000,
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
            connect_timeout_ms: 30000,
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

// A blocked leaf must not strand an already-ready neighbor behind it: one
// poll batch reports both ready (blocked first), the first on_ready() call
// must ask for another pass (ScheduleReady) instead of reporting Continue,
// and the healthy leaf must be reached by the next call.
struct WouldBlockSender;
impl SrtMessageSender for WouldBlockSender {
    fn send_message(&mut self, _message: &Bytes) -> crate::media::srt::SrtSendResult {
        crate::media::srt::SrtSendResult::WouldBlock
    }
    fn close(&mut self, _reason: crate::media::egress::backend::CloseReason) {}
}

#[test]
fn on_ready_does_not_strand_a_second_ready_leaf_behind_a_would_block_leaf() {
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let mut backend = SrtShardBackend::new(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    // `common()` hardcodes one OutputId; a second leaf needs a distinct one
    // or `add_leaf` replaces (closes) the first via `output_sockets`.
    let blocked_common = LeafCommon::new(
        OutputId::new("out-srt-blocked"),
        6,
        FeedId::new("feed-srt"),
        LeafLimits::default(),
    );
    let blocked_sender: Box<dyn SrtMessageSender + Send> = Box::new(WouldBlockSender);
    let blocked = SrtFabricLeaf::new(blocked_common, blocked_sender);
    let blocked_key = backend.add_leaf(41, blocked).unwrap();
    let probe = shared_sender();
    let healthy_key = backend
        .add_leaf(42, SrtFabricLeaf::new(common(7), Box::new(probe.sender)))
        .unwrap();

    poller_handle.push_ready(SrtReadyLeaf {
        socket: 41,
        key: blocked_key,
        generation: 6,
        writable: true,
    });
    poller_handle.push_ready(SrtReadyLeaf {
        socket: 42,
        key: healthy_key,
        generation: 7,
        writable: true,
    });

    let effect = backend.on_ready();
    assert_eq!(effect, EgressShardCommandEffect::ScheduleReady { count: 1 });
    assert!(probe.sends.lock().unwrap().is_empty(), "not skipped yet");

    backend.on_ready();
    assert_eq!(
        probe.sends.lock().unwrap().as_slice(),
        &[Bytes::from_static(b"abc")]
    );
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

/// Test-local sender whose native backlog is settable from outside the
/// boxed transport, so sweeps can observe scripted decline sequences.
#[derive(Clone, Default)]
struct SharedBacklogSender {
    backlog: Arc<Mutex<Option<crate::media::srt::NativeSendBacklog>>>,
}

impl SrtMessageSender for SharedBacklogSender {
    fn send_message(&mut self, message: &Bytes) -> crate::media::srt::SrtSendResult {
        crate::media::srt::SrtSendResult::Accepted {
            bytes: message.len(),
        }
    }

    fn close(&mut self, _reason: crate::media::egress::backend::CloseReason) {}

    fn native_send_backlog(&mut self) -> Option<crate::media::srt::NativeSendBacklog> {
        *self.backlog.lock().unwrap()
    }
}

/// Stall-driven recovery: a leaf whose native sender buffer holds data
/// without declining past the no-progress deadline is closed by the sweep,
/// deregistered from the poller, and leaves no socket mapping behind.
#[test]
fn stall_sweep_closes_leaf_with_stuck_native_backlog() {
    use crate::media::srt::NativeSendBacklog;
    use std::time::Instant;

    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
    );

    let sender = SharedBacklogSender::default();
    *sender.backlog.lock().unwrap() = Some(NativeSendBacklog {
        bytes: 4_096,
        packets: 3,
        ms: 500,
    });
    let leaf = SrtFabricLeaf::new(
        common(7),
        Box::new(sender) as Box<dyn SrtMessageSender + Send>,
    );
    let deadline = leaf.common().limits.max_backpressure_duration;
    backend.add_leaf(42, leaf).unwrap();

    // First sweep: backpressured, within the deadline — nothing closes.
    let start = Instant::now();
    backend.sweep_stalled_leaves(start);
    assert_eq!(backend.output_sockets.len(), 1);

    // Past the no-progress deadline with no native decline: closed and
    // deregistered.
    backend.sweep_stalled_leaves(start + deadline + Duration::from_secs(2));
    assert!(backend.output_sockets.is_empty());
    assert_eq!(poller_handle.removed(), vec![42]);
}

/// The sweep leaves healthy leaves alone: a declining native backlog keeps
/// resetting the stall clock, so the leaf survives sweeps far past the
/// original deadline.
#[test]
fn stall_sweep_spares_leaf_with_draining_native_backlog() {
    use crate::media::srt::NativeSendBacklog;
    use std::time::Instant;

    let poller = FakeReadinessPoller::default();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
    );

    let sender = SharedBacklogSender::default();
    let backlog_handle = sender.backlog.clone();
    *backlog_handle.lock().unwrap() = Some(NativeSendBacklog {
        bytes: 10_000,
        packets: 8,
        ms: 500,
    });
    let leaf = SrtFabricLeaf::new(
        common(7),
        Box::new(sender) as Box<dyn SrtMessageSender + Send>,
    );
    let deadline = leaf.common().limits.max_backpressure_duration;
    backend.add_leaf(42, leaf).unwrap();

    let start = Instant::now();
    backend.sweep_stalled_leaves(start);

    // Backlog declines before each sweep: the leaf keeps making native
    // progress and survives well past the original deadline.
    for step in 1..=3u64 {
        *backlog_handle.lock().unwrap() = Some(NativeSendBacklog {
            bytes: 10_000 - step * 2_000,
            packets: 8,
            ms: 500,
        });
        backend.sweep_stalled_leaves(start + deadline * step as u32);
    }
    assert_eq!(backend.output_sockets.len(), 1);
}

/// Live-path regression: a connected leaf on a real shard thread must send
/// once a feed wake arrives.  The wake schedules ready work, ready work
/// polls the readiness backend, and the visit drains the feed — without this
/// chain the fabric connects sockets but never delivers media (the failure
/// observed live as zero packetsOut on every fabric SRT output).
#[test]
fn feed_wake_drives_connected_leaf_to_send_on_shard_thread() {
    use crate::media::egress::command::ShardId;
    use crate::media::egress::shard::{EgressShardConfig, EgressShardHandle};
    use std::time::Instant;

    let poller = FakeReadinessPoller::default();
    let probe = shared_sender();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller.clone(),
        feed([
            Bytes::from_static(b"payload-1"),
            Bytes::from_static(b"payload-2"),
        ]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
    );
    let bytes_out = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let last_progress_ms = Arc::new(std::sync::atomic::AtomicU64::new(0));
    let sink = crate::media::egress::leaf::EgressProgressSink {
        bytes_sent: Some(bytes_out.clone()),
        last_progress_ms: Some(last_progress_ms.clone()),
        ..Default::default()
    };
    let leaf = SrtFabricLeaf::new(
        common(7).with_progress_sink(sink),
        Box::new(probe.sender) as Box<dyn SrtMessageSender + Send>,
    );
    let key = backend.add_leaf(42, leaf).unwrap();
    poller.push_ready(SrtReadyLeaf {
        socket: 42,
        key,
        generation: 7,
        writable: true,
    });

    // Long idle wait: without the wake-to-ready chain the shard would sleep
    // and the sends assertion below would time out.
    let config = EgressShardConfig::new(64, 8, 8, 8, Duration::from_secs(5)).unwrap();
    let handle = EgressShardHandle::spawn(ShardId::new(0), config, backend);

    handle.deliver_feed_wake().unwrap();

    let deadline = Instant::now() + Duration::from_secs(2);
    loop {
        if !probe.sends.lock().unwrap().is_empty() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "feed wake did not drive the connected leaf to send"
        );
        std::thread::yield_now();
    }

    handle.shutdown_and_join();
    let sends = probe.sends.lock().unwrap();
    // Both chunks in this feed are keyframes, and a fresh leaf is primed onto
    // the newest retained sync point before its first read, so it starts at
    // `payload-2` rather than replaying the already-published `payload-1`.
    assert_eq!(&*sends[0], b"payload-2".as_slice());

    // Status publication: the shard published progress into the
    // application-side counters without any app-thread involvement.
    assert!(bytes_out.load(std::sync::atomic::Ordering::Relaxed) >= sends[0].len() as u64);
    assert!(last_progress_ms.load(std::sync::atomic::Ordering::Relaxed) > 0);
}

/// `on_ready` must remove a leaf that closes (peer closed, failed) instead
/// of silently dropping the decision — otherwise the socket stays
/// registered and connected while never being revisited, permanently
/// stalling the output with zero delivered bytes.
#[test]
fn on_ready_removes_leaf_on_close_decision() {
    let poller = FakeReadinessPoller::default();
    let poller_handle = poller.clone();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
    );

    struct PeerClosedSender;
    impl SrtMessageSender for PeerClosedSender {
        fn send_message(&mut self, _message: &Bytes) -> crate::media::srt::SrtSendResult {
            crate::media::srt::SrtSendResult::PeerClosed
        }
        fn close(&mut self, _reason: crate::media::egress::backend::CloseReason) {}
    }

    let leaf = SrtFabricLeaf::new(
        common(7),
        Box::new(PeerClosedSender) as Box<dyn SrtMessageSender + Send>,
    );
    let key = backend.add_leaf(42, leaf).unwrap();
    poller_handle.push_ready(SrtReadyLeaf {
        socket: 42,
        key,
        generation: 7,
        writable: true,
    });

    backend.on_ready();

    assert!(backend.output_sockets.is_empty());
    assert_eq!(poller_handle.removed(), vec![42]);
}
