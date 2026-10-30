use super::*;
use crate::egress::backend::{Readiness, WaitCondition};
use crate::egress::journal::{FeedEpoch, RingFeed};
use crate::input_gate::InputPacketGate;
use bytes::Bytes;
use restream_core::{MediaPacket, MediaType, PayloadFormat};
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

/// A reader of the target ring sees each unit with the configuration the
/// source published it with, also when the change falls inside a later read
/// that does not start at sequence 0.
#[test]
fn pipeline_engine_copies_the_source_configuration_at_its_unit() {
    const CONFIG_A: &[u8] = &[0, 0, 0, 1, 0x67, 0x0a];
    const CONFIG_B: &[u8] = &[0, 0, 0, 1, 0x67, 0x0b];
    let source_ring = Arc::new(RingBuffer::new(8));
    source_ring.set_video_parameter_sets(CONFIG_A.to_vec());
    source_ring.push((*packet(10, true)).clone());
    source_ring.push((*packet(20, false)).clone());
    let feed = RingFeed::new(source_ring.clone(), Arc::new(FeedEpoch::new()));
    let target_ring = Arc::new(RingBuffer::new(8));
    let mut transport = transport(target_ring.clone());
    let mut engine = PipelineEngine::<RingFeed>::default();
    let mut cursor = FeedCursor::new(feed.epoch(), 0);
    engine.advance(
        &mut transport,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    source_ring.push((*packet(30, false)).clone());
    source_ring.set_video_parameter_sets(CONFIG_B.to_vec());
    source_ring.push((*packet(40, true)).clone());
    engine.advance(
        &mut transport,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    let configs: Vec<Vec<u8>> = (0..target_ring.get_write_idx())
        .map(|sequence| {
            target_ring
                .track_config_at(sequence, MediaType::Video, 0)
                .expect("retained")
                .expect("configured")
                .bytes
                .to_vec()
        })
        .collect();
    assert_eq!(configs, [CONFIG_A, CONFIG_A, CONFIG_A, CONFIG_B]);
}
