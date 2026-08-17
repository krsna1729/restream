use super::super::*;
use super::support::{budget, common, feed, leaf, wrapped_feed};
use crate::media::egress::backend::{EngineProgress, Readiness, WaitCondition};
use crate::media::egress::scheduler::VisitDecision;
use crate::media::egress::visit::EngineVisitResult;
use crate::media::srt::SrtSenderStats;
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
fn srt_fabric_leaf_forwards_readiness_to_transport() {
    let feed = feed([Bytes::from_static(b"abc")]);
    let mut leaf = leaf(7);

    let result = leaf.visit_ready(7, Readiness::BOTH, &feed, budget());

    assert!(matches!(result, EngineVisitResult::Visited(_)));
    assert_eq!(leaf.transport_mut().readiness, vec![Readiness::BOTH]);
}

/// The gap this closes, on the production `TsFeed` rather than a fake: an SRT
/// leaf added to a pipeline that has been running for a while starts against a
/// ring whose head is far past sequence 0. Every such leaf used to begin at
/// `(0, 0)`, so its first read was a guaranteed `FeedRead::Overrun` — one
/// resync WARN per leaf at startup, recovering from a position it should never
/// have held. The leaf must instead be primed onto the newest retained
/// keyframe and send from there.
#[test]
fn fresh_leaf_first_visit_starts_at_the_retained_keyframe_not_sequence_zero() {
    // 40 chunks through a capacity-8 ring: retained window is [32, 40), and
    // the newest retained keyframe is sequence 35.
    let feed = wrapped_feed(40, 5);
    let mut leaf = leaf(7);

    let result = leaf.visit_ready(7, Readiness::WRITABLE, &feed, budget());

    let EngineVisitResult::Visited(outcome) = result else {
        panic!("expected SRT fabric visit");
    };
    assert!(
        !matches!(outcome.progress, EngineProgress::FeedOverrun),
        "a freshly primed leaf must not overrun on its very first read"
    );
    assert_eq!(leaf.common().progress.overrun_count, 0);
    assert_eq!(
        leaf.transport_mut()
            .sends()
            .first()
            .map(|sent| sent.as_ref()),
        Some(b"chunk-35".as_ref()),
        "leaf should send from the newest retained keyframe"
    );
}

/// Same first-visit path when no keyframe is retained (the newest one has
/// already aged out of the window): the leaf starts at the live edge and waits
/// for the feed, rather than rewinding to `oldest_sequence` and running a full
/// retention window behind live.
#[test]
fn fresh_leaf_first_visit_starts_at_the_live_edge_when_no_keyframe_is_retained() {
    // Only sequence 0 is a keyframe, and it aged out 32 sequences ago.
    let feed = wrapped_feed(40, 1_000);
    let mut leaf = leaf(7);

    let result = leaf.visit_ready(7, Readiness::WRITABLE, &feed, budget());

    let EngineVisitResult::Visited(outcome) = result else {
        panic!("expected SRT fabric visit");
    };
    assert!(matches!(
        outcome.progress,
        EngineProgress::Needs(WaitCondition::Feed)
    ));
    assert_eq!(leaf.common().progress.overrun_count, 0);
    assert_eq!(leaf.common().cursor.next_sequence, 40);
    assert!(leaf.transport_mut().sends().is_empty());
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
    assert_eq!(
        leaf.observe_stall(start, None, 0),
        LeafStallClass::Backpressured
    );

    // Backlog declines: native progress resets the stall clock even though
    // the application sent nothing new.
    leaf.transport_mut().native_backlog = Some(NativeSendBacklog {
        bytes: 6_000,
        packets: 5,
        ms: 25,
    });
    let near_deadline = start + deadline - Duration::from_millis(1);
    assert_eq!(
        leaf.observe_stall(near_deadline, None, 0),
        LeafStallClass::Backpressured
    );

    // No further decline: the clock runs from the last native drain and the
    // leaf crosses into stalled after the full deadline.
    assert_eq!(
        leaf.observe_stall(near_deadline + deadline, None, 0),
        LeafStallClass::Stalled
    );

    // Peer fully drains and nothing is pending anywhere: idle.
    leaf.transport_mut().native_backlog = Some(NativeSendBacklog::default());
    assert_eq!(
        leaf.observe_stall(near_deadline + deadline, None, 0),
        LeafStallClass::Idle
    );
}

/// The gap this closes (report open item, "SRT stall-detection latent
/// bug"): TLPKTDROP/TSBPD-deadline discards also shrink the native buffer
/// head, so a backlog decline accompanied by drop-counter growth is not
/// the peer acknowledging data. A drop-riddled leaf (0 sent / millions
/// dropped) must reach Stalled after the no-progress deadline instead of
/// resetting its clock every sweep.
#[test]
fn observe_stall_does_not_count_drop_backlog_decline_as_progress() {
    use crate::media::srt::{NativeSendBacklog, SrtSenderStats};
    use std::time::{Duration, Instant};

    let deadline = leaf(3).common().limits.max_backpressure_duration;
    let mut leaf = leaf(3);
    let start = Instant::now();

    // First observation: saturated buffer, drop counter at 1,000.
    leaf.transport_mut().native_backlog = Some(NativeSendBacklog {
        bytes: 10_000,
        packets: 8,
        ms: 40,
    });
    leaf.transport_mut().quality_stats = Some(SrtSenderStats {
        packets_sent_drop_total: 1_000,
        ..SrtSenderStats::default()
    });
    assert_eq!(
        leaf.observe_stall(start, Some(1_000), 0),
        LeafStallClass::Backpressured
    );

    // Backlog declines — but only because the drop counter advanced
    // (TLPKTDROP discards). That is not progress: the stall clock keeps
    // running and the leaf is Stalled at the deadline.
    leaf.transport_mut().native_backlog = Some(NativeSendBacklog {
        bytes: 6_000,
        packets: 5,
        ms: 25,
    });
    leaf.transport_mut().quality_stats = Some(SrtSenderStats {
        packets_sent_drop_total: 1_037,
        ..SrtSenderStats::default()
    });
    let near_deadline = start + deadline - Duration::from_millis(1);
    assert_eq!(
        leaf.observe_stall(near_deadline, Some(1_037), 0),
        LeafStallClass::Backpressured
    );
    assert_eq!(
        leaf.observe_stall(near_deadline + deadline, Some(1_037), 0),
        LeafStallClass::Stalled
    );
}

/// The lag ceiling: a leaf whose read cursor is more than
/// `max_feed_lag_units` behind the head is Stalled even while its native
/// buffer drains — the ring has already advanced past its position, so
/// catch-up would be lossy.
#[test]
fn observe_stall_lag_over_limit_is_stalled_despite_native_drain() {
    use crate::media::srt::NativeSendBacklog;
    use std::time::Instant;

    let mut leaf = leaf(3);
    leaf.transport_mut().native_backlog = Some(NativeSendBacklog {
        bytes: 6_000,
        packets: 5,
        ms: 25,
    });
    let now = Instant::now();
    // Fresh progress (backlog declining, no drops) would read as
    // Backpressured — but the 301-unit lag overrides it.
    assert_eq!(
        leaf.observe_stall(now, Some(0), 301),
        LeafStallClass::Stalled
    );
    // At or under the ceiling, native drain still counts as progress.
    assert_eq!(
        leaf.observe_stall(now, Some(0), 300),
        LeafStallClass::Backpressured
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
    leaf.transport_mut().quality_stats = Some(SrtSenderStats {
        rtt_ms: 42.5,
        send_rate_mbps: 12.0,
        packets_sent_loss_total: 3,
        ..SrtSenderStats::default()
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
