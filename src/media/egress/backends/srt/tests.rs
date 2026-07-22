use super::*;
use crate::media::egress::backend::{CloseReason, EngineProgress, Readiness};
use crate::media::egress::command::{FeedId, OutputId};
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::{LeafLimits, WorkBudget};
use crate::media::egress::scheduler::VisitDecision;
use crate::media::egress::visit::EngineVisitResult;
use crate::media::srt::{SrtMessageSender, SrtSendResult};
use crate::media::ts_chunk_ring::TsChunkRing;
use bytes::Bytes;
use std::time::Duration;
use std::{sync::Arc, vec::Vec};
use tokio_util::sync::CancellationToken;

#[derive(Default)]
struct FakeSender {
    sends: Vec<Bytes>,
    closed: u32,
}

impl SrtMessageSender for FakeSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        self.sends.push(message.clone());
        SrtSendResult::Accepted {
            bytes: message.len(),
        }
    }

    fn close(&mut self, _reason: CloseReason) {
        self.closed = self.closed.saturating_add(1);
    }
}

fn leaf(generation: u64) -> SrtFabricLeaf<FakeSender> {
    SrtFabricLeaf::new(
        LeafCommon::new(
            OutputId::new("out-srt"),
            generation,
            FeedId::new("feed-srt"),
            LeafLimits::default(),
        ),
        FakeSender::default(),
    )
}

fn feed(chunks: impl IntoIterator<Item = Bytes>) -> TsFeed {
    let ring = TsChunkRing::new(8, CancellationToken::new());
    for chunk in chunks {
        ring.push(chunk, true);
    }
    TsFeed::new(&ring, Arc::new(FeedEpoch::new()))
}

fn budget() -> WorkBudget {
    WorkBudget::new(8, 1024, Duration::from_millis(1))
}

#[test]
fn srt_fabric_leaf_from_socket_keeps_native_sender_opaque() {
    let feed = feed([Bytes::from_static(b"abc")]);
    let common = LeafCommon::new(
        OutputId::new("out-native-srt"),
        9,
        FeedId::new("feed-srt"),
        LeafLimits::default(),
    );
    let mut leaf = srt_fabric_leaf_from_socket(common, 42);

    let result = leaf.visit_ready(8, Readiness::WRITABLE, &feed, budget());

    assert!(matches!(result, EngineVisitResult::StaleGeneration));
    assert_eq!(leaf.common().cursor.next_sequence, 0);
    assert_eq!(leaf.common().progress.total_bytes_sent, 0);
}

#[test]
fn srt_fabric_leaf_visits_ready_ts_feed_through_common_engine_visit() {
    let feed = feed([Bytes::from_static(b"abc")]);
    let mut leaf = leaf(7);

    let result = leaf.visit_ready(7, Readiness::WRITABLE, &feed, budget());

    let EngineVisitResult::Visited(outcome) = result else {
        panic!("expected SRT fabric visit");
    };
    assert!(matches!(
        outcome.progress,
        EngineProgress::Progress {
            bytes: 3,
            units: 1,
            ..
        }
    ));
    assert_eq!(outcome.decision, VisitDecision::Continue);
    assert_eq!(leaf.common().cursor.next_sequence, 1);
    assert_eq!(leaf.common().progress.total_bytes_sent, 3);
    assert_eq!(leaf.common().progress.total_units_sent, 1);
    assert_eq!(leaf.pending_message_bytes(), 0);
}

#[test]
fn srt_fabric_leaf_ignores_stale_ready_generation() {
    let feed = feed([Bytes::from_static(b"abc")]);
    let mut leaf = leaf(9);

    let result = leaf.visit_ready(8, Readiness::WRITABLE, &feed, budget());

    assert!(matches!(result, EngineVisitResult::StaleGeneration));
    assert_eq!(leaf.common().cursor.next_sequence, 0);
    assert_eq!(leaf.common().progress.total_bytes_sent, 0);
}

#[test]
fn srt_visit_continue_decision_requests_requeue() {
    assert!(requeue_after_srt_visit(VisitDecision::Continue));
    assert!(!requeue_after_srt_visit(VisitDecision::Suspend));
    assert!(!requeue_after_srt_visit(VisitDecision::Close));
}
