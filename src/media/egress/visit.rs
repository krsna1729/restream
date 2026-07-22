use crate::media::egress::backend::{EngineProgress, ProtocolEngine, Readiness};
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::scheduler::VisitDecision;

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
        if !self.common.is_current_generation(self.generation) {
            return EngineVisitResult::StaleGeneration;
        }

        self.common.schedule.enqueued = false;
        let progress = self.engine.advance(
            self.transport,
            self.readiness,
            self.feed,
            &mut self.common.cursor,
            self.budget,
        );
        let decision = apply_progress_to_common(self.common, &progress);

        EngineVisitResult::Visited(EngineVisitOutcome { progress, decision })
    }
}

fn apply_progress_to_common(common: &mut LeafCommon, progress: &EngineProgress) -> VisitDecision {
    match progress {
        EngineProgress::Progress { bytes, units, .. } => {
            common.progress.record_send(*bytes, *units);
            common.schedule.mark_serviced();
            VisitDecision::Continue
        }
        EngineProgress::Needs(_) => {
            common.schedule.mark_serviced();
            VisitDecision::Suspend
        }
        EngineProgress::HandshakeComplete => {
            common.schedule.mark_serviced();
            VisitDecision::Continue
        }
        EngineProgress::FeedOverrun => {
            common.progress.record_overrun();
            VisitDecision::Close
        }
        EngineProgress::PeerClosed | EngineProgress::Failed(_) => VisitDecision::Close,
        EngineProgress::Yield => VisitDecision::Continue,
    }
}
