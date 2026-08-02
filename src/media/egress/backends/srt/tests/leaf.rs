use super::super::*;
use super::support::{budget, common, feed, leaf};
use crate::media::egress::backend::{EngineProgress, Readiness};
use crate::media::egress::scheduler::VisitDecision;
use crate::media::egress::visit::EngineVisitResult;
use crate::media::srt::SrtTraceBStats;
use bytes::Bytes;
use std::time::Instant;

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

/// Stall classification per the native-buffer accounting rule: a declining
/// native backlog is protocol progress (peer acknowledged data), so slow
/// native drain reads as backpressured; a native buffer that holds data
/// without declining past the no-progress deadline reads as stalled.
#[test]
fn observe_stall_uses_native_drain_as_progress() {
    use crate::media::srt::NativeSendBacklog;
    use std::time::{Duration, Instant};

    let deadline = leaf(3).common().limits.max_backpressure_duration;
    let mut leaf = leaf(3);
    let start = Instant::now();

    // Saturated native buffer, no progress recorded yet.
    leaf.transport_mut().native_backlog = Some(NativeSendBacklog {
        bytes: 10_000,
        packets: 8,
        ms: 40,
    });
    assert_eq!(leaf.observe_stall(start), LeafStallClass::Backpressured);

    // Backlog declines: native progress resets the stall clock even though
    // the application sent nothing new.
    leaf.transport_mut().native_backlog = Some(NativeSendBacklog {
        bytes: 6_000,
        packets: 5,
        ms: 25,
    });
    let near_deadline = start + deadline - Duration::from_millis(1);
    assert_eq!(
        leaf.observe_stall(near_deadline),
        LeafStallClass::Backpressured
    );

    // No further decline: the clock runs from the last native drain and the
    // leaf crosses into stalled after the full deadline.
    assert_eq!(
        leaf.observe_stall(near_deadline + deadline),
        LeafStallClass::Stalled
    );

    // Peer fully drains and nothing is pending anywhere: idle.
    leaf.transport_mut().native_backlog = Some(NativeSendBacklog::default());
    assert_eq!(
        leaf.observe_stall(near_deadline + deadline),
        LeafStallClass::Idle
    );
}

/// The gap this closes: fabric SRT egress never called `sender_quality_stats`
/// at all before this, so `ActiveEgress.quality` stayed at its all-`None`
/// default for every fabric-owned SRT output, unlike legacy egress which
/// sampled `srt_bistats` every second. Proves the leaf actually converts a
/// transport's raw stats into a real `PublisherQuality`.
#[test]
fn srt_fabric_leaf_samples_quality_from_transport_stats() {
    let mut leaf = leaf(1);
    leaf.transport_mut().quality_stats = Some(SrtTraceBStats {
        ms_rtt: 42.5,
        mbps_send_rate: 12.0,
        pkt_snd_loss_total: 3,
        ..unsafe { std::mem::zeroed() }
    });

    let quality = leaf
        .sample_quality(Instant::now())
        .expect("transport has quality stats");

    assert_eq!(quality.ms_rtt, Some(42.5));
    assert_eq!(quality.mbps_send_rate, Some(12.0));
    assert_eq!(quality.packets_sent_loss, Some(3));
}

/// A transport with nothing to report (no connected native socket) must not
/// fabricate an empty quality snapshot — the caller keeps whatever was
/// published last instead of clobbering it.
#[test]
fn srt_fabric_leaf_reports_no_quality_when_transport_has_none() {
    let mut leaf = leaf(1);
    assert!(leaf.sample_quality(Instant::now()).is_none());
}
