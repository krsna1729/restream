//! Sink fabric shard backend: wires [`SinkEngine`] into
//! [`EgressShardBackend`] on real shard OS threads, reached through
//! `EgressManager`/`EgressCommand` dispatch — the production counterpart to
//! the test-only `SinkHarnessBackend` (`crates/restream-engine/src/egress/shard/tests/sink.rs`).
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
use std::time::{Duration, Instant};

use crate::egress::backend::{CloseReason, ProtocolEngine, Readiness};
use crate::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::egress::delivery::DeliveryTracker;
use crate::egress::journal::RingFeed;
use crate::egress::leaf::LeafCommon;
use crate::egress::leaf_arena::{LeafArena, LeafKey};
use crate::egress::metrics::{ServiceAccumulator, ShardMetrics, ShardService};
use crate::egress::policy::{LeafLimits, WorkBudget, WorkBudgetConfig};
use crate::egress::scheduler::{ReadyQueue, VisitDecision};
use crate::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::egress::visit::{EngineVisit, EngineVisitResult};

use super::sink::{SinkEngine, SinkTransport};

/// Ready leaves drained per `on_media_tick`. Sink work is cheap (no I/O, no
/// syscalls) so this is generous compared to the poller-driven backends'
/// per-visit budgets — the limit exists to bound one tick's wall-clock
/// cost on a shard serving many sink outputs, not to ration a scarce
/// resource the way SRT/RTMP's connect or send budgets do.
const MEDIA_TICK_VISIT_BUDGET: usize = 256;
/// How often the shard rates its leaves' delivery, as the RTMP and SRT stall
/// sweeps do.
const SERVICE_SWEEP_EVERY: Duration = Duration::from_secs(1);

struct SinkFabricLeaf {
    common: LeafCommon,
    engine: SinkEngine<RingFeed>,
    transport: SinkTransport,
    delivery: DeliveryTracker,
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
    budget_config: WorkBudgetConfig,
    leaves: LeafArena<SinkFabricLeaf>,
    output_leaves: HashMap<OutputId, LeafKey>,
    ready: ReadyQueue,
    sweep: ServiceAccumulator,
    /// Leaves in sweep order: each sweep rates the next `begin`'s budget of
    /// them, so one tick's cost is bounded however many leaves there are.
    sweep_order: VecDeque<LeafKey>,
    /// The last completed rotation's summary, published in shard metrics:
    /// what the fabric's sizer prices this shard's outputs from.
    service: ShardService,
    next_sweep: Option<Instant>,
}

impl SinkShardBackend {
    pub(crate) fn new(feed: RingFeed, budget: WorkBudgetConfig) -> Self {
        Self {
            feed,
            budget_config: budget,
            leaves: LeafArena::with_capacity(0),
            output_leaves: HashMap::new(),
            ready: ReadyQueue::new(),
            sweep: ServiceAccumulator::default(),
            sweep_order: VecDeque::new(),
            service: ShardService::default(),
            next_sweep: None,
        }
        .with_leaf_capacity(crate::egress::shard::EgressShardConfig::DEFAULT_LEAF_CAPACITY)
    }

    pub(crate) fn with_leaf_capacity(mut self, capacity: usize) -> Self {
        self.leaves = LeafArena::with_capacity(capacity);
        self.ready = ReadyQueue::with_capacity(capacity);
        self
    }

    fn add_leaf(&mut self, spec: OutputSpec) {
        let output_id = spec.id.clone();
        let previous = self.output_leaves.get(&output_id).copied();
        if previous.is_none() && self.leaves.is_full() {
            tracing::warn!(
                output_id = %output_id,
                "sink fabric leaf rejected: shard leaf capacity exhausted"
            );
            spec.progress.mark_terminated_unexpectedly();
            return;
        }
        if let Some(previous) = previous {
            self.remove_leaf_key(previous);
        }
        let progress = spec.progress.clone();
        let common = LeafCommon::new(
            spec.id,
            spec.generation,
            spec.feed,
            LeafLimits::from_policy(&spec.policy),
        )
        .with_progress_sink(spec.progress);
        let Some(key) = self.leaves.insert_with(|_| SinkFabricLeaf {
            common,
            engine: SinkEngine::default(),
            transport: SinkTransport::default(),
            delivery: DeliveryTracker::default(),
        }) else {
            progress.mark_terminated_unexpectedly();
            return;
        };
        self.output_leaves.insert(output_id, key);
        self.sweep_order.push_back(key);
        self.enqueue(key);
    }

    fn enqueue(&mut self, key: LeafKey) {
        if self.leaves.get(key).is_some() {
            self.ready.push(key);
        }
    }

    fn remove_leaf_by_output(&mut self, output_id: &OutputId) {
        if let Some(key) = self.output_leaves.remove(output_id) {
            self.remove_leaf_key(key);
        }
    }

