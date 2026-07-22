use super::super::*;
use super::support::{config, output_spec};
use crate::media::egress::backend::{EngineProgress, ProtocolEngine, Readiness};
use crate::media::egress::command::{EgressCommand, OutputId, ShardId};
use crate::media::egress::feed::FeedCursor;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::scheduler::{LeafKey, ReadyQueue, ScheduleState, try_enqueue};
use crate::media::egress::test_driver::{FakeEngine, FakeFeed, FakeTransport};
use std::num::NonZeroU32;
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

#[test]
fn blocked_leaf_does_not_starve_ready_leaf_on_same_shard_thread() {
    let probe = LeafProbe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(16, 4),
        LeafHarnessBackend::new(probe.clone()),
    );

    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-blocked"))),
        Ok(())
    );
    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-healthy"))),
        Ok(())
    );

    probe.wait_for_healthy_visits(3);
    let snapshot = handle.shutdown_and_join();
    let state = probe.state();

    assert_eq!(state.blocked_visits, 1);
    assert!(state.healthy_visits >= 3);
    assert!(snapshot.media_ticks >= 1);
    assert!(snapshot.stopped);
    assert!(!snapshot.panicked);
}

#[test]
fn blocked_leaf_on_one_shard_does_not_starve_ready_leaf_on_another_shard_thread() {
    let blocked_probe = LeafProbe::default();
    let healthy_probe = LeafProbe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(16, 4),
        vec![
            LeafHarnessBackend::new(blocked_probe.clone()),
            LeafHarnessBackend::new(healthy_probe.clone()),
        ],
    )
    .unwrap();

    assert_eq!(
        group.try_send_to(
            ShardId::new(0),
            EgressCommand::Add(output_spec("out-blocked"))
        ),
        Ok(())
    );
    assert_eq!(
        group.try_send_to(
            ShardId::new(1),
            EgressCommand::Add(output_spec("out-healthy"))
        ),
        Ok(())
    );

    healthy_probe.wait_for_healthy_visits(3);
    let snapshots = group.shutdown_and_join();
    let blocked = blocked_probe.state();
    let healthy = healthy_probe.state();

    assert_eq!(blocked.blocked_visits, 1);
    assert_eq!(blocked.healthy_visits, 0);
    assert_eq!(healthy.blocked_visits, 0);
    assert!(healthy.healthy_visits >= 3);
    assert_eq!(snapshots.len(), 2);
    assert!(snapshots.iter().all(|snapshot| snapshot.media_ticks >= 1));
    assert!(snapshots.iter().all(|snapshot| snapshot.stopped));
    assert!(snapshots.iter().all(|snapshot| !snapshot.panicked));
}

struct LeafHarnessBackend {
    probe: LeafProbe,
    feed: FakeFeed,
    queue: ReadyQueue,
    leaves: Vec<HarnessLeaf>,
}

impl LeafHarnessBackend {
    fn new(probe: LeafProbe) -> Self {
        Self {
            probe,
            feed: FakeFeed::new(),
            queue: ReadyQueue::new(),
            leaves: Vec::new(),
        }
    }

    fn visit_ready_leaf(&mut self, key: LeafKey) {
        let leaf = &mut self.leaves[key.0];
        leaf.schedule.enqueued = false;
        let progress = leaf.engine.advance(
            &mut leaf.transport,
            Readiness::WRITABLE,
            &self.feed,
            &mut leaf.cursor,
            WorkBudget::new(4, 4 * 1024, Duration::from_millis(10)),
        );

        match progress {
            EngineProgress::Progress { .. } => {
                leaf.schedule.mark_serviced();
                self.probe.record_visit(&leaf.output_id);
                try_enqueue(&mut leaf.schedule, &mut self.queue, key);
            }
            EngineProgress::Yield => {
                try_enqueue(&mut leaf.schedule, &mut self.queue, key);
            }
            EngineProgress::Needs(_) => {
                self.probe.record_visit(&leaf.output_id);
            }
            EngineProgress::HandshakeComplete
            | EngineProgress::FeedOverrun
            | EngineProgress::PeerClosed
            | EngineProgress::Failed(_) => {}
        }
    }
}

impl EgressShardBackend for LeafHarnessBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        if let EgressCommand::Add(spec) = command {
            let key = LeafKey(self.leaves.len());
            let engine = if spec.id.as_str().contains("blocked") {
                FakeEngine::always_blocks()
            } else {
                FakeEngine::always_progress(1, 1)
            };
            let mut leaf = HarnessLeaf {
                output_id: spec.id,
                schedule: ScheduleState::new(),
                engine,
                transport: FakeTransport::default(),
                cursor: FeedCursor::new(0, 0),
            };

            try_enqueue(&mut leaf.schedule, &mut self.queue, key);
            self.leaves.push(leaf);
        }
        EgressShardCommandEffect::Continue
    }

    fn on_media_tick(&mut self) {
        for _ in 0..4 {
            let Some(key) = self.queue.dequeue_next() else {
                return;
            };
            self.visit_ready_leaf(key);
        }
    }
}

struct HarnessLeaf {
    output_id: OutputId,
    schedule: ScheduleState,
    engine: FakeEngine,
    transport: FakeTransport,
    cursor: FeedCursor,
}

#[derive(Debug, Clone, Default)]
struct LeafProbe {
    inner: Arc<(Mutex<LeafProbeState>, Condvar)>,
}

impl LeafProbe {
    fn record_visit(&self, output_id: &OutputId) {
        let (lock, condvar) = &*self.inner;
        let mut state = lock.lock().unwrap();
        if output_id.as_str().contains("blocked") {
            state.blocked_visits = state.blocked_visits.saturating_add(1);
        } else {
            state.healthy_visits = state.healthy_visits.saturating_add(1);
        }
        condvar.notify_all();
    }

    fn wait_for_healthy_visits(&self, target: u64) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(2), |state| {
                state.healthy_visits < target
            })
            .unwrap();
        assert!(result.0.healthy_visits >= target);
    }

    fn state(&self) -> LeafProbeState {
        *self.inner.0.lock().unwrap()
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct LeafProbeState {
    blocked_visits: u64,
    healthy_visits: u64,
}
