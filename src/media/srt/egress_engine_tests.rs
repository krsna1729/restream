use bytes::Bytes;
use tokio_util::sync::CancellationToken;

use super::srt_egress_engine::*;
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
    assert!(matches!(
        second,
        EngineProgress::Progress {
            bytes: 3,
            units: 0,
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
