//! Sink fabric shard backend: wires [`SinkEngine`] into
//! [`EgressShardBackend`] on real shard OS threads — the production
//! counterpart to the test-only `SinkHarnessBackend`
//! (`src/media/egress/shard/tests/sink.rs`), which already proved
//! `SinkEngine` is schedulable on a shard but was never wired into
//! `EgressManager`/`EgressCommand` dispatch (see
//! `docs/egress-implementation.md` Phase 4a status).
//!
//! Unlike SRT/RTMP, a sink leaf has no socket and no poller: discarding
//! costs no I/O, so a leaf is always conceptually "writable." That means
//! there is no epoll/`srt_epoll_wait`-style readiness source to fall back
//! on — `EgressCommand::FeedWake` is the *only* signal this backend ever
//! gets that new data might exist. RTMP and SRT enqueue feed-waiting leaves
//! directly on a wake too (`enqueue_feed_waiting_leaves`), but they can
//! still fall back on a real poll for anything that wake missed; this
//! backend cannot, so its `FeedWake` handler must re-enqueue every leaf
//! into the ready queue. `on_media_tick` then drains the ready
//! queue every tick regardless, so a missed or coalesced wake is not fatal
//! — it just costs one extra tick of latency, the same tradeoff the
//! poller-driven backends make against their own idle-poll cadence.

use std::collections::{HashMap, VecDeque};
use std::time::Duration;

use crate::media::egress::backend::{CloseReason, ProtocolEngine, Readiness};
use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::RingFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::{LeafLimits, WorkBudget};
use crate::media::egress::scheduler::{LeafKey, ReadyQueue, VisitDecision, try_enqueue};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::egress::visit::{EngineVisit, EngineVisitResult};

use super::sink::{SinkEngine, SinkTransport};

/// Ready leaves drained per `on_media_tick`. Sink work is cheap (no I/O, no
/// syscalls) so this is generous compared to the poller-driven backends'
/// per-visit budgets — the limit exists to bound one tick's wall-clock
/// cost on a shard serving many sink outputs, not to ration a scarce
/// resource the way SRT/RTMP's connect or send budgets do.
const MEDIA_TICK_VISIT_BUDGET: usize = 256;

struct SinkFabricLeaf {
    common: LeafCommon,
    engine: SinkEngine<RingFeed>,
    transport: SinkTransport,
}

impl SinkFabricLeaf {
    fn visit_ready(
        &mut self,
        generation: u64,
        feed: &RingFeed,
        budget: WorkBudget,
    ) -> EngineVisitResult {
        EngineVisit {
            generation,
            common: &mut self.common,
            engine: &mut self.engine,
            transport: &mut self.transport,
            readiness: Readiness::WRITABLE,
            feed,
            budget,
        }
        .run()
    }
}

pub(crate) struct SinkShardBackend {
    feed: RingFeed,
    budget_max_units: usize,
    budget_max_bytes: usize,
    budget_window: Duration,
    leaves: Vec<Option<SinkFabricLeaf>>,
    output_leaves: HashMap<OutputId, LeafKey>,
    ready: ReadyQueue,
}

impl SinkShardBackend {
    pub(crate) fn new(feed: RingFeed, budget: WorkBudget) -> Self {
        let budget_window = budget
            .deadline
            .saturating_duration_since(std::time::Instant::now());
        Self {
            feed,
            budget_max_units: budget.max_units,
            budget_max_bytes: budget.max_bytes,
            budget_window,
            leaves: Vec::new(),
            output_leaves: HashMap::new(),
            ready: ReadyQueue::new(),
        }
    }

    fn add_leaf(&mut self, spec: OutputSpec) {
        let output_id = spec.id.clone();
        let common = LeafCommon::new(
            spec.id,
            spec.generation,
            spec.feed,
            LeafLimits::from_policy(&spec.policy),
        )
        .with_progress_sink(spec.progress);
        let key = LeafKey(self.leaves.len());
        self.leaves.push(Some(SinkFabricLeaf {
            common,
            engine: SinkEngine::default(),
            transport: SinkTransport::default(),
        }));
        if let Some(previous) = self.output_leaves.insert(output_id, key) {
            self.remove_leaf_key(previous);
        }
        self.enqueue(key);
    }

    fn enqueue(&mut self, key: LeafKey) {
        if let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut) {
            try_enqueue(&mut leaf.common.schedule, &mut self.ready, key);
        }
    }

    fn remove_leaf_by_output(&mut self, output_id: &OutputId) {
        if let Some(key) = self.output_leaves.remove(output_id) {
            self.remove_leaf_key(key);
        }
    }

    fn remove_leaf_key(&mut self, key: LeafKey) {
        if let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::take) {
            let mut leaf = leaf;
            leaf.engine.close(&mut leaf.transport, CloseReason::Removed);
        }
    }

    /// The only readiness signal this backend has — see the module doc.
    /// Re-enqueues every leaf that isn't already pending a visit.
    fn enqueue_all_leaves(&mut self) {
        let keys: Vec<LeafKey> = (0..self.leaves.len()).map(LeafKey).collect();
        for key in keys {
            self.enqueue(key);
        }
    }

    fn drain_ready_leaves(&mut self) {
        for _ in 0..MEDIA_TICK_VISIT_BUDGET {
            let Some(key) = self.ready.dequeue_next() else {
                return;
            };
            let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::as_mut) else {
                continue;
            };
            let generation = leaf.common.generation;
            let budget = WorkBudget::new(
                self.budget_max_units,
                self.budget_max_bytes,
                self.budget_window,
            );
            let result = leaf.visit_ready(generation, &self.feed, budget);
            let decision = match result {
                EngineVisitResult::StaleGeneration => VisitDecision::Suspend,
                EngineVisitResult::Visited(outcome) => outcome.decision,
            };
            match decision {
                VisitDecision::Continue => self.enqueue(key),
                VisitDecision::Suspend => {}
                VisitDecision::Close => {
                    if let Some(leaf) = self.leaves.get_mut(key.0).and_then(Option::take) {
                        let mut leaf = leaf;
                        self.output_leaves.remove(&leaf.common.output_id);
                        leaf.engine.close(&mut leaf.transport, CloseReason::Removed);
                    }
                }
            }
        }
    }
}

impl EgressShardBackend for SinkShardBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match command {
            EgressCommand::Add(spec) | EgressCommand::Update(spec) => {
                if matches!(spec.protocol, ProtocolSpec::Sink) {
                    self.add_leaf(spec);
                }
            }
            EgressCommand::Remove(output_id) => {
                self.remove_leaf_by_output(&output_id);
            }
            EgressCommand::FeedWake => self.enqueue_all_leaves(),
            EgressCommand::DrainShard(_) | EgressCommand::Shutdown => {}
        }
        EgressShardCommandEffect::Continue
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        self.drain_ready_leaves();
        EgressShardCommandEffect::Continue
    }

    fn on_shutdown(&mut self) {
        let leaves: Vec<Option<SinkFabricLeaf>> = self.leaves.drain(..).collect();
        for leaf in leaves.into_iter().flatten() {
            let mut leaf = leaf;
            leaf.engine
                .close(&mut leaf.transport, CloseReason::ShardShutdown);
        }
        self.output_leaves.clear();
        let _: VecDeque<LeafKey> = self.ready.drain().collect();
    }
}

#[cfg(test)]
mod tests;
