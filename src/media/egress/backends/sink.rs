use std::marker::PhantomData;

use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, Readiness, RecoveryCapability,
};
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::policy::WorkBudget;

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
    F::Unit: AsRef<[u8]>,
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
                let bytes = units.iter().map(|unit| unit.as_ref().len()).sum();
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::egress::backend::Readiness;
    use crate::media::egress::test_driver::FakeFeed;
    use bytes::Bytes;
    use std::time::Duration;

    fn budget(max_units: usize, max_bytes: usize) -> WorkBudget {
        WorkBudget::new(max_units, max_bytes, Duration::from_secs(1))
    }

    #[test]
    fn sink_discards_available_units_when_feed_has_data() {
        let feed = FakeFeed::new();
        feed.push(Bytes::from_static(b"abc"), true);
        feed.push(Bytes::from_static(b"de"), false);
        let mut engine = SinkEngine::<FakeFeed>::default();
        let mut transport = SinkTransport::default();
        let mut cursor = FeedCursor::new(0, 0);

        let progress = engine.advance(
            &mut transport,
            Readiness::WRITABLE,
            &feed,
            &mut cursor,
            budget(8, 1024),
        );

        assert!(matches!(
            progress,
            EngineProgress::Progress {
                bytes: 5,
                units: 2,
                interest: Interest::NONE,
            }
        ));
        assert_eq!(cursor, FeedCursor::new(0, 2));
        assert_eq!(
            transport.stats(),
            SinkDiscardStats {
                discarded_bytes: 5,
                discarded_units: 2,
                close_count: 0,
            }
        );
    }

    #[test]
    fn sink_respects_visit_budget_when_discarding() {
        let feed = FakeFeed::new();
        feed.push(Bytes::from_static(b"abc"), true);
        feed.push(Bytes::from_static(b"de"), false);
        let mut engine = SinkEngine::<FakeFeed>::default();
        let mut transport = SinkTransport::default();
        let mut cursor = FeedCursor::new(0, 0);

        let progress = engine.advance(
            &mut transport,
            Readiness::WRITABLE,
            &feed,
            &mut cursor,
            budget(1, 1024),
        );

        assert!(matches!(
            progress,
            EngineProgress::Progress {
                bytes: 3,
                units: 1,
                interest: Interest::NONE,
            }
        ));
        assert_eq!(cursor, FeedCursor::new(0, 1));
        assert_eq!(transport.stats().discarded_units, 1);
    }

    #[test]
    fn sink_suspends_when_feed_is_empty() {
        let feed = FakeFeed::new();
        let mut engine = SinkEngine::<FakeFeed>::default();
        let mut transport = SinkTransport::default();
        let mut cursor = FeedCursor::new(0, 0);

        let progress = engine.advance(
            &mut transport,
            Readiness::WRITABLE,
            &feed,
            &mut cursor,
            budget(8, 1024),
        );

        assert!(matches!(progress, EngineProgress::Needs(Interest::NONE)));
        assert_eq!(cursor, FeedCursor::new(0, 0));
        assert_eq!(transport.stats(), SinkDiscardStats::default());
    }

    #[test]
    fn sink_reports_feed_overrun_for_stale_cursor() {
        let feed = FakeFeed::new();
        feed.push(Bytes::from_static(b"abc"), true);
        feed.set_overrun_at(1);
        let mut engine = SinkEngine::<FakeFeed>::default();
        let mut transport = SinkTransport::default();
        let mut cursor = FeedCursor::new(0, 0);

        let progress = engine.advance(
            &mut transport,
            Readiness::WRITABLE,
            &feed,
            &mut cursor,
            budget(8, 1024),
        );

        assert!(matches!(progress, EngineProgress::FeedOverrun));
        assert_eq!(cursor, FeedCursor::new(0, 0));
        assert_eq!(transport.stats(), SinkDiscardStats::default());
    }

    #[test]
    fn sink_reports_feed_overrun_for_epoch_mismatch() {
        let feed = FakeFeed::new();
        feed.advance_epoch();
        let mut engine = SinkEngine::<FakeFeed>::default();
        let mut transport = SinkTransport::default();
        let mut cursor = FeedCursor::new(0, 0);

        let progress = engine.advance(
            &mut transport,
            Readiness::WRITABLE,
            &feed,
            &mut cursor,
            budget(8, 1024),
        );

        assert!(matches!(progress, EngineProgress::FeedOverrun));
        assert_eq!(cursor, FeedCursor::new(0, 0));
        assert_eq!(transport.stats(), SinkDiscardStats::default());
    }

    #[test]
    fn sink_close_records_diagnostic_count() {
        let mut engine = SinkEngine::<FakeFeed>::default();
        let mut transport = SinkTransport::default();

        engine.close(&mut transport, CloseReason::Removed);

        assert_eq!(transport.stats().close_count, 1);
        assert_eq!(
            engine.recovery_capability(),
            RecoveryCapability::InPlaceResync
        );
    }
}
