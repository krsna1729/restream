//! Product-level close semantics: graceful drain of a removed leaf, stall
//! detection, and unexpected-termination reporting.

use super::super::*;
use super::support::*;
use bytes::Bytes;
use std::sync::atomic::Ordering;
use std::time::Duration;

/// A removed leaf that still has protocol backlog stays registered (draining)
/// so it can flush, and is force-closed once the drain deadline passes even
/// though its peer never acknowledges; an explicit `Remove` is not an
/// unexpected termination.
#[test]
fn removed_leaf_with_backlog_drains_then_force_closes_at_the_deadline() {
    let sink = SinkPeer::v4();
    let (spec, terminated) = srt_spec_with_flag("out", 1, &url_for(sink.addr));
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 16 * 1316]);
    harness.add_resolved(spec, vec![sink.addr]);
    assert!(harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 5));

    // Wedge the peer, keep the sender's buffer occupied, then remove.
    sink.set_stalled(true);
    for _ in 0..40 {
        harness.publish(unit.clone());
        harness.turn(Duration::from_millis(1));
    }
    harness
        .backend
        .on_command(EgressCommand::Remove(OutputId::new("out")));
    let key = harness
        .backend
        .output_sockets
        .get(&OutputId::new("out"))
        .copied();
    if let Some(key) = key {
        let leaf = harness.backend.leaves[key.0].as_ref().unwrap();
        assert!(
            leaf.draining_since.is_some(),
            "backlog keeps the leaf draining"
        );
    }

    let closed = harness.pump(Duration::from_secs(5), |backend| {
        backend.output_sockets.is_empty()
    });
    assert!(
        closed,
        "the drain deadline force-closes a leaf whose peer never flushes it"
    );
    assert!(harness.backend.callers.is_empty());
    assert!(
        !terminated.load(Ordering::Relaxed),
        "a requested removal is not unexpected"
    );
}

/// A leaf with nothing queued closes immediately on `Remove`.
#[test]
fn removed_idle_leaf_closes_immediately() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    harness.add_resolved(srt_spec("out", 1, &url_for(sink.addr)), vec![sink.addr]);
    assert!(harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 3));
    // Let the sender buffer drain fully.
    for _ in 0..200 {
        harness.turn(Duration::from_millis(2));
    }
    harness
        .backend
        .on_command(EgressCommand::Remove(OutputId::new("out")));
    assert!(harness.backend.output_sockets.is_empty());
    assert!(harness.backend.callers.is_empty());
}

/// A leaf whose peer disappears mid-stream is retired as an unexpected
/// termination once the no-progress deadline passes.
#[test]
fn stalled_leaf_is_closed_as_an_unexpected_termination() {
    let sink = SinkPeer::v4();
    let (mut spec, terminated) = srt_spec_with_flag("out", 1, &url_for(sink.addr));
    spec.policy.no_progress_timeout = Duration::from_millis(800);
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    harness.add_resolved(spec, vec![sink.addr]);
    assert!(harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 3));

    sink.set_stalled(true);
    let closed = harness.feed_until(&unit, Duration::from_secs(30), |backend| {
        backend.output_sockets.is_empty()
    });
    assert!(closed, "a wedged peer is eventually retired");
    assert!(
        terminated.load(Ordering::Relaxed),
        "and reported as unexpected"
    );
}
