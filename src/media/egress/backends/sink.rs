use std::marker::PhantomData;
use std::sync::Arc;
use std::time::Duration;

use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, Readiness, RecoveryCapability,
};
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::journal::{FeedEpoch, RingFeed};
use crate::media::egress::policy::WorkBudget;
use crate::media::engine::{EgressRegistration, MediaEngine};
use crate::media::packet::MediaPacket;
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer};
use bytes::Bytes;

const SINK_VISIT_BUDGET: Duration = Duration::from_millis(1);

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

pub async fn start_sink_egress(
    output_id: String,
    ring: Arc<RingBuffer>,
    engine: Arc<MediaEngine>,
    registration: EgressRegistration,
) {
    let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
    let mut cursor = FeedCursor::new(feed.epoch(), feed.head_sequence());
    let mut discard_engine = SinkEngine::<RingFeed>::default();
    let mut transport = SinkTransport::default();
    let mut wake_reader = Reader::new_live(format!("sink_egress:{output_id}"), ring);

    engine
        .update_egress_phase_if_current(
            &output_id,
            &registration,
            crate::domain::state::EgressPhase::Discarding,
        )
        .await;

    loop {
        tokio::select! {
            _ = registration.cancel_token.cancelled() => break,
            _ = wake_reader.wait_for_data() => {
                drive_sink_until_blocked(
                    &output_id,
                    &registration,
                    &engine,
                    &feed,
                    &mut cursor,
                    &mut discard_engine,
                    &mut transport,
                )
                .await;
                wake_reader.sync_read_idx(cursor.next_sequence as usize);
            }
        }
    }

    discard_engine.close(&mut transport, CloseReason::Removed);
}

async fn drive_sink_until_blocked(
    output_id: &str,
    registration: &EgressRegistration,
    engine: &MediaEngine,
    feed: &RingFeed,
    cursor: &mut FeedCursor,
    discard_engine: &mut SinkEngine<RingFeed>,
    transport: &mut SinkTransport,
) {
    loop {
        match discard_engine.advance(
            transport,
            Readiness::WRITABLE,
            feed,
            cursor,
            WorkBudget::new(
                MEDIA_PULL_BURST_PACKETS,
                MEDIA_TS_BATCH_TARGET_BYTES,
                SINK_VISIT_BUDGET,
            ),
        ) {
            EngineProgress::Progress { units, .. } if units > 0 => {
                if !engine
                    .record_egress_discard_progress_if_current(output_id, registration)
                    .await
                {
                    break;
                }
            }
            EngineProgress::Progress { .. } | EngineProgress::Needs(_) => break,
            EngineProgress::FeedOverrun => {
                if let Some(sync_cursor) = feed.latest_sync_point() {
                    *cursor = sync_cursor;
                } else {
                    *cursor = FeedCursor::new(feed.epoch(), feed.head_sequence());
                }
            }
            EngineProgress::PeerClosed | EngineProgress::Failed(_) => break,
            EngineProgress::HandshakeComplete | EngineProgress::Yield => break,
        }
    }
}

#[cfg(test)]
mod tests;
