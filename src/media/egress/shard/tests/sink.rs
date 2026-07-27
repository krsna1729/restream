use super::super::*;
use super::support::{config, output_spec};
use crate::media::egress::backend::{EngineProgress, ProtocolEngine, Readiness};
use crate::media::egress::backends::sink::{SinkDiscardStats, SinkEngine, SinkTransport};
use crate::media::egress::command::{EgressCommand, OutputId, ShardId};
use crate::media::egress::feed::FeedCursor;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::scheduler::{LeafKey, ReadyQueue, ScheduleState, try_enqueue};
use crate::media::egress::test_driver::{FakeEngine, FakeFeed, FakeTransport};
use bytes::Bytes;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[test]
fn sink_leaf_discards_feed_units_on_shard_thread() {
    let probe = SinkProbe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(16, 4),
        SinkHarnessBackend::new(probe.clone()),
    );

    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-sink"))),
        Ok(())
    );

    probe.wait_for_discarded_units(3);
    let snapshot = handle.shutdown_and_join();
    let state = probe.state();

    assert_eq!(state.sink.discarded_units, 3);
    assert_eq!(state.sink.discarded_bytes, 9);
    assert_eq!(state.sink.close_count, 0);
    assert_eq!(state.network_visits, 0);
    assert!(snapshot.media_ticks >= 1);
    assert!(snapshot.stopped);
    assert!(!snapshot.panicked);
}

#[test]
fn slow_sink_leaf_does_not_starve_network_leaf_on_same_shard_thread() {
    let probe = SinkProbe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(16, 4),
        SinkHarnessBackend::new(probe.clone()),
    );

    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-slow-sink"))),
        Ok(())
    );
    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-network"))),
        Ok(())
    );

    probe.wait_for_network_visits(3);
    let snapshot = handle.shutdown_and_join();
    let state = probe.state();

    assert!(state.sink.discarded_units >= 1);
    assert!(state.network_visits >= 3);
    assert!(snapshot.media_ticks >= 1);
    assert!(snapshot.stopped);
    assert!(!snapshot.panicked);
}

struct SinkHarnessBackend {
    probe: SinkProbe,
    feed: FakeFeed,
    queue: ReadyQueue,
    leaves: Vec<SinkHarnessLeaf>,
}

impl SinkHarnessBackend {
    fn new(probe: SinkProbe) -> Self {
        let feed = FakeFeed::new();
        feed.push(Bytes::from_static(b"one"), true);
        feed.push(Bytes::from_static(b"two"), false);
        feed.push(Bytes::from_static(b"six"), false);
        Self {
            probe,
            feed,
            queue: ReadyQueue::new(),
            leaves: Vec::new(),
        }
    }

    fn visit_ready_leaf(&mut self, key: LeafKey) {
        let leaf = &mut self.leaves[key.0];
        leaf.schedule.enqueued = false;
        let progress = leaf.kind.advance(&self.feed, &mut leaf.cursor);

        match progress {
            EngineProgress::Progress { .. } => match &leaf.kind {
                HarnessEngine::Sink { transport, .. } => {
                    leaf.schedule.mark_serviced();
                    self.probe.record_sink(transport.stats());
                    try_enqueue(&mut leaf.schedule, &mut self.queue, key);
                }
                HarnessEngine::Network { .. } => {
                    leaf.schedule.mark_serviced();
                    self.probe.record_network_visit();
                    try_enqueue(&mut leaf.schedule, &mut self.queue, key);
                }
            },
            EngineProgress::Needs(_) => {
                leaf.schedule.mark_serviced();
                if let HarnessEngine::Sink { transport, .. } = &leaf.kind {
                    self.probe.record_sink(transport.stats());
                }
            }
            EngineProgress::Yield => {
                try_enqueue(&mut leaf.schedule, &mut self.queue, key);
            }
            EngineProgress::HandshakeComplete
            | EngineProgress::FeedOverrun
            | EngineProgress::PeerClosed
            | EngineProgress::Failed(_) => {}
        }
    }
}

impl EgressShardBackend for SinkHarnessBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        if let EgressCommand::Add(spec) = command {
            let key = LeafKey(self.leaves.len());
            let mut leaf = SinkHarnessLeaf {
                schedule: ScheduleState::new(),
                kind: HarnessEngine::from_output_id(&spec.id),
                cursor: FeedCursor::new(0, 0),
            };
            try_enqueue(&mut leaf.schedule, &mut self.queue, key);
            self.leaves.push(leaf);
        }
        EgressShardCommandEffect::Continue
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        for _ in 0..4 {
            let Some(key) = self.queue.dequeue_next() else {
                return EgressShardCommandEffect::Continue;
            };
            self.visit_ready_leaf(key);
        }
        EgressShardCommandEffect::Continue
    }
}

struct SinkHarnessLeaf {
    schedule: ScheduleState,
    kind: HarnessEngine,
    cursor: FeedCursor,
}

enum HarnessEngine {
    Sink {
        engine: SinkEngine<FakeFeed>,
        transport: SinkTransport,
        budget_units: usize,
    },
    Network {
        engine: FakeEngine,
        transport: FakeTransport,
    },
}

impl HarnessEngine {
    fn from_output_id(output_id: &OutputId) -> Self {
        if output_id.as_str().contains("network") {
            Self::Network {
                engine: FakeEngine::always_progress(1, 1),
                transport: FakeTransport::default(),
            }
        } else {
            Self::Sink {
                engine: SinkEngine::<FakeFeed>::default(),
                transport: SinkTransport::default(),
                budget_units: if output_id.as_str().contains("slow") {
                    1
                } else {
                    4
                },
            }
        }
    }

    fn advance(&mut self, feed: &FakeFeed, cursor: &mut FeedCursor) -> EngineProgress {
        match self {
            Self::Sink {
                engine,
                transport,
                budget_units,
            } => engine.advance(
                transport,
                Readiness::WRITABLE,
                feed,
                cursor,
                WorkBudget::new(*budget_units, 4 * 1024, Duration::from_millis(10)),
            ),
            Self::Network { engine, transport } => engine.advance(
                transport,
                Readiness::WRITABLE,
                feed,
                cursor,
                WorkBudget::new(1, 4 * 1024, Duration::from_millis(10)),
            ),
        }
    }
}

#[derive(Debug, Clone, Default)]
struct SinkProbe {
    inner: Arc<(Mutex<SinkProbeState>, Condvar)>,
}

impl SinkProbe {
    fn record_sink(&self, stats: SinkDiscardStats) {
        let (lock, condvar) = &*self.inner;
        lock.lock().unwrap().sink = stats;
        condvar.notify_all();
    }

    fn record_network_visit(&self) {
        let (lock, condvar) = &*self.inner;
        let mut state = lock.lock().unwrap();
        state.network_visits = state.network_visits.saturating_add(1);
        condvar.notify_all();
    }

    fn wait_for_discarded_units(&self, target: u64) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.sink.discarded_units < target
            })
            .unwrap();
        assert!(result.0.sink.discarded_units >= target);
    }

    fn wait_for_network_visits(&self, target: u64) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.network_visits < target
            })
            .unwrap();
        assert!(result.0.network_visits >= target);
    }

    fn state(&self) -> SinkProbeState {
        *self.inner.0.lock().unwrap()
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct SinkProbeState {
    sink: SinkDiscardStats,
    network_visits: u64,
}
