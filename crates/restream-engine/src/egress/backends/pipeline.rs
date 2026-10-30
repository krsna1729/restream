//! Pipeline recirculation `ProtocolEngine`: publishes feed units into
//! another pipeline's ring buffer through the same
//! [`crate::recirculation::RecirculationInputPublisher`]
//! `start_pipeline_recirculation` uses, so timestamp mapping, standby-GOP
//! replay and input-gate handling are shared; here a shard-scheduled
//! `advance()` call drives it instead of a per-output `tokio::select!` loop.

use std::marker::PhantomData;
use std::sync::Arc;

use crate::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::egress::backend::{
    CloseReason, EngineProgress, ProtocolEngine, Readiness, RecoveryCapability, WaitCondition,
};
use crate::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::egress::journal::RingFeed;
use crate::egress::policy::WorkBudget;
use crate::engine::IngestRegistration;

use crate::recirculation::RecirculationInputPublisher;
use restream_sync::{MEDIA_PULL_BURST_PACKETS, RingBuffer};

/// Everything a pipeline leaf needs to publish into its claimed target
/// input — resolved asynchronously by the application layer (claiming the
/// target input is itself async and may be rejected) and delivered to the
/// shard thread before the leaf is added, the same way RTMP's publish
/// startup snapshot is (see `PipelineTargetSource`). `IngestRegistration`
/// does not implement `Debug`, so neither type here derives it.
#[derive(Clone)]
pub struct PipelineTarget {
    pub target_ring: Arc<RingBuffer>,
    pub input_registration: IngestRegistration,
}

pub(crate) struct PipelineTransport {
    target_ring: Arc<RingBuffer>,
    input_registration: IngestRegistration,
    publisher: RecirculationInputPublisher,
}

impl PipelineTransport {
    pub(crate) fn new(target: PipelineTarget) -> Self {
        Self {
            target_ring: target.target_ring,
            input_registration: target.input_registration,
            publisher: RecirculationInputPublisher::default(),
        }
    }
}

#[derive(Debug)]
pub(crate) struct PipelineEngine<F> {
    _feed: PhantomData<F>,
}

impl<F> Default for PipelineEngine<F> {
    fn default() -> Self {
        Self { _feed: PhantomData }
    }
}

impl ProtocolEngine for PipelineEngine<RingFeed> {
    type Feed = RingFeed;
    type Transport = PipelineTransport;

    fn advance(
        &mut self,
        transport: &mut PipelineTransport,
        _readiness: Readiness,
        feed: &RingFeed,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
    ) -> EngineProgress {
        let read_budget = ReadBudget::new(
            budget.max_units().min(MEDIA_PULL_BURST_PACKETS),
            budget.max_bytes().min(MEDIA_TS_BATCH_TARGET_BYTES),
        );
        match feed.read_from(*cursor, read_budget) {
            FeedRead::Units { units, next_cursor } => {
                let first_sequence = cursor.next_sequence as usize;
                *cursor = next_cursor;
                let outcome = transport.publisher.publish(
                    &feed.current_ring(),
                    first_sequence,
                    &units,
                    &transport.target_ring,
                    &transport.input_registration,
                );
                EngineProgress::Progress {
                    bytes: outcome.bytes,
                    units: outcome.units,
                    wait: WaitCondition::Feed,
                }
            }
            FeedRead::Empty => EngineProgress::Needs(WaitCondition::Feed),
            FeedRead::Overrun { .. } | FeedRead::EpochMismatch { .. } => {
                EngineProgress::FeedOverrun
            }
        }
    }

    fn close(&mut self, _transport: &mut PipelineTransport, _reason: CloseReason) {
        // Releasing the claimed target input is an async engine call
        // (`unregister_ingest_if_current`) the application layer owns —
        // see `EgressTask::run_pipeline_fabric` — the same split RTMP uses
        // for its publish-startup snapshot rather than reaching back into
        // `MediaEngine` from a shard thread.
    }

    fn recovery_capability(&self) -> RecoveryCapability {
        RecoveryCapability::InPlaceResync
    }
}

#[cfg(test)]
mod tests;
