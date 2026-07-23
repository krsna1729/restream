use super::*;
use crate::media::egress::feed::ReadBudget;
use crate::media::packet::{MediaType, PayloadFormat};
use tokio_util::sync::CancellationToken;

// -----------------------------------------------------------------------
// WakeGate
// -----------------------------------------------------------------------

#[test]
fn wake_gate_set_and_take() {
    let gate = WakeGate::new();
    assert!(!gate.take());
    assert!(gate.notify());
    assert!(gate.is_pending());
    assert!(gate.take());
    assert!(!gate.take());
}

/// Only the clear-to-set transition obligates a wake delivery; repeats
/// coalesce until the consumer takes the flag.
#[test]
fn wake_gate_coalesces_repeat_notifies() {
    let gate = WakeGate::new();
    assert!(gate.notify());
    assert!(!gate.notify());
    assert!(!gate.notify());
    assert!(gate.take());
    assert!(!gate.take());
    // After the consumer drains, the next notify transitions again.
    assert!(gate.notify());
}

/// ABA-window safety: notify between take and re-read must leave data visible
/// and deliver a fresh wake.
#[test]
fn wake_gate_aba_window() {
    let gate = WakeGate::new();
    // Shard about to sleep: takes the flag (false → nothing pending yet).
    assert!(!gate.take());
    // Publisher pushes AFTER shard cleared flag — transition delivers a wake.
    assert!(gate.notify());
    // Shard re-reads feed head and must see new data — the flag is set.
    assert!(gate.is_pending());
    // Next cycle: shard takes it.
    assert!(gate.take());
}

// -----------------------------------------------------------------------
// FeedEpoch
// -----------------------------------------------------------------------

#[test]
fn feed_epoch_advances_monotonically() {
    let ep = FeedEpoch::new();
    assert_eq!(ep.current(), 0);
    assert_eq!(ep.advance(), 1);
    assert_eq!(ep.current(), 1);
    assert_eq!(ep.advance(), 2);
}

// -----------------------------------------------------------------------
// RingFeed
// -----------------------------------------------------------------------

fn push_packet_at(ring: &RingBuffer, payload: &[u8], is_keyframe: bool, dts: i64) {
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: dts,
        dts,
        is_keyframe,
        format: PayloadFormat::Raw,
        payload: Bytes::copy_from_slice(payload),
    });
}

fn push_packet(ring: &RingBuffer, payload: &[u8], is_keyframe: bool) {
    push_packet_at(ring, payload, is_keyframe, 0);
}

#[test]
fn ring_feed_empty_when_nothing_pushed() {
    let ring = Arc::new(RingBuffer::new(16));
    let epoch = Arc::new(FeedEpoch::new());
    let feed = RingFeed::new(ring, epoch);
    let cursor = FeedCursor::new(0, 0);
    assert!(matches!(
        feed.read_from(cursor, ReadBudget::default()),
        FeedRead::Empty
    ));
}

#[test]
fn ring_feed_reads_pushed_packets() {
    let ring = Arc::new(RingBuffer::new(16));
    let epoch = Arc::new(FeedEpoch::new());
    push_packet(&ring, b"hello", true);
    push_packet(&ring, b"world", false);

    let feed = RingFeed::new(ring, epoch);
    let cursor = FeedCursor::new(0, 0);
    match feed.read_from(cursor, ReadBudget::default()) {
        FeedRead::Units { units, next_cursor } => {
            assert_eq!(units.len(), 2);
            assert_eq!(next_cursor.next_sequence, 2);
            assert_eq!(&*units[0].payload, b"hello");
        }
        other => panic!("expected Units, got {other:?}"),
    }
}

