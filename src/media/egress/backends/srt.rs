#![allow(dead_code)]

use std::collections::VecDeque;

use crate::media::egress::backend::{ProtocolEngine, Readiness};
use crate::media::egress::journal::TsFeed;
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::scheduler::{LeafKey, VisitDecision};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::media::egress::visit::{EngineVisit, EngineVisitResult};
use crate::media::srt::{
    SRTSOCKET, SrtEgressEngine, SrtEgressInterest, SrtEgressPollError, SrtEgressSendMode,
    SrtFabricPoller, SrtMessageSender, SrtReadyLeaf, srt_fabric_message_sender,
};

mod add_error;
mod socket_config;

pub(crate) use add_error::SrtBackendAddError;
pub(crate) use socket_config::{NativeSrtSocketConfigurator, SrtSocketConfigurator};

type NativeSrtLeaf = SrtFabricLeaf<Box<dyn SrtMessageSender + Send>>;

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

pub(crate) fn srt_fabric_leaf_from_socket(common: LeafCommon, socket: SRTSOCKET) -> NativeSrtLeaf {
    SrtFabricLeaf::new(common, srt_fabric_message_sender(socket))
}

pub(crate) trait SrtReadinessPoller {
    fn register_leaf(
        &mut self,
        socket: SRTSOCKET,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError>;

    fn remove(&mut self, socket: SRTSOCKET) -> Result<(), SrtEgressPollError>;

    fn poll_leaves(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError>;
}

impl SrtReadinessPoller for SrtFabricPoller {
    fn register_leaf(
        &mut self,
        socket: SRTSOCKET,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        self.register_leaf(socket, key, generation, interest)
    }

    fn remove(&mut self, socket: SRTSOCKET) -> Result<(), SrtEgressPollError> {
        self.remove(socket)
    }

    fn poll_leaves(
        &mut self,
        timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        self.poll_leaves(timeout_ms, ready)
    }
}

pub(crate) struct SrtShardBackend<P, C = NativeSrtSocketConfigurator>
where
    P: SrtReadinessPoller,
    C: SrtSocketConfigurator,
{
    poller: P,
    socket_configurator: C,
    feed: TsFeed,
    budget: WorkBudget,
    leaves: Vec<Option<NativeSrtLeaf>>,
    ready: VecDeque<SrtReadyLeaf>,
    poll_buffer: Vec<SrtReadyLeaf>,
}

impl<P> SrtShardBackend<P, NativeSrtSocketConfigurator>
where
    P: SrtReadinessPoller,
{
    pub(crate) fn new(poller: P, feed: TsFeed, budget: WorkBudget) -> Self {
        Self::with_socket_configurator(poller, feed, budget, NativeSrtSocketConfigurator)
    }
}

impl<P, C> SrtShardBackend<P, C>
where
    P: SrtReadinessPoller,
    C: SrtSocketConfigurator,
{
    pub(crate) fn with_socket_configurator(
        poller: P,
        feed: TsFeed,
        budget: WorkBudget,
        socket_configurator: C,
    ) -> Self {
        Self {
            poller,
            socket_configurator,
            feed,
            budget,
            leaves: Vec::new(),
            ready: VecDeque::new(),
            poll_buffer: Vec::new(),
        }
    }

    pub(crate) fn add_connected_socket(
        &mut self,
        common: LeafCommon,
        socket: SRTSOCKET,
    ) -> Result<LeafKey, SrtBackendAddError> {
        self.socket_configurator
            .configure_connected(socket, SrtEgressSendMode::FabricNonblocking)?;

        let key = LeafKey(self.leaves.len());
        self.poller
            .register_leaf(socket, key, common.generation, SrtEgressInterest::WRITE)
            .map_err(SrtBackendAddError::Poller)?;
        let leaf = srt_fabric_leaf_from_socket(common, socket);
        self.leaves.push(Some(leaf));
        Ok(key)
    }

    fn add_leaf(
        &mut self,
        socket: SRTSOCKET,
        leaf: NativeSrtLeaf,
    ) -> Result<LeafKey, SrtEgressPollError> {
        let key = LeafKey(self.leaves.len());
        self.poller.register_leaf(
            socket,
            key,
            leaf.common.generation,
            SrtEgressInterest::WRITE,
        )?;
        self.leaves.push(Some(leaf));
        Ok(key)
    }

    fn poll_ready(&mut self) {
        if self.poller.poll_leaves(0, &mut self.poll_buffer).is_err() {
            return;
        }

        let events: Vec<_> = self.poll_buffer.drain(..).collect();
        for event in events {
            let Some(leaf) = self.leaf_mut(event.key) else {
                continue;
            };
            if leaf.common.schedule.enqueued {
                continue;
            }
            leaf.common.schedule.enqueued = true;
            self.ready.push_back(event);
        }
    }

    fn leaf_mut(&mut self, key: LeafKey) -> Option<&mut NativeSrtLeaf> {
        self.leaves.get_mut(key.0).and_then(Option::as_mut)
    }

    fn visit_one_ready_leaf(&mut self) -> Option<VisitDecision> {
        let event = self.ready.pop_front()?;
        let budget = self.budget;
        let feed = &self.feed;
        let leaf = self.leaves.get_mut(event.key.0).and_then(Option::as_mut)?;
        let result = leaf.visit_ready(
            event.generation,
            Readiness {
                readable: false,
                writable: event.writable,
            },
            feed,
            budget,
        );

        match result {
            EngineVisitResult::StaleGeneration => Some(VisitDecision::Suspend),
            EngineVisitResult::Visited(outcome) => Some(outcome.decision),
        }
    }
}

impl<P, C> EgressShardBackend for SrtShardBackend<P, C>
where
    P: SrtReadinessPoller + Send + 'static,
    C: SrtSocketConfigurator + Send + 'static,
{
    fn on_command(
        &mut self,
        _command: crate::media::egress::command::EgressCommand,
    ) -> EgressShardCommandEffect {
        EgressShardCommandEffect::Continue
    }

    fn on_ready(&mut self) -> EgressShardCommandEffect {
        if self.ready.is_empty() {
            self.poll_ready();
        }

        if matches!(self.visit_one_ready_leaf(), Some(VisitDecision::Continue)) {
            EgressShardCommandEffect::ScheduleReady { count: 1 }
        } else {
            EgressShardCommandEffect::Continue
        }
    }

    fn on_shutdown(&mut self) {
        for leaf in &mut self.leaves {
            if let Some(leaf) = leaf.as_mut() {
                leaf.engine.close(
                    &mut leaf.transport,
                    crate::media::egress::backend::CloseReason::ShardShutdown,
                );
            }
        }
    }
}

#[cfg(test)]
mod tests;
