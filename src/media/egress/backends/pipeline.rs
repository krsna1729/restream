//! Pipeline recirculation `ProtocolEngine`: publishes feed units into
//! another pipeline's ring buffer through the same
//! [`crate::media::recirculation::RecirculationInputPublisher`] the
//! pre-fabric implementation used — the timestamp mapping, standby-GOP
//! replay, and input-gate handling logic is unchanged and untouched here,
//! only *what drives it* changes (a shard-scheduled `advance()` call
//! instead of a per-output `tokio::select!` loop).

use std::marker::PhantomData;
use std::sync::Arc;

use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, Readiness, RecoveryCapability,
};
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::policy::WorkBudget;
use crate::media::engine::IngestRegistration;

use crate::media::packet::MediaPacket;
use crate::media::recirculation::RecirculationInputPublisher;
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, RingBuffer};

/// Everything a pipeline leaf needs to publish into its claimed target
/// input — resolved asynchronously by the application layer (claiming the
/// target input is itself async and may be rejected) and delivered to the
/// shard thread before the leaf is added, the same way RTMP's publish
/// startup snapshot is (see `PipelineTargetSource`). `IngestRegistration`
/// does not implement `Debug`, so neither type here derives it.
pub(crate) struct PipelineTarget {
    pub(crate) target_ring: Arc<RingBuffer>,
    pub(crate) input_registration: IngestRegistration,
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

impl<F> ProtocolEngine for PipelineEngine<F>
where
    F: EgressFeed<Unit = Arc<MediaPacket>>,
{
    type Feed = F;
    type Transport = PipelineTransport;

    fn advance(
        &mut self,
        transport: &mut PipelineTransport,
        _readiness: Readiness,
        feed: &F,
        cursor: &mut FeedCursor,
        budget: WorkBudget,
    ) -> EngineProgress {
        let read_budget = ReadBudget::new(
            budget.max_units.min(MEDIA_PULL_BURST_PACKETS),
            budget.max_bytes.min(MEDIA_TS_BATCH_TARGET_BYTES),
        );
        match feed.read_from(*cursor, read_budget) {
            FeedRead::Units { units, next_cursor } => {
                *cursor = next_cursor;
                let outcome = transport.publisher.publish(
                    &units,
                    &transport.target_ring,
                    &transport.input_registration,
                );
                EngineProgress::Progress {
                    bytes: outcome.bytes,
                    units: outcome.units,
                    interest: Interest::NONE,
                }
            }
            FeedRead::Empty => EngineProgress::Needs(Interest::NONE),
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