#[test]
fn ring_feed_epoch_mismatch() {
    let ring = Arc::new(RingBuffer::new(16));
    let epoch = Arc::new(FeedEpoch::new());
    push_packet(&ring, b"data", false);
    epoch.advance(); // now epoch = 1

    let feed = RingFeed::new(ring, epoch);
    let old_cursor = FeedCursor::new(0, 0);
    assert!(matches!(
        feed.read_from(old_cursor, ReadBudget::default()),
        FeedRead::EpochMismatch { current_epoch: 1 }
    ));
}

#[test]
fn ring_feed_head_sequence_matches_write_idx() {
    let ring = Arc::new(RingBuffer::new(16));
    let epoch = Arc::new(FeedEpoch::new());
    let feed = RingFeed::new(ring.clone(), epoch);
    assert_eq!(feed.head_sequence(), 0);
    push_packet(&ring, b"a", false);
    assert_eq!(feed.head_sequence(), 1);
    push_packet(&ring, b"b", false);
    assert_eq!(feed.head_sequence(), 2);
}

#[test]
fn ring_feed_respects_unit_budget() {
    let ring = Arc::new(RingBuffer::new(32));
    let epoch = Arc::new(FeedEpoch::new());
    for i in 0..10 {
        push_packet(&ring, &[i as u8; 8], false);
    }
    let feed = RingFeed::new(ring, epoch);
    let cursor = FeedCursor::new(0, 0);
    match feed.read_from(cursor, ReadBudget::new(3, usize::MAX)) {
        FeedRead::Units { units, .. } => assert!(units.len() <= 3),
        other => panic!("expected Units, got {other:?}"),
    }
}

#[test]
fn ring_feed_retention_snapshot_tracks_retained_bytes_and_media_age() {
    let ring = Arc::new(RingBuffer::new(4));
    let epoch = Arc::new(FeedEpoch::new());
    for i in 0..6 {
        push_packet_at(&ring, &[i as u8; 3], i == 4, i * 10);
    }

    let feed = RingFeed::new(ring, epoch);
    let snapshot = feed.retention_snapshot();

    assert_eq!(snapshot.head_sequence, 6);
    assert_eq!(snapshot.oldest_sequence, 2);
    assert_eq!(snapshot.retained_units, 4);
    assert_eq!(snapshot.retained_bytes, 12);
    assert_eq!(snapshot.media_age_ms, Some(30));
    assert_eq!(snapshot.largest_unit_bytes, 3);
}

#[test]
fn ring_feed_limit_status_reports_strict_retention_violations() {
    let ring = Arc::new(RingBuffer::new(4));
    let epoch = Arc::new(FeedEpoch::new());
    push_packet_at(&ring, &[0; 2], true, 10);
    push_packet_at(&ring, &[1; 8], false, 80);

    let feed = RingFeed::new(ring, epoch);
    let limits = FeedLimits {
        max_retained_bytes: 9,
        max_retained_units: 1,
        max_retained_media_age: Duration::from_millis(50),
        max_unit_bytes: 4,
    };
    let status = feed.limit_status(&limits);

    assert!(status.retained_bytes_exceeded);
    assert!(status.retained_units_exceeded);
    assert!(status.media_age_exceeded);
    assert_eq!(status.oversized_unit_count, 1);
    assert!(!status.is_within_limits());
}

#[test]
fn ring_feed_reads_single_oversized_unit_under_nonzero_byte_budget() {
    let ring = Arc::new(RingBuffer::new(4));
    let epoch = Arc::new(FeedEpoch::new());
    push_packet(&ring, &[7; 8], true);

    let feed = RingFeed::new(ring, epoch);
    match feed.read_from(FeedCursor::new(0, 0), ReadBudget::new(8, 4)) {
        FeedRead::Units { units, next_cursor } => {
            assert_eq!(units.len(), 1);
            assert_eq!(units[0].payload.len(), 8);
            assert_eq!(next_cursor, FeedCursor::new(0, 1));
        }
        other => panic!("expected oversized unit to be admitted, got {other:?}"),
    }
}

