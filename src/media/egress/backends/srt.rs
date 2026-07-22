#![allow(dead_code)]

use crate::media::egress::backend::Readiness;
use crate::media::egress::journal::TsFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::scheduler::VisitDecision;
use crate::media::egress::visit::{EngineVisit, EngineVisitResult};
use crate::media::srt::{SRTSOCKET, SrtEgressEngine, SrtMessageSender, srt_fabric_message_sender};

pub(crate) struct SrtFabricLeaf<T>
where
    T: SrtMessageSender,
{
    common: LeafCommon,
    engine: SrtEgressEngine<T>,
    transport: T,
}

impl<T> SrtFabricLeaf<T>
where
    T: SrtMessageSender,
{
    pub(crate) fn new(common: LeafCommon, transport: T) -> Self {
        Self {
            common,
            engine: SrtEgressEngine::default(),
            transport,
        }
    }

    pub(crate) fn common(&self) -> &LeafCommon {
        &self.common
    }

    pub(crate) fn pending_message_bytes(&self) -> usize {
        self.engine.pending_message_bytes()
    }

    pub(crate) fn visit_ready(
        &mut self,
        generation: u64,
        readiness: Readiness,
        feed: &TsFeed,
        budget: WorkBudget,
    ) -> EngineVisitResult {
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

pub(crate) fn requeue_after_srt_visit(decision: VisitDecision) -> bool {
    matches!(decision, VisitDecision::Continue)
}

pub(crate) fn srt_fabric_leaf_from_socket(
    common: LeafCommon,
    socket: SRTSOCKET,
) -> SrtFabricLeaf<impl SrtMessageSender> {
    SrtFabricLeaf::new(common, srt_fabric_message_sender(socket))
}

#[cfg(test)]
mod tests;
