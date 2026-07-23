use super::super::*;
use super::support::{config, output_spec};
use crate::media::egress::backend::{EngineProgress, Interest, ProtocolEngine, Readiness};
use crate::media::egress::command::{EgressCommand, OutputId, ShardId};
use crate::media::egress::feed::FeedCursor;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::scheduler::{LeafKey, ReadyQueue, ScheduleState, try_enqueue};
use crate::media::egress::test_driver::{EngineScript, FakeEngine, FakeFeed, FakeTransport};
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

/// Phase 3 exit-gate headline shape on one real shard thread: a healthy
/// population shares its shard with one permanently blocked and one severely
/// throttled leaf.  Every healthy leaf must keep making progress with
/// deterministic bounded completion; the blocked leaf leaves the runnable set
/// after one visit and the throttled leaf's rotation cannot starve neighbors.
#[test]
fn healthy_population_progresses_beside_blocked_and_throttled_leaves_same_shard() {
    const HEALTHY: usize = 97;
    const VISITS_PER_HEALTHY: u64 = 3;

    let probe = LeafProbe::default();
    let handle = EgressShardHandle::spawn(
        ShardId::new(0),
        config(128, 8),
        LeafHarnessBackend::new(probe.clone()),
    );

    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-blocked"))),
        Ok(())
    );
    assert_eq!(
        handle.try_send(EgressCommand::Add(output_spec("out-throttled"))),
        Ok(())
    );
    for idx in 0..HEALTHY {
        assert_eq!(
            handle.try_send(EgressCommand::Add(output_spec(&format!("out-h{idx}")))),
            Ok(())
        );
    }

    probe.wait_for_each_healthy_at_least(VISITS_PER_HEALTHY, HEALTHY);
    let snapshot = handle.shutdown_and_join();
    let state = probe.state();

    assert_eq!(state.blocked_visits, 1);
    assert!(state.throttled_visits >= 1);
    assert_eq!(state.healthy_leaf_count(), HEALTHY);
    assert!(state.min_healthy_visits() >= VISITS_PER_HEALTHY);
    assert!(snapshot.stopped);
    assert!(!snapshot.panicked);
}

/// Cross-shard control for the headline shape: the bad neighbors live on
/// shard 0 while the healthy population runs on shard 1.  The healthy shard's
/// progress must be indistinguishable from an all-healthy run: full visit
/// targets, no bad-leaf visits recorded, both shard threads join cleanly.
#[test]
fn healthy_shard_unaffected_by_blocked_and_throttled_leaves_on_other_shard() {
    const HEALTHY: usize = 97;
    const VISITS_PER_HEALTHY: u64 = 3;

    let bad_probe = LeafProbe::default();
    let healthy_probe = LeafProbe::default();
    let group = EgressShardGroup::spawn(
        NonZeroU32::new(2).unwrap(),
        config(128, 8),
        vec![
            LeafHarnessBackend::new(bad_probe.clone()),
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
            ShardId::new(0),
            EgressCommand::Add(output_spec("out-throttled"))
        ),
        Ok(())
    );
    for idx in 0..HEALTHY {
        assert_eq!(
            group.try_send_to(
                ShardId::new(1),
                EgressCommand::Add(output_spec(&format!("out-h{idx}")))
            ),
            Ok(())
        );
    }

    healthy_probe.wait_for_each_healthy_at_least(VISITS_PER_HEALTHY, HEALTHY);
    let snapshots = group.shutdown_and_join();
    let bad = bad_probe.state();
    let healthy = healthy_probe.state();

    assert_eq!(bad.blocked_visits, 1);
    assert_eq!(bad.healthy_leaf_count(), 0);
    assert_eq!(healthy.blocked_visits, 0);
    assert_eq!(healthy.throttled_visits, 0);
    assert_eq!(healthy.healthy_leaf_count(), HEALTHY);
    assert!(healthy.min_healthy_visits() >= VISITS_PER_HEALTHY);
    assert_eq!(snapshots.len(), 2);
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
            } else if spec.id.as_str().contains("throttled") {
                // Severely throttled but writable: trickles one unit, then
                // yields the visit — stays in rotation without leaving it.
                FakeEngine::new(
                    std::iter::repeat_n(
                        [
                            EngineScript::Progress {
                                bytes: 1,
                                units: 1,
                                interest: Interest::WRITE,
                            },
                            EngineScript::Yield,
                        ],
                        512,
                    )
                    .flatten()
                    .collect(),
                )
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
        } else if output_id.as_str().contains("throttled") {
            state.throttled_visits = state.throttled_visits.saturating_add(1);
        } else {
            state.healthy_visits = state.healthy_visits.saturating_add(1);
            *state
                .healthy_visits_by_leaf
                .entry(output_id.as_str().to_owned())
                .or_insert(0) += 1;
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

    /// Deterministic bounded completion for the headline matrix shapes:
    /// every one of `leaf_count` healthy leaves reaches `target` visits.
    fn wait_for_each_healthy_at_least(&self, target: u64, leaf_count: usize) {
        let (lock, condvar) = &*self.inner;
        let state = lock.lock().unwrap();
        let result = condvar
            .wait_timeout_while(state, Duration::from_secs(10), |state| {
                state.healthy_leaf_count() < leaf_count || state.min_healthy_visits() < target
            })
            .unwrap();
        assert!(
            !result.1.timed_out(),
            "healthy population did not reach {target} visits per leaf: \
             {} leaves seen, min visits {}",
            result.0.healthy_leaf_count(),
            result.0.min_healthy_visits()
        );
    }

    fn state(&self) -> LeafProbeState {
        self.inner.0.lock().unwrap().clone()
    }
}

#[derive(Debug, Clone, Default)]
struct LeafProbeState {
    blocked_visits: u64,
    throttled_visits: u64,
    healthy_visits: u64,
    healthy_visits_by_leaf: std::collections::HashMap<String, u64>,
}

impl LeafProbeState {
    fn healthy_leaf_count(&self) -> usize {
        self.healthy_visits_by_leaf.len()
    }

    fn min_healthy_visits(&self) -> u64 {
        self.healthy_visits_by_leaf
            .values()
            .copied()
            .min()
            .unwrap_or(0)
    }
}