#[test]
fn ring_feed_reports_overrun_at_retention_boundary() {
    let ring = Arc::new(RingBuffer::new(4));
    let epoch = Arc::new(FeedEpoch::new());
    for i in 0..6 {
        push_packet(&ring, &[i as u8], i == 4);
    }

    let feed = RingFeed::new(ring, epoch);
    assert_eq!(feed.head_sequence(), 6);
    assert_eq!(feed.oldest_sequence(), 2);
    assert!(matches!(
        feed.read_from(FeedCursor::new(0, 1), ReadBudget::default()),
        FeedRead::Overrun { oldest_sequence: 2 }
    ));
}

#[test]
fn ring_feed_finds_sync_points_without_registered_readers_after_wraparound() {
    let ring = Arc::new(RingBuffer::new(4));
    let epoch = Arc::new(FeedEpoch::new());
    for i in 0..6 {
        push_packet(&ring, &[i as u8], i == 4);
    }

    let feed = RingFeed::new(ring, epoch);
    assert_eq!(feed.latest_sync_point(), Some(FeedCursor::new(0, 4)));
    assert_eq!(feed.sync_point_at_or_after(2), Some(FeedCursor::new(0, 4)));
    assert_eq!(feed.sync_point_at_or_after(5), None);
}

#[test]
fn ring_feed_many_leaf_cursors_share_payload_storage() {
    let ring = Arc::new(RingBuffer::new(16));
    let epoch = Arc::new(FeedEpoch::new());
    push_packet(&ring, b"shared-payload", true);

    let feed = RingFeed::new(ring.clone(), epoch);
    let retained = ring.read_at(0).expect("packet should be retained");
    assert_eq!(Arc::strong_count(&retained), 2);

    let mut batches = Vec::new();
    for _ in 0..1_000 {
        match feed.read_from(FeedCursor::new(0, 0), ReadBudget::new(1, usize::MAX)) {
            FeedRead::Units { units, .. } => batches.push(units),
            other => panic!("expected shared unit batch, got {other:?}"),
        }
    }

    assert_eq!(Arc::strong_count(&retained), 1_002);
    assert!(
        batches
            .iter()
            .all(|units| Arc::ptr_eq(&units[0], &retained))
    );
}

#[test]
fn wake_gate_coalesces_one_publication_per_shard() {
    let shard_gates: Vec<WakeGate> = (0..1_000).map(|_| WakeGate::new()).collect();

    let deliveries: usize = shard_gates
        .iter()
        .map(|gate| usize::from(gate.notify()) + usize::from(gate.notify()))
        .sum();

    // Two publications coalesce to exactly one delivered wake per shard gate.
    assert_eq!(deliveries, 1_000);
    assert_eq!(
        shard_gates.iter().filter(|gate| gate.is_pending()).count(),
        1_000
    );
    assert_eq!(shard_gates.iter().filter(|gate| gate.take()).count(), 1_000);
    assert_eq!(shard_gates.iter().filter(|gate| gate.take()).count(), 0);
}

// -----------------------------------------------------------------------
// TsFeed
// -----------------------------------------------------------------------

#[test]
fn ts_feed_reads_pushed_chunks() {
    let cancel = CancellationToken::new();
    let ts_ring = TsChunkRing::new(16, cancel);
    ts_ring.push(Bytes::from_static(b"chunk1"), true);
    ts_ring.push(Bytes::from_static(b"chunk2"), false);

    let epoch = Arc::new(FeedEpoch::new());
    let feed = TsFeed::new(&ts_ring, epoch);
    let cursor = FeedCursor::new(0, 0);
    match feed.read_from(cursor, ReadBudget::default()) {
        FeedRead::Units { units, next_cursor } => {
            assert_eq!(units.len(), 2);
            assert_eq!(next_cursor.next_sequence, 2);
            assert_eq!(units[0].as_ref(), b"chunk1" as &[u8]);
        }
        other => panic!("expected Units, got {other:?}"),
    }
}

