use crate::media::egress::backend::Readiness;
use crate::media::egress::journal::TsFeed;
use crate::media::egress::policy::{LeafStallClass, WorkBudget, classify_stall};
use crate::media::egress::visit::{EngineVisit, EngineVisitResult};
use crate::media::srt::SrtOwner;

use super::{SrtFabricLeaf, SrtLeafPressure, apply_native_send_backlog};

impl<T> SrtFabricLeaf<T>
where
    T: super::SrtMessageSender,
{
    pub(crate) fn new(common: super::LeafCommon, transport: T) -> Self {
        Self {
            common,
            engine: super::SrtEgressEngine::default(),
            transport,
            last_native_backlog_bytes: 0,
            last_packets_sent_drop: 0,
            observed_since: std::time::Instant::now(),
            draining_since: None,
            draining_reason: None,
            handshake_permit: None,
        }
    }

    pub(crate) fn common(&self) -> &super::LeafCommon {
        &self.common
    }

    #[cfg(test)]
    pub(crate) fn pending_message_bytes(&self) -> usize {
        self.engine.pending_message_bytes()
    }

    #[cfg(test)]
    pub(crate) fn transport_mut(&mut self) -> &mut T {
        &mut self.transport
    }

    pub(crate) fn pressure_with_owner(&mut self, owner: &mut SrtOwner<'_>) -> SrtLeafPressure {
        SrtLeafPressure {
            app_pending_bytes: self.engine.pending_message_bytes(),
            native_backlog: self.transport.native_send_backlog_with_owner(owner),
        }
    }

    pub(crate) fn observe_stall_with_owner(
        &mut self,
        now: std::time::Instant,
        packets_sent_drop: Option<u64>,
        lag_units: u64,
        owner: &mut SrtOwner<'_>,
    ) -> LeafStallClass {
        let pressure = self.pressure_with_owner(owner);
        let native_bytes = pressure.native_backlog.map_or(0, |backlog| backlog.bytes);
        let drops = packets_sent_drop.unwrap_or(self.last_packets_sent_drop);
        if native_bytes < self.last_native_backlog_bytes && drops <= self.last_packets_sent_drop {
            self.common.progress.last_protocol_progress = Some(now);
        }
        self.last_native_backlog_bytes = native_bytes;
        if let Some(drops) = packets_sent_drop {
            self.last_packets_sent_drop = drops;
        }

        let last_progress = self
            .common
            .progress
            .last_byte_progress
            .into_iter()
            .chain(self.common.progress.last_protocol_progress)
            .max()
            .unwrap_or(self.observed_since);
        let age = now.saturating_duration_since(last_progress);
        classify_stall(
            pressure.pending_bytes(),
            age,
            lag_units,
            &self.common.limits,
        )
    }

    pub(crate) fn sample_quality_with_owner(
        &mut self,
        owner: &mut SrtOwner<'_>,
    ) -> Option<super::PublisherQuality> {
        let mut quality = self.transport.sender_quality_with_owner(owner)?;
        if let Some(backlog) = self.transport.native_send_backlog_with_owner(owner) {
            apply_native_send_backlog(&mut quality, backlog);
        }
        Some(quality)
    }

    pub(crate) fn visit_ready_with_owner(
        &mut self,
        generation: u64,
        readiness: Readiness,
        feed: &TsFeed,
        budget: WorkBudget,
        owner: &mut SrtOwner<'_>,
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
        .run_with(|engine, transport, readiness, feed, cursor, budget| {
            engine.advance_with_owner(transport, readiness, feed, cursor, budget, owner)
        })
    }
}

#[cfg(test)]
impl<T> SrtFabricLeaf<T>
where
    T: super::SrtMessageSender,
{
    pub(crate) fn pressure(&mut self) -> SrtLeafPressure {
        self.pressure_with_owner(&mut SrtOwner::empty())
    }

    pub(crate) fn observe_stall(
        &mut self,
        now: std::time::Instant,
        packets_sent_drop: Option<u64>,
        lag_units: u64,
    ) -> LeafStallClass {
        self.observe_stall_with_owner(now, packets_sent_drop, lag_units, &mut SrtOwner::empty())
    }

    pub(crate) fn sample_quality(
        &mut self,
        _now: std::time::Instant,
    ) -> Option<super::PublisherQuality> {
        self.sample_quality_with_owner(&mut SrtOwner::empty())
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
