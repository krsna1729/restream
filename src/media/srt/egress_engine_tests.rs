use bytes::Bytes;
use tokio_util::sync::CancellationToken;

use super::srt_egress_engine::*;
use super::srt_egress_sender::*;
use crate::media::egress::backend::{
    CloseReason, EngineProgress, ProtocolEngine, Readiness, RecoveryCapability,
};
use crate::media::egress::feed::FeedCursor;
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::egress::policy::WorkBudget;
use crate::media::ts_chunk_ring::TsChunkRing;
use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

#[derive(Default)]
struct FakeSender {
    outcomes: VecDeque<SrtSendResult>,
    sent: Vec<Bytes>,
    closed: Vec<CloseReason>,
}

impl FakeSender {
    fn with_outcomes(outcomes: impl IntoIterator<Item = SrtSendResult>) -> Self {
        Self {
            outcomes: outcomes.into_iter().collect(),
            sent: Vec::new(),
            closed: Vec::new(),
        }
    }
}

impl SrtMessageSender for FakeSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        self.sent.push(message.clone());
        self.outcomes
            .pop_front()
            .unwrap_or(SrtSendResult::Accepted {
                bytes: message.len(),
            })
    }

    fn close(&mut self, reason: CloseReason) {
        self.closed.push(reason);
    }
}

fn feed_with_chunks(chunks: impl IntoIterator<Item = Bytes>) -> TsFeed {
    feed_with_capacity(8, chunks)
}

fn feed_with_capacity(capacity: usize, chunks: impl IntoIterator<Item = Bytes>) -> TsFeed {
    let ring = TsChunkRing::new(capacity, CancellationToken::new());
    for chunk in chunks {
        ring.push(chunk, true);
    }
    TsFeed::new(&ring, Arc::new(FeedEpoch::new()))
}

fn budget() -> WorkBudget {
    WorkBudget::new(8, 1024, Duration::from_millis(1))
}