#[test]
fn ts_feed_empty_initially() {
    let cancel = CancellationToken::new();
    let ts_ring = TsChunkRing::new(16, cancel);
    let epoch = Arc::new(FeedEpoch::new());
    let feed = TsFeed::new(&ts_ring, epoch);
    let cursor = FeedCursor::new(0, 0);
    assert!(matches!(
        feed.read_from(cursor, ReadBudget::default()),
        FeedRead::Empty
    ));
}

#[test]
fn ts_feed_epoch_mismatch_on_advance() {
    let cancel = CancellationToken::new();
    let ts_ring = TsChunkRing::new(16, cancel);
    ts_ring.push(Bytes::from_static(b"data"), false);
    let epoch = Arc::new(FeedEpoch::new());
    epoch.advance();
    let feed = TsFeed::new(&ts_ring, epoch);
    let old_cursor = FeedCursor::new(0, 0);
    assert!(matches!(
        feed.read_from(old_cursor, ReadBudget::default()),
        FeedRead::EpochMismatch { current_epoch: 1 }
    ));
}

#[test]
fn ts_feed_retention_snapshot_tracks_retained_bytes() {
    let cancel = CancellationToken::new();
    let ts_ring = TsChunkRing::new(4, cancel);
    for i in 0..6 {
        ts_ring.push(Bytes::from(vec![i as u8; 2]), i == 4);
    }

    let epoch = Arc::new(FeedEpoch::new());
    let feed = TsFeed::new(&ts_ring, epoch);
    let snapshot = feed.retention_snapshot();

    assert_eq!(snapshot.head_sequence, 6);
    assert_eq!(snapshot.oldest_sequence, 2);
    assert_eq!(snapshot.retained_units, 4);
    assert_eq!(snapshot.retained_bytes, 8);
    assert_eq!(snapshot.media_age_ms, Some(0));
    assert_eq!(snapshot.largest_unit_bytes, 2);
}

#[test]
fn ts_feed_reports_overrun_at_retention_boundary() {
    let cancel = CancellationToken::new();
    let ts_ring = TsChunkRing::new(4, cancel);
    for i in 0..6 {
        ts_ring.push(Bytes::from(vec![i as u8]), i == 4);
    }

    let epoch = Arc::new(FeedEpoch::new());
    let feed = TsFeed::new(&ts_ring, epoch);
    assert_eq!(feed.head_sequence(), 6);
    assert_eq!(feed.oldest_sequence(), 2);
    assert!(matches!(
        feed.read_from(FeedCursor::new(0, 1), ReadBudget::default()),
        FeedRead::Overrun { oldest_sequence: 2 }
    ));
}

#[test]
fn ts_feed_finds_sync_points_without_registered_readers_after_wraparound() {
    let cancel = CancellationToken::new();
    let ts_ring = TsChunkRing::new(4, cancel);
    for i in 0..6 {
        ts_ring.push(Bytes::from(vec![i as u8]), i == 4);
    }

    let epoch = Arc::new(FeedEpoch::new());
    let feed = TsFeed::new(&ts_ring, epoch);
    assert_eq!(feed.latest_sync_point(), Some(FeedCursor::new(0, 4)));
    assert_eq!(feed.sync_point_at_or_after(2), Some(FeedCursor::new(0, 4)));
    assert_eq!(feed.sync_point_at_or_after(5), None);
}

// -----------------------------------------------------------------------
// FeedOverrunStats
// -----------------------------------------------------------------------

#[test]
fn overrun_stats_accumulate() {
    let mut stats = FeedOverrunStats::default();
    stats.record_overrun();
    stats.record_overrun();
    assert_eq!(stats.overrun_count, 2);
    assert!(stats.last_overrun_at.is_some());
    stats.record_epoch();
    assert_eq!(stats.epoch_count, 1);
    stats.record_oversized_unit();
    assert_eq!(stats.oversized_unit_count, 1);
}
