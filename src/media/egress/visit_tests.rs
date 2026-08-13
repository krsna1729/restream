use super::backend::{EngineProgress, Interest, ProtocolFailure, Readiness, WaitCondition};
use super::command::{FeedId, OutputId};
use super::feed::{EgressFeed, FeedCursor};
use super::leaf::LeafCommon;
use super::policy::{LeafLimits, WorkBudget};
use super::scheduler::VisitDecision;
use super::test_driver::{EngineScript, FakeEngine, FakeFeed, FakeTransport};
use super::visit::{EngineVisit, EngineVisitOutcome, EngineVisitResult};
use bytes::Bytes;
use std::time::Duration;

fn common(generation: u64) -> LeafCommon {
    LeafCommon::new(
        OutputId::new("out"),
        generation,
        FeedId::new("feed"),
        LeafLimits::default(),
    )
}

fn budget() -> WorkBudget {
    WorkBudget::new(4, 4096, Duration::from_millis(1))
}

#[test]
fn records_progress_and_continues_current_generation() {
    let feed = FakeFeed::new();
    feed.push(Bytes::from_static(b"abc"), true);
    let mut common = common(7);
    common.schedule.enqueued = true;
    let mut engine = FakeEngine::new(vec![EngineScript::Progress {
        bytes: 3,
        units: 1,
        wait: WaitCondition::Io(Interest::WRITE),
    }]);
    let mut transport = FakeTransport::default();

    let result = EngineVisit {
        generation: 7,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::WRITABLE,
        feed: &feed,
        budget: budget(),
    }
    .run();

    let EngineVisitResult::Visited(outcome) = result else {
        panic!("expected visit");
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
    assert_eq!(common.cursor, FeedCursor::new(0, 1));
    assert!(!common.schedule.enqueued);
    assert!(common.schedule.last_service_at.is_some());
    assert_eq!(common.progress.total_bytes_sent, 3);
    assert_eq!(common.progress.total_units_sent, 1);
}

#[test]
fn suspends_when_engine_needs_readiness() {
    let feed = FakeFeed::new();
    let mut common = common(1);
    common.schedule.enqueued = true;
    let mut engine = FakeEngine::new(vec![EngineScript::Needs(WaitCondition::Io(
        Interest::WRITE,
    ))]);
    let mut transport = FakeTransport::default();

    let result = EngineVisit {
        generation: 1,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::default(),
        feed: &feed,
        budget: budget(),
    }
    .run();

    let EngineVisitResult::Visited(outcome) = result else {
        panic!("expected visit");
    };
    assert!(matches!(outcome.progress, EngineProgress::Needs(wait) if wait.io_interest().writable));
    assert_eq!(outcome.decision, VisitDecision::Suspend);
    assert!(!common.schedule.enqueued);
    assert!(common.schedule.last_service_at.is_some());
}

/// Exhaustive proof of `apply_progress_to_common`'s `wants_feed_wake`
/// mapping: `Feed`/`FeedOrIo` (from either `Needs` or `Progress`) set it
/// `true`; `Io` and every non-wait-carrying `EngineProgress` variant set
/// it `false`. This is the flag `RtmpShardBackend`/`SrtShardBackend`'s
/// `enqueue_feed_waiting_leaves` reads to decide whether a `FeedWake`
/// should directly re-enqueue a leaf without any poller call.
#[test]
fn wants_feed_wake_reflects_the_visit_outcomes_wait_condition() {
    let feed = FakeFeed::new();

    let cases: Vec<(EngineScript, bool)> = vec![
        (EngineScript::Needs(WaitCondition::Feed), true),
        (
            EngineScript::Needs(WaitCondition::FeedOrIo(Interest::READ)),
            true,
        ),
        (
            EngineScript::Needs(WaitCondition::Io(Interest::WRITE)),
            false,
        ),
        (
            EngineScript::Progress {
                bytes: 1,
                units: 1,
                wait: WaitCondition::Feed,
            },
            true,
        ),
        (
            EngineScript::Progress {
                bytes: 1,
                units: 1,
                wait: WaitCondition::FeedOrIo(Interest::WRITE),
            },
            true,
        ),
        (
            EngineScript::Progress {
                bytes: 1,
                units: 1,
                wait: WaitCondition::Io(Interest::WRITE),
            },
            false,
        ),
        (EngineScript::HandshakeComplete, false),
        (EngineScript::PeerClosed, false),
        (EngineScript::Yield, false),
    ];

    for (script, expected) in cases {
        let mut common = common(1);
        common.schedule.enqueued = true;
        common.schedule.wants_feed_wake = !expected; // start opposite, prove it flips
        let mut engine = FakeEngine::new(vec![script]);
        let mut transport = FakeTransport::default();

        let result = EngineVisit {
            generation: 1,
            common: &mut common,
            engine: &mut engine,
            transport: &mut transport,
            readiness: Readiness::default(),
            feed: &feed,
            budget: budget(),
        }
        .run();

        assert!(matches!(result, EngineVisitResult::Visited(_)));
        assert_eq!(
            common.schedule.wants_feed_wake, expected,
            "wants_feed_wake mismatch"
        );
    }
}

#[test]
fn resynchronizes_and_records_overrun_instead_of_closing() {
    let feed = FakeFeed::new();
    feed.push(bytes::Bytes::from_static(b"sync"), true);
    let sync_point = feed.latest_sync_point().expect("keyframe was pushed");
    let mut common = common(2);
    common.cursor_primed = true; // already running; not a first-visit prime
    common.cursor = FeedCursor::new(0, 999); // stale position past the overrun boundary
    let resync_count = std::sync::Arc::new(std::sync::atomic::AtomicU64::new(0));
    common.progress_sink.resync_count = Some(resync_count.clone());
    let mut engine = FakeEngine::new(vec![EngineScript::FeedOverrun]);
    let mut transport = FakeTransport::default();

    let result = EngineVisit {
        generation: 2,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::WRITABLE,
        feed: &feed,
        budget: budget(),
    }
    .run();

    let EngineVisitResult::Visited(outcome) = result else {
        panic!("expected visit");
    };
    assert!(matches!(outcome.progress, EngineProgress::FeedOverrun));
    // Overrun resynchronizes in place: the connection and retry budget are
    // preserved instead of cycling through a reconnect for a transient
    // overrun, per the architecture's resync-at-sync-point requirement.
    assert_eq!(outcome.decision, VisitDecision::Continue);
    assert_eq!(common.progress.overrun_count, 1);
    assert_eq!(common.cursor, sync_point);
    // Cross-thread counter (API/alerts path) must also advance, not just
    // the shard-local `common.progress` counter.
    assert_eq!(resync_count.load(std::sync::atomic::Ordering::Relaxed), 1);
}

/// Without a retained sync point the resync target is the live edge, *not*
/// `oldest_sequence`. Rewinding to the oldest retained sequence is the largest
/// backward jump the feed allows: it lands mid-GOP, one publish away from
/// being overwritten again, and leaves the leaf a full retention window behind
/// live — which on SRT is silently dropped by `TLPKTDROP` instead of
/// delivered. This mirrors `RingBuffer::fast_forward`, which returns the write
/// index when no keyframe is retained.
#[test]
fn resync_falls_back_to_the_live_edge_without_a_sync_point() {
    let feed = FakeFeed::new();
    for _ in 0..40 {
        feed.push_str("unit", false); // no sync points at all
    }
    feed.set_overrun_at(32); // oldest retained = 32, head = 40
    let mut common = common(2);
    common.cursor_primed = true; // already running; not a first-visit prime
    common.cursor = FeedCursor::new(0, 999);
    let mut engine = FakeEngine::new(vec![EngineScript::FeedOverrun]);
    let mut transport = FakeTransport::default();

    let result = EngineVisit {
        generation: 2,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::WRITABLE,
        feed: &feed,
        budget: budget(),
    }
    .run();

    assert!(matches!(result, EngineVisitResult::Visited(_)));
    assert_eq!(
        common.cursor,
        FeedCursor::new(feed.epoch(), feed.head_sequence())
    );
    assert_ne!(common.cursor.next_sequence, feed.oldest_sequence());
}

#[test]
fn closes_failed_or_peer_closed_visits() {
    let feed = FakeFeed::new();
    for progress in [
        EngineScript::PeerClosed,
        EngineScript::Fail {
            reason: "failed",
            retryable: true,
        },
    ] {
        let mut common = common(3);
        let mut engine = FakeEngine::new(vec![progress]);
        let mut transport = FakeTransport::default();

        let result = EngineVisit {
            generation: 3,
            common: &mut common,
            engine: &mut engine,
            transport: &mut transport,
            readiness: Readiness::WRITABLE,
            feed: &feed,
            budget: budget(),
        }
        .run();

        let EngineVisitResult::Visited(outcome) = result else {
            panic!("expected visit");
        };
        assert_eq!(outcome.decision, VisitDecision::Close);
    }
}

#[test]
fn ignores_stale_generation_without_touching_engine_or_common_state() {
    let feed = FakeFeed::new();
    let mut common = common(5);
    common.schedule.enqueued = true;
    let mut engine = FakeEngine::new(vec![EngineScript::Progress {
        bytes: 3,
        units: 1,
        wait: WaitCondition::Io(Interest::WRITE),
    }]);
    let mut transport = FakeTransport::default();

    let result = EngineVisit {
        generation: 4,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::WRITABLE,
        feed: &feed,
        budget: budget(),
    }
    .run();

    assert!(matches!(result, EngineVisitResult::StaleGeneration));
    assert!(common.schedule.enqueued);
    assert_eq!(common.cursor, FeedCursor::new(0, 0));
    // A rejected visit must not consume the one-shot cursor priming either:
    // the leaf still has to be anchored by whichever visit actually runs.
    assert!(!common.cursor_primed);
    assert_eq!(engine.advance_calls, 0);
    assert_eq!(transport.bytes_written, 0);
}

/// The gap this closes: every leaf was constructed with `FeedCursor::new(0, 0)`
/// and nothing moved it before the first read, so on any established pipeline
/// — where the feed head is already far past the retention window — the first
/// read of every new leaf was a guaranteed `FeedRead::Overrun`, recovered from
/// whatever position the overrun handler happened to pick. Priming makes the
/// first read start from a valid live position instead.
#[test]
fn first_visit_primes_the_cursor_to_the_latest_sync_point() {
    let feed = FakeFeed::new();
    for seq in 0..40 {
        feed.push_str("unit", seq % 10 == 0); // sync points at 0, 10, 20, 30
    }
    feed.set_overrun_at(32); // oldest retained = 32, head = 40
    let mut common = common(1);
    assert_eq!(common.cursor, FeedCursor::new(0, 0));
    assert!(!common.cursor_primed);
    // `Needs` leaves the cursor alone, so what is asserted below is purely
    // the priming step, not anything the engine did.
    let mut engine = FakeEngine::new(vec![EngineScript::Needs(WaitCondition::Feed)]);
    let mut transport = FakeTransport::default();

    let result = EngineVisit {
        generation: 1,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::WRITABLE,
        feed: &feed,
        budget: budget(),
    }
    .run();

    assert!(matches!(result, EngineVisitResult::Visited(_)));
    assert_eq!(common.cursor, feed.latest_sync_point().expect("sync point"));
    assert_eq!(common.cursor.next_sequence, 30);
    assert!(common.cursor_primed);
    // Priming is not a failure: it must not be accounted as an overrun.
    assert_eq!(common.progress.overrun_count, 0);
}

/// A leaf created against a feed with no retained sync point (audio-only, or a
/// GOP longer than the retention window — the common case for a small
/// high-bitrate TS ring) starts at the live edge, never at `oldest_sequence`.
#[test]
fn first_visit_primes_to_the_live_edge_without_a_sync_point() {
    let feed = FakeFeed::new();
    for _ in 0..40 {
        feed.push_str("unit", false);
    }
    feed.set_overrun_at(32); // oldest retained = 32, head = 40
    let mut common = common(1);
    let mut engine = FakeEngine::new(vec![EngineScript::Needs(WaitCondition::Feed)]);
    let mut transport = FakeTransport::default();

    let result = EngineVisit {
        generation: 1,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::WRITABLE,
        feed: &feed,
        budget: budget(),
    }
    .run();

    assert!(matches!(result, EngineVisitResult::Visited(_)));
    assert_eq!(common.cursor, FeedCursor::new(0, 40));
    assert_ne!(common.cursor.next_sequence, feed.oldest_sequence());
}

/// Priming reads the feed's current epoch, so a leaf created before an epoch
/// bump does not start with a stale epoch and burn its first read on an
/// `EpochMismatch`.
#[test]
fn first_visit_primes_the_cursor_epoch_from_the_feed() {
    let feed = FakeFeed::new();
    feed.push_str("kf", true);
    feed.advance_epoch();
    let mut common = common(1);
    let mut engine = FakeEngine::new(vec![EngineScript::Needs(WaitCondition::Feed)]);
    let mut transport = FakeTransport::default();

    let result = EngineVisit {
        generation: 1,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::WRITABLE,
        feed: &feed,
        budget: budget(),
    }
    .run();

    assert!(matches!(result, EngineVisitResult::Visited(_)));
    assert_eq!(common.cursor.epoch, 1);
}

/// Priming is one-shot: once a leaf is reading, later visits must never rewind
/// it to the feed's sync point, or a leaf on a keyframe-dense feed would
/// re-anchor forward on every visit and drop everything in between.
#[test]
fn later_visits_do_not_re_prime_a_progressing_leaf() {
    let feed = FakeFeed::new();
    feed.push_str("kf", true);
    let mut common = common(1);
    let mut engine = FakeEngine::new(vec![
        EngineScript::Progress {
            bytes: 2,
            units: 1,
            wait: WaitCondition::Feed,
        },
        EngineScript::Needs(WaitCondition::Feed),
    ]);
    let mut transport = FakeTransport::default();

    for _ in 0..2 {
        // Between visits the feed publishes a new sync point; a re-prime
        // would jump the cursor forward to it and skip unsent units.
        let result = EngineVisit {
            generation: 1,
            common: &mut common,
            engine: &mut engine,
            transport: &mut transport,
            readiness: Readiness::WRITABLE,
            feed: &feed,
            budget: budget(),
        }
        .run();
        assert!(matches!(result, EngineVisitResult::Visited(_)));
        feed.push_str("kf", true);
    }

    // Primed to sync point 0, then advanced by exactly the one unit the
    // engine reported — not re-anchored to the newer sync point.
    assert_eq!(common.cursor, FeedCursor::new(0, 1));
}

/// Handshake and negotiation states never read the feed, so the anchor taken
/// on the first visit ages across the whole connect/handshake round trip. The
/// transition into a media-capable state re-anchors rather than handing the
/// publisher a cursor that is already behind live.
#[test]
fn handshake_completion_re_anchors_the_cursor_to_the_live_start() {
    let feed = FakeFeed::new();
    feed.push_str("kf", true);
    let mut common = common(1);
    let mut engine = FakeEngine::new(vec![
        EngineScript::Needs(WaitCondition::Io(Interest::WRITE)),
        EngineScript::HandshakeComplete,
    ]);
    let mut transport = FakeTransport::default();

    // First visit: primed at sequence 0 while still handshaking.
    let first = EngineVisit {
        generation: 1,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::WRITABLE,
        feed: &feed,
        budget: budget(),
    }
    .run();
    assert!(matches!(first, EngineVisitResult::Visited(_)));
    assert_eq!(common.cursor, FeedCursor::new(0, 0));

    // The feed moves on while the handshake is still in flight.
    for _ in 0..8 {
        feed.push_str("unit", false);
    }
    feed.push_str("kf", true);

    let second = EngineVisit {
        generation: 1,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::WRITABLE,
        feed: &feed,
        budget: budget(),
    }
    .run();

    let EngineVisitResult::Visited(outcome) = second else {
        panic!("expected visit");
    };
    assert!(matches!(
        outcome.progress,
        EngineProgress::HandshakeComplete
    ));
    assert_eq!(common.cursor, feed.latest_sync_point().expect("sync point"));
    assert_eq!(common.cursor.next_sequence, 9);
}

#[test]
fn failed_progress_remains_inspectable_by_caller() {
    let feed = FakeFeed::new();
    let mut common = common(6);
    let mut engine = FakeEngine::new(vec![EngineScript::Fail {
        reason: "socket",
        retryable: false,
    }]);
    let mut transport = FakeTransport::default();

    let result = EngineVisit {
        generation: 6,
        common: &mut common,
        engine: &mut engine,
        transport: &mut transport,
        readiness: Readiness::WRITABLE,
        feed: &feed,
        budget: budget(),
    }
    .run();

    let EngineVisitResult::Visited(EngineVisitOutcome {
        progress:
            EngineProgress::Failed(ProtocolFailure {
                reason, retryable, ..
            }),
        decision,
    }) = result
    else {
        panic!("expected failed visit");
    };
    assert_eq!(reason, "socket");
    assert!(!retryable);
    assert_eq!(decision, VisitDecision::Close);
}
