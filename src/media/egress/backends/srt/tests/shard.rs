use super::super::*;
use super::support::{FakeReadinessPoller, common, feed, shared_sender, shared_sender_recording};
use crate::media::egress::command::{EgressCommand, OutputId};
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::scheduler::LeafKey;
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::srt::{
    SRTSOCKET, SrtEgressInterest, SrtEgressSendMode, SrtEgressSocketError,
    SrtFabricEgressConnectConfig, SrtReadyLeaf,
};
use bytes::Bytes;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[derive(Clone, Default)]
struct FakeSocketConfigurator {
    calls: Arc<Mutex<Vec<(SRTSOCKET, SrtEgressSendMode)>>>,
    fail: bool,
}

impl FakeSocketConfigurator {
    fn failing() -> Self {
        Self {
            calls: Arc::new(Mutex::new(Vec::new())),
            fail: true,
        }
    }

    fn calls(&self) -> Vec<(SRTSOCKET, SrtEgressSendMode)> {
        self.calls.lock().unwrap().clone()
    }
}

impl SrtSocketConfigurator for FakeSocketConfigurator {
    fn configure_connected(
        &mut self,
        socket: SRTSOCKET,
        mode: SrtEgressSendMode,
    ) -> Result<(), SrtEgressSocketError> {
        self.calls.lock().unwrap().push((socket, mode));
        if self.fail {
            return Err(SrtEgressSocketError {
                option: "SRTO_SNDSYN",
                code: 1234,
                message: "fake socket setup failure".to_owned(),
            });
        }
        Ok(())
    }
}

#[derive(Clone)]
struct FakeSocketConnector {
    socket: Result<SRTSOCKET, String>,
    calls: Arc<Mutex<Vec<FakeConnectCall>>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct FakeConnectCall {
    peer_addrs: Vec<std::net::SocketAddr>,
    stream_id: String,
    connect_timeout_ms: u64,
}

impl FakeSocketConnector {
    fn returning(socket: SRTSOCKET) -> Self {
        Self {
            socket: Ok(socket),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn failing(error: &str) -> Self {
        Self {
            socket: Err(error.to_string()),
            calls: Arc::new(Mutex::new(Vec::new())),
        }
    }

    fn calls(&self) -> Vec<FakeConnectCall> {
        self.calls.lock().unwrap().clone()
    }
}

impl SrtSocketConnector for FakeSocketConnector {
    fn connect(&mut self, config: SrtFabricEgressConnectConfig<'_>) -> Result<SRTSOCKET, String> {
        self.calls.lock().unwrap().push(FakeConnectCall {
            peer_addrs: config.peer_addrs().to_vec(),
            stream_id: config.stream_id().to_string(),
            connect_timeout_ms: config.connect_timeout_ms(),
        });
        self.socket.clone()
    }
}

fn fabric_connect_config(peer_addrs: &[std::net::SocketAddr]) -> SrtFabricEgressConnectConfig<'_> {
    SrtFabricEgressConnectConfig::plaintext(peer_addrs, "publish:key", 1500, None)
}

fn peer_addrs() -> Vec<std::net::SocketAddr> {
    vec![
        "127.0.0.1:9000".parse().unwrap(),
        "127.0.0.2:9001".parse().unwrap(),
    ]
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