#[test]
fn sends_one_ts_message_and_advances_cursor_when_writable() {
    let feed = feed_with_chunks([Bytes::from_static(b"abc"), Bytes::from_static(b"def")]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::default();
    let mut cursor = FeedCursor::new(0, 0);

    let progress = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    assert!(matches!(
        progress,
        EngineProgress::Progress {
            bytes: 3,
            units: 1,
            ..
        }
    ));
    assert_eq!(cursor, FeedCursor::new(0, 1));
    assert_eq!(sender.sent, vec![Bytes::from_static(b"abc")]);
    assert_eq!(engine.pending_message_bytes(), 0);
}

#[test]
fn retains_one_message_when_sender_backpressures() {
    let feed = feed_with_chunks([Bytes::from_static(b"abc"), Bytes::from_static(b"def")]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::with_outcomes([SrtSendResult::WouldBlock]);
    let mut cursor = FeedCursor::new(0, 0);

    let progress = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    assert!(matches!(progress, EngineProgress::Needs(interest) if interest.writable));
    assert_eq!(cursor, FeedCursor::new(0, 1));
    assert_eq!(sender.sent, vec![Bytes::from_static(b"abc")]);
    assert_eq!(engine.pending_message_bytes(), 3);
}

#[test]
fn writable_recovery_sends_pending_without_reading_next_feed_unit() {
    let feed = feed_with_chunks([Bytes::from_static(b"abc"), Bytes::from_static(b"def")]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::with_outcomes([
        SrtSendResult::WouldBlock,
        SrtSendResult::Accepted { bytes: 3 },
    ]);
    let mut cursor = FeedCursor::new(0, 0);

    let first = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );
    let second = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    assert!(matches!(first, EngineProgress::Needs(interest) if interest.writable));
    // The retry completes the retained unit (it fits in one fragment), so it
    // now correctly reports units: 1 — the engine fragments a unit into
    // MAX_SRT_MESSAGE_PAYLOAD-sized sends and only counts the unit once its
    // final fragment is accepted, on whichever visit that happens to be.
    assert!(matches!(
        second,
        EngineProgress::Progress {
            bytes: 3,
            units: 1,
            ..
        }
    ));
    assert_eq!(cursor, FeedCursor::new(0, 1));
    assert_eq!(
        sender.sent,
        vec![Bytes::from_static(b"abc"), Bytes::from_static(b"abc")]
    );
    assert_eq!(engine.pending_message_bytes(), 0);
}

#[test]
fn non_writable_visit_retains_without_transport_send() {
    let feed = feed_with_chunks([Bytes::from_static(b"abc")]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::default();
    let mut cursor = FeedCursor::new(0, 0);

    let progress = engine.advance(
        &mut sender,
        Readiness::default(),
        &feed,
        &mut cursor,
        budget(),
    );

    assert!(matches!(progress, EngineProgress::Needs(interest) if interest.writable));
    assert_eq!(cursor, FeedCursor::new(0, 1));
    assert!(sender.sent.is_empty());
    assert_eq!(engine.pending_message_bytes(), 3);
}

#[test]
fn feed_overrun_uses_common_engine_progress() {
    let feed = feed_with_capacity(
        1,
        [
            Bytes::from_static(b"one"),
            Bytes::from_static(b"two"),
            Bytes::from_static(b"three"),
        ],
    );
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::default();
    let mut cursor = FeedCursor::new(0, 0);

    let progress = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    assert!(matches!(progress, EngineProgress::FeedOverrun));
    assert_eq!(cursor, FeedCursor::new(0, 0));
}

#[test]
fn peer_close_maps_to_common_engine_progress() {
    let feed = feed_with_chunks([Bytes::from_static(b"abc")]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::with_outcomes([SrtSendResult::PeerClosed]);
    let mut cursor = FeedCursor::new(0, 0);

    let progress = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    assert!(matches!(progress, EngineProgress::PeerClosed));
    assert_eq!(cursor, FeedCursor::new(0, 1));
}

#[test]
fn send_failure_maps_to_common_protocol_failure() {
    let feed = feed_with_chunks([Bytes::from_static(b"abc")]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::with_outcomes([SrtSendResult::Failed(SrtSendFailure {
        reason: "srt_async_send",
        detail: "native send failed".to_string(),
        retryable: true,
    })]);
    let mut cursor = FeedCursor::new(0, 0);

    let progress = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    match progress {
        EngineProgress::Failed(failure) => {
            assert_eq!(failure.reason, "srt_async_send");
            assert_eq!(failure.detail, "native send failed");
            assert!(failure.retryable);
        }
        other => panic!("expected failure progress, got {other:?}"),
    }
    assert_eq!(cursor, FeedCursor::new(0, 1));
}

#[test]
fn close_drops_pending_message_and_delegates_transport_close() {
    let feed = feed_with_chunks([Bytes::from_static(b"abc")]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::default();
    let mut cursor = FeedCursor::new(0, 0);

    let _ = engine.advance(
        &mut sender,
        Readiness::default(),
        &feed,
        &mut cursor,
        budget(),
    );
    engine.close(&mut sender, CloseReason::Removed);

    assert_eq!(engine.pending_message_bytes(), 0);
    assert_eq!(sender.closed, vec![CloseReason::Removed]);
}

#[test]
fn srt_engine_uses_reconnect_resynchronization() {
    let engine = SrtEgressEngine::<FakeSender>::default();

    assert_eq!(
        engine.recovery_capability(),
        RecoveryCapability::ReconnectOnly
    );
}

/// Regression: an SRT message-mode `srt_send()` call fails (SRT error 5009,
/// "Incorrect use of Message API") when the payload exceeds the socket's
/// configured message size. A single muxed TS feed unit (one chunk boundary
/// from the shared muxer) can be much larger than that — a keyframe burst is
/// commonly tens of KB — so the engine must fragment a retained unit into
/// `MAX_SRT_MESSAGE_PAYLOAD`-sized pieces across multiple visits rather than
/// handing the whole unit to one `srt_send()` call. Live evidence: this was
/// the root cause of every SRT fabric output silently delivering zero bytes
/// despite successful connects, confirmed wake delivery, and confirmed
/// muxer production (see `docs/egress-implementation.md` Phase 4 status).
#[test]
fn large_feed_unit_is_fragmented_across_multiple_visits() {
    let big_unit = Bytes::from(vec![7u8; MAX_SRT_MESSAGE_PAYLOAD * 2 + 500]);
    let feed = feed_with_chunks([big_unit.clone()]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::default(); // always Accepted { bytes: message.len() }
    let mut cursor = FeedCursor::new(0, 0);

    // Three visits: two full-size fragments, then the 500-byte remainder.
    let first = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );
    let second = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );
    let third = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    assert!(matches!(
        first,
        EngineProgress::Progress {
            bytes: MAX_SRT_MESSAGE_PAYLOAD,
            units: 0,
            ..
        }
    ));
    assert!(matches!(
        second,
        EngineProgress::Progress {
            bytes: MAX_SRT_MESSAGE_PAYLOAD,
            units: 0,
            ..
        }
    ));
    assert!(matches!(
        third,
        EngineProgress::Progress {
            bytes: 500,
            units: 1,
            ..
        }
    ));

    // The cursor only advances once the whole unit is consumed, and every
    // fragment sent to the transport stays within the message-size limit.
    assert_eq!(cursor, FeedCursor::new(0, 1));
    assert!(
        sender
            .sent
            .iter()
            .all(|f| f.len() <= MAX_SRT_MESSAGE_PAYLOAD)
    );
    assert_eq!(
        sender.sent.iter().map(Bytes::len).sum::<usize>(),
        big_unit.len()
    );
    // Reassembling the sent fragments in order must reproduce the original
    // unit exactly -- no bytes dropped, duplicated, or reordered.
    let reassembled: Vec<u8> = sender.sent.iter().flat_map(|b| b.to_vec()).collect();
    assert_eq!(reassembled, big_unit.to_vec());
    assert_eq!(engine.pending_message_bytes(), 0);
}

/// A large unit that hits `WouldBlock` mid-fragmentation must resume from
/// the exact byte offset on the next writable visit, not restart or skip.
#[test]
fn large_feed_unit_resumes_at_the_correct_offset_after_would_block() {
    let big_unit = Bytes::from(vec![9u8; MAX_SRT_MESSAGE_PAYLOAD + 100]);
    let feed = feed_with_chunks([big_unit.clone()]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::with_outcomes([
        SrtSendResult::Accepted {
            bytes: MAX_SRT_MESSAGE_PAYLOAD,
        },
        SrtSendResult::WouldBlock,
        SrtSendResult::Accepted { bytes: 100 },
    ]);
    let mut cursor = FeedCursor::new(0, 0);

    let first = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );
    let second = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );
    let third = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        budget(),
    );

    assert!(matches!(
        first,
        EngineProgress::Progress {
            bytes: MAX_SRT_MESSAGE_PAYLOAD,
            units: 0,
            ..
        }
    ));
    assert!(matches!(second, EngineProgress::Needs(interest) if interest.writable));
    assert!(matches!(
        third,
        EngineProgress::Progress {
            bytes: 100,
            units: 1,
            ..
        }
    ));
    assert_eq!(sender.sent.last().unwrap().len(), 100);
    assert_eq!(cursor, FeedCursor::new(0, 1));
}

/// The CPU-regression fix: with a generous budget, multiple fragments of
/// the same large unit are sent within ONE visit (one `advance()` call)
/// instead of costing one scheduler cycle per fragment.
#[test]
fn generous_budget_batches_multiple_fragments_into_one_visit() {
    let big_unit = Bytes::from(vec![3u8; MAX_SRT_MESSAGE_PAYLOAD * 3]);
    let feed = feed_with_chunks([big_unit.clone()]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::default(); // always Accepted { bytes: message.len() }
    let mut cursor = FeedCursor::new(0, 0);
    let generous_budget = WorkBudget::new(64, 1024 * 1024, Duration::from_secs(1));

    let progress = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        generous_budget,
    );

    // All three fragments went out in this single visit.
    assert!(matches!(
        progress,
        EngineProgress::Progress {
            bytes,
            units: 1,
            ..
        } if bytes == big_unit.len()
    ));
    assert_eq!(sender.sent.len(), 3);
    assert!(
        sender
            .sent
            .iter()
            .all(|f| f.len() <= MAX_SRT_MESSAGE_PAYLOAD)
    );
    assert_eq!(cursor, FeedCursor::new(0, 1));
    assert_eq!(engine.pending_message_bytes(), 0);
}

/// A tight byte budget still stops the loop mid-unit within one visit,
/// leaving the rest for the next visit — the batching change does not
/// bypass the visit budget.
#[test]
fn tight_byte_budget_stops_batching_mid_unit() {
    let big_unit = Bytes::from(vec![5u8; MAX_SRT_MESSAGE_PAYLOAD * 3]);
    let feed = feed_with_chunks([big_unit]);
    let mut engine = SrtEgressEngine::<FakeSender>::default();
    let mut sender = FakeSender::default();
    let mut cursor = FeedCursor::new(0, 0);
    // The budget check runs after a fragment is sent, so exactly one
    // fragment goes out whenever max_bytes is at most one fragment's size:
    // sending it reaches total_bytes == MAX_SRT_MESSAGE_PAYLOAD >= max_bytes
    // and the loop stops before attempting a second fragment.
    let tight_budget = WorkBudget::new(64, MAX_SRT_MESSAGE_PAYLOAD, Duration::from_secs(1));

    let progress = engine.advance(
        &mut sender,
        Readiness::WRITABLE,
        &feed,
        &mut cursor,
        tight_budget,
    );

    assert!(matches!(
        progress,
        EngineProgress::Progress {
            bytes: MAX_SRT_MESSAGE_PAYLOAD,
            units: 0,
            ..
        }
    ));
    assert_eq!(sender.sent.len(), 1);
    assert_eq!(engine.pending_message_bytes(), MAX_SRT_MESSAGE_PAYLOAD * 2);
    // Cursor has not advanced -- the unit is still in flight.
    assert_eq!(cursor, FeedCursor::new(0, 1));
}
