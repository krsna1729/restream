use super::backend::{EngineProgress, Interest, ProtocolFailure, Readiness};
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
        interest: Interest::WRITE,
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
    let mut engine = FakeEngine::new(vec![EngineScript::Needs(Interest::WRITE)]);
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
    assert!(matches!(outcome.progress, EngineProgress::Needs(interest) if interest.writable));
    assert_eq!(outcome.decision, VisitDecision::Suspend);
    assert!(!common.schedule.enqueued);
    assert!(common.schedule.last_service_at.is_some());
}

#[test]
fn resynchronizes_and_records_overrun_instead_of_closing() {
    let feed = FakeFeed::new();
    feed.push(bytes::Bytes::from_static(b"sync"), true);
    let sync_point = feed.latest_sync_point().expect("keyframe was pushed");
    let mut common = common(2);
    common.cursor = FeedCursor::new(0, 999); // stale position past the overrun boundary
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
}

#[test]
fn resync_falls_back_to_oldest_sequence_without_a_sync_point() {
    let feed = FakeFeed::new();
    let mut common = common(2);
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
        FeedCursor::new(feed.epoch(), feed.oldest_sequence())
    );
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
        interest: Interest::WRITE,
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
    assert_eq!(engine.advance_calls, 0);
    assert_eq!(transport.bytes_written, 0);
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
