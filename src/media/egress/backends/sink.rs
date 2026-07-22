use std::marker::PhantomData;
use std::sync::Arc;

use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, Readiness, RecoveryCapability,
};
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::policy::WorkBudget;
use crate::media::packet::MediaPacket;
use bytes::Bytes;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SinkDiscardStats {
    pub discarded_bytes: u64,
    pub discarded_units: u64,
    pub close_count: u32,
}

#[derive(Debug, Default)]
pub struct SinkTransport {
    stats: SinkDiscardStats,
}

impl SinkTransport {
    pub fn stats(&self) -> SinkDiscardStats {
        self.stats
    }

    fn record_discard(&mut self, bytes: usize, units: usize) {
        self.stats.discarded_bytes = self.stats.discarded_bytes.saturating_add(bytes as u64);
        self.stats.discarded_units = self.stats.discarded_units.saturating_add(units as u64);
    }

    fn record_close(&mut self) {
        self.stats.close_count = self.stats.close_count.saturating_add(1);
    }
}

#[derive(Debug)]
pub struct SinkEngine<F> {
    _feed: PhantomData<F>,
}

impl<F> Default for SinkEngine<F> {
    fn default() -> Self {
        Self { _feed: PhantomData }
    }
}

impl<F> ProtocolEngine for SinkEngine<F>
where
    F: EgressFeed,
    F::Unit: SinkDiscardUnit,
{
    type Feed = F;
    type Transport = SinkTransport;

    fn advance(
        &mut self,
        transport: &mut SinkTransport,
        _readiness: Readiness,
        feed: &F,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
    ) -> EngineProgress {
        let read_budget = ReadBudget::new(budget.max_units, budget.max_bytes);
        match feed.read_from(*cursor, read_budget) {
            FeedRead::Units { units, next_cursor } => {
                let bytes = units.iter().map(SinkDiscardUnit::discard_len).sum();
                let unit_count = units.len();
                *cursor = next_cursor;
                transport.record_discard(bytes, unit_count);
                EngineProgress::Progress {
                    bytes,
                    units: unit_count,
                    interest: Interest::NONE,
                }
            }
            FeedRead::Empty => EngineProgress::Needs(Interest::NONE),
            FeedRead::Overrun { .. } | FeedRead::EpochMismatch { .. } => {
                EngineProgress::FeedOverrun
            }
        }
    }

    fn close(&mut self, transport: &mut SinkTransport, _reason: CloseReason) {
        transport.record_close();
    }

    fn recovery_capability(&self) -> RecoveryCapability {
        RecoveryCapability::InPlaceResync
    }
}

pub(crate) trait SinkDiscardUnit {
    fn discard_len(&self) -> usize;
}

impl SinkDiscardUnit for Bytes {
    fn discard_len(&self) -> usize {
        self.len()
    }
}

impl SinkDiscardUnit for Arc<MediaPacket> {
    fn discard_len(&self) -> usize {
        self.payload.len()
    }
}

#[cfg(test)]
mod tests;