    fn remove_leaf_key(&mut self, key: LeafKey) {
        self.ready.remove(key);
        // As the RTMP and SRT sweeps: the key leaves the rotation, which
        // restarts so it neither revisits a leaf nor rates a gone one.
        self.sweep_order.retain(|queued| *queued != key);
        self.sweep.invalidate();
        self.service = ShardService::default();
        if let Some(mut leaf) = self.leaves.remove(key) {
            leaf.engine.close(&mut leaf.transport, CloseReason::Removed);
        }
    }

    /// The only readiness signal this backend has — see the module doc.
    /// Re-enqueues every leaf that isn't already pending a visit.
    fn enqueue_all_leaves(&mut self) {
        let ready = &mut self.ready;
        for (key, _) in self.leaves.iter() {
            ready.push(key);
        }
    }

    fn drain_ready_leaves(&mut self) {
        for _ in 0..MEDIA_TICK_VISIT_BUDGET {
            let Some(key) = self.ready.pop() else {
                return;
            };
            let Some(leaf) = self.leaves.get_mut(key) else {
                continue;
            };
            let generation = leaf.common.generation;
            let budget = self.budget_config.new_visit();
            let result = leaf.visit_ready(generation, &self.feed, budget);
            let decision = match result {
                EngineVisitResult::StaleGeneration => VisitDecision::Suspend,
                EngineVisitResult::Visited(outcome) => outcome.decision,
            };
            match decision {
                VisitDecision::Continue => self.enqueue(key),
                VisitDecision::Suspend => {}
                VisitDecision::Close => {
                    self.output_leaves.retain(|_, queued| *queued != key);
                    self.remove_leaf_key(key);
                }
            }
        }
    }

    /// Once a second, rate the next leaves in sweep order over each one's
    /// last closed delivery window: the payload bytes it consumed against the
    /// bytes the feed published. A leaf a saturated shard leaves behind
    /// resynchronizes past what it missed, so its ratio falls under the
    /// delivery floor. At most `begin`'s budget of leaves per sweep, as the
    /// RTMP and SRT stall sweeps; the summary changes when a rotation ends.
    fn sweep_service(&mut self, now: Instant) {
        if self.next_sweep.is_some_and(|due| now < due) {
            return;
        }
        self.next_sweep = Some(now + SERVICE_SWEEP_EVERY);
        let offered = self.feed.published_bytes();
        let visits = self.sweep.begin(self.sweep_order.len());
        for _ in 0..visits {
            let Some(key) = self.sweep_order.pop_front() else {
                break;
            };
            // Removal retains keys out of the rotation; a miss is defensive.
            let Some(leaf) = self.leaves.get_mut(key) else {
                self.sweep.add(false, None);
                continue;
            };
            let rates = leaf
                .delivery
                .sample(leaf.common.progress.total_bytes_sent, offered, now);
            self.sweep.add(false, rates.ratio.zip(rates.offered_bps));
            self.sweep_order.push_back(key);
        }
        if let Some(service) = self.sweep.finish(now) {
            self.service = service;
        }
    }
}

impl EgressShardBackend for SinkShardBackend {
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match command {
            EgressCommand::Add(spec) | EgressCommand::Update(spec) => {
                #[cfg(test)]
                if spec.id.as_str().starts_with(INJECTED_SHARD_PANIC_OUTPUT) {
                    std::panic::resume_unwind(Box::new("injected sink shard panic"));
                }
                if matches!(spec.protocol, ProtocolSpec::Sink) {
                    self.add_leaf(spec);
                }
            }
            EgressCommand::Remove(output_id) | EgressCommand::Abandon(output_id) => {
                self.remove_leaf_by_output(&output_id);
            }
            EgressCommand::FeedWake => self.enqueue_all_leaves(),
            EgressCommand::DrainShard(_) | EgressCommand::Shutdown => {}
        }
        EgressShardCommandEffect::Continue
    }

    fn on_media_tick(&mut self) -> EgressShardCommandEffect {
        self.drain_ready_leaves();
        self.sweep_service(Instant::now());
        EgressShardCommandEffect::Continue
    }

    fn observe_metrics(&self, metrics: &mut ShardMetrics) {
        metrics.service = self.service;
    }

    fn on_shutdown(&mut self) {
        self.ready.clear();
        self.sweep_order.clear();
        for mut leaf in self.leaves.drain() {
            leaf.engine
                .close(&mut leaf.transport, CloseReason::ShardShutdown);
        }
        self.output_leaves.clear();
    }
}

/// Adding an output whose id starts with this panics the whole shard thread.
#[cfg(test)]
pub(crate) const INJECTED_SHARD_PANIC_OUTPUT: &str = "restream-test-injected-shard-panic";

#[cfg(test)]
mod tests;
