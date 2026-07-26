#![allow(dead_code)]

//! Pipeline recirculation fabric shard backend: wires [`PipelineEngine`]
//! into [`EgressShardBackend`] — the production home
//! `crate::media::recirculation::start_pipeline_recirculation`'s plain
//! per-output `tokio::spawn` task never had (`docs/egress-implementation.md`
//! Phase 6a status). Structurally this mirrors `SinkShardBackend` closely
//! (same no-socket, `FeedWake`-is-the-only-readiness-signal shape — see
//! that module's doc comment) with one addition: a pipeline leaf needs its
//! claimed target ring and `IngestRegistration` *before* it can be added,
//! and claiming a target input is an async, fallible `MediaEngine` call —
//! not something a shard-thread `on_command` handler may do. The
//! application layer resolves it and hands it off through
//! [`PipelineTargetSource`], the same seam RTMP's publish-startup
//! snapshot uses for the identical reason.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::media::egress::backend::{CloseReason, ProtocolEngine, Readiness};
use crate::media::egress::command::{EgressCommand, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::RingFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::{LeafLimits, WorkBudget};
use crate::media::egress::scheduler::{LeafKey, ReadyQueue, VisitDecision, try_enqueue};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::egress::visit::{EngineVisit, EngineVisitResult};

use super::pipeline::{PipelineEngine, PipelineTarget, PipelineTransport};

/// Ready leaves drained per `on_media_tick` — see `SinkShardBackend`'s
/// identical constant for why this can be generous (no I/O per visit).
const MEDIA_TICK_VISIT_BUDGET: usize = 256;

pub(crate) trait PipelineTargetSource {
    fn take_target(&mut self, output_id: &OutputId) -> Option<PipelineTarget>;
}

/// Never supplies a target — every `Add` is rejected. Correct default
/// until the application-layer source is wired in, matching
/// `EmptyRtmpPublishStartupSource`'s role for RTMP.
#[derive(Debug, Default)]
pub(crate) struct EmptyPipelineTargetSource;

impl PipelineTargetSource for EmptyPipelineTargetSource {
    fn take_target(&mut self, _output_id: &OutputId) -> Option<PipelineTarget> {
        None
    }
}

/// Real source backed by a shared map: the application layer claims the
/// target input (async, fallible — see
/// `EgressTask::run_pipeline_fabric`), then calls [`Self::set`] before
/// dispatching `EgressCommand::Add` for that output. One instance is
/// shared (cloned) across every shard of a fabric runtime.
#[derive(Clone, Default)]
pub(crate) struct SharedPipelineTargetSource {
    pending: Arc<Mutex<HashMap<OutputId, PipelineTarget>>>,
}

impl SharedPipelineTargetSource {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn set(&self, output_id: OutputId, target: PipelineTarget) {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(output_id, target);
    }
}

impl PipelineTargetSource for SharedPipelineTargetSource {
    fn take_target(&mut self, output_id: &OutputId) -> Option<PipelineTarget> {
        self.pending
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(output_id)
    }
}

struct PipelineFabricLeaf {
    common: LeafCommon,
    engine: PipelineEngine<RingFeed>,
    transport: PipelineTransport,
}

impl PipelineFabricLeaf {
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

pub(crate) struct PipelineShardBackend<S = EmptyPipelineTargetSource>
where
    S: PipelineTargetSource,
{
    feed: RingFeed,
    budget_max_units: usize,
    budget_max_bytes: usize,
    budget_window: Duration,
    target_source: S,
    leaves: Vec<Option<PipelineFabricLeaf>>,
    output_leaves: HashMap<OutputId, LeafKey>,
    ready: ReadyQueue,
}

impl<S> PipelineShardBackend<S>
where
    S: PipelineTargetSource,
{
    pub(crate) fn new(feed: RingFeed, budget: WorkBudget, target_source: S) -> Self {
        let budget_window = budget
            .deadline
            .saturating_duration_since(std::time::Instant::now());
        Self {
            feed,
            budget_max_units: budget.max_units,
            budget_max_bytes: budget.max_bytes,
            budget_window,
            target_source,
            leaves: Vec::new(),
            output_leaves: HashMap::new(),
            ready: ReadyQueue::new(),
        }
    }

    fn add_leaf(&mut self, spec: OutputSpec) {
        let output_id = spec.id.clone();
        let Some(target) = self.target_source.take_target(&output_id) else {
            tracing::warn!(output_id = %output_id, "pipeline fabric leaf rejected: no target available");
            return;
        };
        let common = LeafCommon::new(
            spec.id,
            spec.generation,
            spec.feed,
            LeafLimits::from_policy(&spec.policy),
        )
        .with_progress_sink(spec.progress);
        let key = LeafKey(self.leaves.len());
        self.leaves.push(Some(PipelineFabricLeaf {
            common,
            engine: PipelineEngine::default(),
            transport: PipelineTransport::new(target),
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

impl<S> EgressShardBackend for PipelineShardBackend<S>
where
    S: PipelineTargetSource + Send + 'static,
{
    fn on_command(&mut self, command: EgressCommand) -> EgressShardCommandEffect {
        match command {
            EgressCommand::Add(spec) | EgressCommand::Update(spec) => {
                if matches!(spec.protocol, ProtocolSpec::Pipeline { .. }) {
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

    fn on_media_tick(&mut self) {
        self.drain_ready_leaves();
    }

    fn on_shutdown(&mut self) {
        let leaves: Vec<Option<PipelineFabricLeaf>> = self.leaves.drain(..).collect();
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
