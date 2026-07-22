use super::*;
use crate::media::egress::backend::{CloseReason, EngineProgress, Readiness};
use crate::media::egress::command::{FeedId, OutputId};
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::{LeafLimits, WorkBudget};
use crate::media::egress::scheduler::{LeafKey, VisitDecision};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::egress::visit::EngineVisitResult;
use crate::media::srt::{
    SRTSOCKET, SrtEgressInterest, SrtEgressPollError, SrtMessageSender, SrtReadyLeaf, SrtSendResult,
};
use crate::media::ts_chunk_ring::TsChunkRing;
use bytes::Bytes;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use std::{collections::VecDeque, vec::Vec};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct FakeSender {
    sends: Vec<Bytes>,
    closed: u32,
}

impl SrtMessageSender for FakeSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        self.sends.push(message.clone());
        SrtSendResult::Accepted {
            bytes: message.len(),
        }
    }

    fn close(&mut self, _reason: CloseReason) {
        self.closed = self.closed.saturating_add(1);
    }
}

struct SharedFakeSender {
    sends: Arc<Mutex<Vec<Bytes>>>,
    closed: Arc<Mutex<u32>>,
}

impl SrtMessageSender for SharedFakeSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        self.sends.lock().unwrap().push(message.clone());
        SrtSendResult::Accepted {
            bytes: message.len(),
        }
    }

    fn close(&mut self, _reason: CloseReason) {
        let mut closed = self.closed.lock().unwrap();
        *closed = closed.saturating_add(1);
    }
}

struct SharedSenderProbe {
    sender: SharedFakeSender,
    sends: Arc<Mutex<Vec<Bytes>>>,
    closed: Arc<Mutex<u32>>,
}

#[derive(Clone, Default)]
struct FakeReadinessPoller {
    inner: Arc<Mutex<FakeReadinessState>>,
}

#[derive(Default)]
struct FakeReadinessState {
    registered: Vec<(SRTSOCKET, LeafKey, u64, SrtEgressInterest)>,
    ready: VecDeque<SrtReadyLeaf>,
}

impl FakeReadinessPoller {
    fn push_ready(&self, event: SrtReadyLeaf) {
        self.inner.lock().unwrap().ready.push_back(event);
    }
}

impl SrtReadinessPoller for FakeReadinessPoller {
    fn register_leaf(
        &mut self,
        socket: SRTSOCKET,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        self.inner
            .lock()
            .unwrap()
            .registered
            .push((socket, key, generation, interest));
        Ok(())
    }

    fn remove(&mut self, _socket: SRTSOCKET) -> Result<(), SrtEgressPollError> {
        Ok(())
    }

    fn poll_leaves(
        &mut self,
        _timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        ready.clear();
        let mut state = self.inner.lock().unwrap();
        while let Some(event) = state.ready.pop_front() {
            ready.push(event);
        }
        Ok(ready.len())
    }
}

fn leaf(generation: u64) -> SrtFabricLeaf<FakeSender> {
    SrtFabricLeaf::new(
        LeafCommon::new(
            OutputId::new("out-srt"),
            generation,
            FeedId::new("feed-srt"),
            LeafLimits::default(),
        ),
        FakeSender::default(),
    )
}

fn feed(chunks: impl IntoIterator<Item = Bytes>) -> TsFeed {
    let ring = TsChunkRing::new(8, CancellationToken::new());
    for chunk in chunks {
        ring.push(chunk, true);
    }
    TsFeed::new(&ring, Arc::new(FeedEpoch::new()))
}

fn budget() -> WorkBudget {
    WorkBudget::new(8, 1024, Duration::from_millis(1))
}

fn shared_sender() -> SharedSenderProbe {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(Mutex::new(0));
    SharedSenderProbe {
        sender: SharedFakeSender {
            sends: Arc::clone(&sends),
            closed: Arc::clone(&closed),
        },
        sends,
        closed,
    }
}

fn common(generation: u64) -> LeafCommon {
    LeafCommon::new(
        OutputId::new("out-srt"),
        generation,
        FeedId::new("feed-srt"),
        LeafLimits::default(),
    )
}

#[test]
fn srt_fabric_leaf_from_socket_keeps_native_sender_opaque() {
    let feed = feed([Bytes::from_static(b"abc")]);
    let mut leaf = srt_fabric_leaf_from_socket(common(9), 42);

    let result = leaf.visit_ready(8, Readiness::WRITABLE, &feed, budget());

    assert!(matches!(result, EngineVisitResult::StaleGeneration));
    assert_eq!(leaf.common().cursor.next_sequence, 0);
    assert_eq!(leaf.common().progress.total_bytes_sent, 0);
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
fn srt_fabric_leaf_visits_ready_ts_feed_through_common_engine_visit() {
    let feed = feed([Bytes::from_static(b"abc")]);
    let mut leaf = leaf(7);

    let result = leaf.visit_ready(7, Readiness::WRITABLE, &feed, budget());

    let EngineVisitResult::Visited(outcome) = result else {
        panic!("expected SRT fabric visit");
    };
    assert!(matches!(
        outcome.progress,
        EngineProgress::Progress {
            bytes: 3,
            units: 1,
            ..
        }
    ));
    assert_eq!(outcome.decision, VisitDecision::Continue);
    assert_eq!(leaf.common().cursor.next_sequence, 1);
    assert_eq!(leaf.common().progress.total_bytes_sent, 3);
    assert_eq!(leaf.common().progress.total_units_sent, 1);
    assert_eq!(leaf.pending_message_bytes(), 0);
}

#[test]
fn srt_fabric_leaf_ignores_stale_ready_generation() {
    let feed = feed([Bytes::from_static(b"abc")]);
    let mut leaf = leaf(9);

    let result = leaf.visit_ready(8, Readiness::WRITABLE, &feed, budget());

    assert!(matches!(result, EngineVisitResult::StaleGeneration));
    assert_eq!(leaf.common().cursor.next_sequence, 0);
    assert_eq!(leaf.common().progress.total_bytes_sent, 0);
}

#[test]
fn srt_visit_continue_decision_requests_requeue() {
    assert!(requeue_after_srt_visit(VisitDecision::Continue));
    assert!(!requeue_after_srt_visit(VisitDecision::Suspend));
    assert!(!requeue_after_srt_visit(VisitDecision::Close));
}
