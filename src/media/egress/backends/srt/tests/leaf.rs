use super::super::*;
use super::support::{budget, common, feed, leaf};
use crate::media::egress::backend::{EngineProgress, Readiness};
use crate::media::egress::scheduler::VisitDecision;
use crate::media::egress::visit::EngineVisitResult;
use bytes::Bytes;

#[test]
fn srt_fabric_leaf_from_socket_keeps_native_sender_opaque() {
    let feed = feed([Bytes::from_static(b"abc")]);
    let mut leaf = srt_fabric_leaf_from_socket(common(9), 42);

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

/// Native-buffer accounting: a leaf whose application queue is drained but
/// whose native libsrt sender buffer still holds unacknowledged bytes is
/// backpressured, and those bytes count toward the leaf memory envelope.
#[test]
fn leaf_pressure_counts_native_backlog_beyond_app_pending() {
    let mut leaf = leaf(3);

    // Nothing anywhere: idle.
    let pressure = leaf.pressure();
    assert_eq!(pressure.pending_bytes(), 0);
    assert!(!pressure.is_backpressured());

    // App queue drained, native buffer saturated: still backpressured.
    leaf.transport_mut().native_backlog = Some(crate::media::srt::NativeSendBacklog {
        bytes: 1_316 * 8,
        packets: 8,
        ms: 40,
    });
    let pressure = leaf.pressure();
    assert_eq!(pressure.app_pending_bytes, 0);
    assert_eq!(pressure.pending_bytes(), 1_316 * 8);
    assert!(pressure.is_backpressured());
}
