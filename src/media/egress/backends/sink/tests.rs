use super::*;
use crate::media::egress::backend::Readiness;
use crate::media::egress::journal::{FeedEpoch, RingFeed};
use crate::media::egress::leaf::ProgressState;
use crate::media::egress::metrics::LeafMetrics;
use crate::media::egress::test_driver::FakeFeed;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::RingBuffer;
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
fn sink_does_not_retain_ring_payload_after_discard_visit() {
    let ring = Arc::new(RingBuffer::new(4));
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: Bytes::from_static(b"discard-me"),
    });
    let retained = ring.read_at(0).unwrap();
    let feed = RingFeed::new(ring, Arc::new(FeedEpoch::new()));
    let mut engine = SinkEngine::<RingFeed>::default();
    let mut transport = SinkTransport::default();
    let mut cursor = FeedCursor::new(0, 0);

    assert_eq!(Arc::strong_count(&retained), 2);
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
            bytes: 10,
            units: 1,
            interest: Interest::NONE,
        }
    ));
    assert_eq!(cursor, FeedCursor::new(0, 1));
    assert_eq!(transport.stats().discarded_units, 1);
    assert_eq!(Arc::strong_count(&retained), 2);
}

#[test]
fn sink_discard_stats_update_discard_metrics_not_sent_metrics() {
    let feed = FakeFeed::new();
    feed.push(Bytes::from_static(b"abc"), true);
    feed.push(Bytes::from_static(b"de"), false);
    let mut engine = SinkEngine::<FakeFeed>::default();
    let mut transport = SinkTransport::default();
    let mut cursor = FeedCursor::new(0, 0);
    let mut metrics = LeafMetrics::default();
    let mut progress_state = ProgressState::new();

    let progress = engine.advance(
        &mut transport,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(8, 1024),
    );
    let stats = transport.stats();
    metrics.record_discarded(stats.discarded_bytes, stats.discarded_units);
    progress_state.record_discard(
        stats.discarded_bytes as usize,
        stats.discarded_units as usize,
    );

    assert!(matches!(progress, EngineProgress::Progress { .. }));
    assert!(progress_state.last_byte_progress.is_some());
    assert!(progress_state.last_protocol_progress.is_some());
    assert_eq!(progress_state.total_bytes_sent, 0);
    assert_eq!(progress_state.total_units_sent, 0);
    assert_eq!(progress_state.total_bytes_discarded, 5);
    assert_eq!(progress_state.total_units_discarded, 2);
    assert_eq!(metrics.bytes_sent, 0);
    assert_eq!(metrics.units_sent, 0);
    assert_eq!(metrics.bytes_discarded, 5);
    assert_eq!(metrics.units_discarded, 2);
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
