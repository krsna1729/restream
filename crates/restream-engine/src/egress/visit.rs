use crate::egress::backend::{EngineProgress, ProtocolEngine, Readiness};
use crate::egress::feed::{EgressFeed, FeedCursor};
use crate::egress::leaf::LeafCommon;
use crate::egress::policy::WorkBudget;
use crate::egress::scheduler::VisitDecision;

pub struct EngineVisit<'a, E>
where
    E: ProtocolEngine,
{
    pub generation: u64,
    pub common: &'a mut LeafCommon,
    pub engine: &'a mut E,
    pub transport: &'a mut E::Transport,
    pub readiness: Readiness,
    pub feed: &'a E::Feed,
    pub budget: WorkBudget,
}

#[derive(Debug)]
pub enum EngineVisitResult {
    StaleGeneration,
    Visited(EngineVisitOutcome),
}

#[derive(Debug)]
pub struct EngineVisitOutcome {
    pub progress: EngineProgress,
    pub decision: VisitDecision,
}

impl<E> EngineVisit<'_, E>
where
    E: ProtocolEngine,
{
    pub fn run(self) -> EngineVisitResult {
        self.run_with(|engine, transport, readiness, feed, cursor, budget| {
            E::advance(engine, transport, readiness, feed, cursor, budget)
        })
    }

    /// Variant used by native transports that need to pass one owner-thread
    /// completion/submission context into the concrete protocol engine.
    pub fn run_with(
        self,
        advance: impl FnOnce(
            &mut E,
            &mut E::Transport,
            Readiness,
            &E::Feed,
            &mut FeedCursor,
            WorkBudget,
        ) -> EngineProgress,
    ) -> EngineVisitResult {
        visit_leaf(
            self.generation,
            self.common,
            self.feed,
            self.readiness,
            self.budget,
            |cursor, readiness, budget| {
                advance(
                    self.engine,
                    self.transport,
                    readiness,
                    self.feed,
                    cursor,
                    budget,
                )
            },
        )
    }
}

/// One scheduler visit of a leaf, independent of the engine's transport shape:
/// generation check, first-visit cursor priming, the engine's own advance, and
/// the shared progress-to-decision mapping. `EngineVisit::run_with` is this
/// function plus the `ProtocolEngine` plumbing; engines whose transport lives
/// outside the leaf (SRT: the shard's Owner) call it directly.
pub(crate) fn visit_leaf<F: EgressFeed>(
    generation: u64,
    common: &mut LeafCommon,
    feed: &F,
    readiness: Readiness,
    budget: WorkBudget,
    advance: impl FnOnce(&mut FeedCursor, Readiness, WorkBudget) -> EngineProgress,
) -> EngineVisitResult {
    if !common.is_current_generation(generation) {
        return EngineVisitResult::StaleGeneration;
    }
    let progress = common.advance(feed, readiness, budget, advance);
    let decision = common.apply_progress(&progress, feed);
    EngineVisitResult::Visited(EngineVisitOutcome { progress, decision })
}
