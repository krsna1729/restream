use super::*;
use crate::media::egress::visit::EngineVisitOutcome;

impl<T> SrtFabricLeaf<T>
where
    T: SrtMessageSender,
{
    pub(super) fn visit_ready(
        &mut self,
        generation: u64,
        readiness: Readiness,
        feed: &TsFeed,
        budget: WorkBudget,
    ) -> EngineVisitResult {
        if !self.common.is_current_generation(generation) {
            return EngineVisitResult::StaleGeneration;
        }
        self.transport.on_readiness(readiness);
        if self.transport.is_closed() {
            return EngineVisitResult::Visited(EngineVisitOutcome {
                progress: crate::media::egress::backend::EngineProgress::PeerClosed,
                decision: VisitDecision::Close,
            });
        }
        EngineVisit {
            generation,
            common: &mut self.common,
            engine: &mut self.engine,
            transport: &mut self.transport,
            readiness,
            feed,
            budget,
        }
        .run()
    }
}
