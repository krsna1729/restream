use super::*;
use crate::media::egress::backend::{Readiness, WaitCondition};
use crate::media::egress::journal::{FeedEpoch, RingFeed};
use crate::media::input_gate::InputPacketGate;
use crate::media::packet::{MediaType, PayloadFormat};
use bytes::Bytes;
use std::sync::atomic::AtomicI64;
use std::time::Duration;
use tokio_util::sync::CancellationToken;

fn budget() -> WorkBudget {
    WorkBudget::new(8, 4096, Duration::from_secs(1))
}

fn registration() -> IngestRegistration {
    IngestRegistration {
        cancel_token: CancellationToken::new(),
        attempt_id: 1,
        input_id: "recirculated-input".to_string(),
        gate: Arc::new(InputPacketGate::active()),
        last_forwarded_dts: Arc::new(AtomicI64::new(i64::MIN)),
        preview_ring: Arc::new(arc_swap::ArcSwapOption::empty()),
    }
}

fn transport(target_ring: Arc<RingBuffer>) -> PipelineTransport {
    PipelineTransport::new(PipelineTarget {
        target_ring,
        input_registration: registration(),
    })
}

fn packet(dts: i64, is_keyframe: bool) -> Arc<MediaPacket> {
    Arc::new(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe,
        track_index: 0,
        pts: dts,
        dts,
        payload: Bytes::from_static(&[1, 2, 3, 4]),
    })
}

/// Proves the fabric engine actually publishes into the target ring
/// through the unmodified `RecirculationInputPublisher` — the shard
/// scheduler only changes what calls `advance()`, not the publish logic
/// itself.
#[test]
fn pipeline_engine_publishes_available_units_into_the_target_ring() {
    let source_ring = Arc::new(RingBuffer::new(8));
    source_ring.push((*packet(10, true)).clone());
    source_ring.push((*packet(20, false)).clone());
    let feed = RingFeed::new(source_ring, Arc::new(FeedEpoch::new()));
    let target_ring = Arc::new(RingBuffer::new(8));
    let mut transport = transport(target_ring.clone());
    let mut engine = PipelineEngine::<RingFeed>::default();
    let mut cursor = FeedCursor::new(feed.epoch(), 0);

    let progress = engine.advance(
        &mut transport,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    assert!(matches!(
        progress,
        EngineProgress::Progress {
            units: 2,
            bytes: 8,
            ..
        }
    ));
    assert_eq!(target_ring.read_at(0).unwrap().dts, 10);
    assert_eq!(target_ring.read_at(1).unwrap().dts, 20);
}

#[test]
fn pipeline_engine_reports_needs_when_the_feed_is_empty() {
    let source_ring = Arc::new(RingBuffer::new(8));
    let feed = RingFeed::new(source_ring, Arc::new(FeedEpoch::new()));
    let target_ring = Arc::new(RingBuffer::new(8));
    let mut transport = transport(target_ring);
    let mut engine = PipelineEngine::<RingFeed>::default();
    let mut cursor = FeedCursor::new(feed.epoch(), 0);

    let progress = engine.advance(
        &mut transport,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    assert!(matches!(
        progress,
        EngineProgress::Needs(WaitCondition::Feed)
    ));
}
