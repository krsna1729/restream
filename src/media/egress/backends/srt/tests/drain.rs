//! Graceful-drain regression tests for `SrtShardBackend`, mirroring
//! `rtmp_shard_drain_tests.rs` exactly (same mechanism:
//! `begin_graceful_close`/`sweep_draining_leaves`/the opportunistic
//! flush-close in `visit_one_ready_leaf`), adapted to this module's
//! fake-sender test style (`leaf_termination.rs`'s `NeverDrainsSender`
//! pattern) since SRT sockets are native FFI, not fakeable at the OS level
//! the way `rtmp_shard_tests.rs` fakes a TCP peer.

use super::super::*;
use super::support::{common, feed};
use bytes::Bytes;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::media::egress::backend::CloseReason;
use crate::media::egress::command::{EgressCommand, OutputId};
use crate::media::egress::policy::WorkBudget;
use crate::media::srt::{NativeSendBacklog, SrtSendResult};

/// A sender whose native backlog is externally controllable, so a test can
/// simulate "pending bytes queued" then "flushed" without a real visit
/// (`observe_stall`/`pressure` read `native_send_backlog()` directly, not
/// `LeafCommon.pending_application_bytes`).
struct ControllableSender {
    native_backlog: Arc<Mutex<Option<NativeSendBacklog>>>,
}

impl SrtMessageSender for ControllableSender {
    fn send_message(&mut self, _message: &Bytes) -> SrtSendResult {
        SrtSendResult::WouldBlock
    }

    fn close(&mut self, _reason: CloseReason) {}

    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        *self.native_backlog.lock().unwrap()
    }
}

fn backlog(bytes: u64) -> NativeSendBacklog {
    NativeSendBacklog {
        bytes,
        packets: 3,
        ms: 500,
    }
}

fn backend_with_one_leaf() -> (
    SrtShardBackend,
    OutputId,
    Arc<Mutex<Option<NativeSendBacklog>>>,
) {
    let mut backend = SrtShardBackend::new(
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    let native_backlog = Arc::new(Mutex::new(Some(backlog(4_096))));
    let leaf_common = common(7);
    let output_id = leaf_common.output_id.clone();
    let leaf = SrtFabricLeaf::new(
        leaf_common,
        Box::new(ControllableSender {
            native_backlog: native_backlog.clone(),
        }) as Box<dyn SrtMessageSender + Send>,
    );
    backend.add_leaf(leaf);
    (backend, output_id, native_backlog)
}

#[test]
fn remove_defers_close_until_pending_bytes_are_flushed() {
    // Before this change, `Remove` called `remove_leaf_by_output`
    // immediately, tearing down the transport regardless of whatever the
    // engine or native libsrt sender buffer still had queued — that data
    // was silently lost. `begin_graceful_close` must instead keep a leaf
    // with a nonzero native backlog registered (not closed) until a later
    // sweep finds it flushed. Mirrors the identical RTMP test exactly.
    let (mut backend, output_id, native_backlog) = backend_with_one_leaf();

    backend.on_command(EgressCommand::Remove(output_id.clone()));

    assert!(
        backend.output_sockets.contains_key(&output_id),
        "a leaf with pending bytes must not be closed immediately on Remove"
    );
    {
        let key = *backend.output_sockets.get(&output_id).unwrap();
        let leaf = backend.leaves[key.0].as_ref().unwrap();
        assert!(
            leaf.draining_since.is_some(),
            "a deferred close must mark the leaf as draining"
        );
    }

    // Once flushed (native backlog reaches zero), the next sweep must close
    // it for real rather than leaving it registered forever.
    *native_backlog.lock().unwrap() = None;
    backend.sweep_draining_leaves(Instant::now());

    assert!(
        !backend.output_sockets.contains_key(&output_id),
        "a fully flushed draining leaf must close on the next sweep"
    );
}

#[test]
fn remove_closes_immediately_when_nothing_is_queued() {
    // The common case — a leaf with no pending bytes at removal time — must
    // not pay a drain delay it doesn't need.
    let (mut backend, output_id, native_backlog) = backend_with_one_leaf();
    *native_backlog.lock().unwrap() = None;

    backend.on_command(EgressCommand::Remove(output_id.clone()));

    assert!(
        !backend.output_sockets.contains_key(&output_id),
        "a leaf with nothing queued must close immediately, not wait for a drain sweep"
    );
}

#[test]
fn draining_leaf_force_closes_once_the_drain_deadline_passes() {
    // A peer that stops reading mid-drain must not be able to hang a
    // removal or shutdown forever — `sweep_draining_leaves` force-closes
    // once `draining_since` is older than `drain_timeout`, regardless of
    // remaining pending bytes.
    let (backend, output_id, native_backlog) = backend_with_one_leaf();
    let mut backend = backend.with_drain_timeout(Duration::from_millis(50));

    backend.on_command(EgressCommand::Remove(output_id.clone()));
    assert!(backend.output_sockets.contains_key(&output_id));

    // Simulate the deadline already having passed, the same way
    // `leaf_termination.rs`'s stall tests simulate "no progress for a long
    // time" — real elapsed-time waiting would make this test slow and
    // flaky for no benefit.
    let long_ago = Instant::now() - Duration::from_secs(3600);
    {
        let key = *backend.output_sockets.get(&output_id).unwrap();
        let leaf = backend.leaves[key.0].as_mut().unwrap();
        leaf.draining_since = Some(long_ago);
    }
    // Still nonzero — proves the close is deadline-driven, not
    // flush-driven.
    assert!(native_backlog.lock().unwrap().is_some());

    backend.sweep_draining_leaves(Instant::now());

    assert!(
        !backend.output_sockets.contains_key(&output_id),
        "a leaf stuck draining past its deadline must be force-closed"
    );
}

#[test]
fn shutdown_marks_every_connected_leaf_draining() {
    // `Shutdown` (unlike the old immediate-stop behavior) must give every
    // leaf a chance to flush, not just the one being explicitly removed.
    let (mut backend, output_id, _native_backlog) = backend_with_one_leaf();

    backend.on_command(EgressCommand::Shutdown);

    assert!(
        backend.output_sockets.contains_key(&output_id),
        "Shutdown must not close a leaf with pending bytes immediately"
    );
    let key = *backend.output_sockets.get(&output_id).unwrap();
    let leaf = backend.leaves[key.0].as_ref().unwrap();
    assert!(leaf.draining_since.is_some());
    assert_eq!(leaf.draining_reason, Some(CloseReason::ShardShutdown));
}

/// `feed_lag_units` is a measurement of how far behind a *reading* leaf is.
/// A leaf that has not been visited yet still holds the placeholder cursor,
/// so subtracting it from the head would publish the entire feed length as
/// lag — a leaf that is connecting, on a pipeline that has been running for
/// an hour, would look catastrophically behind while doing nothing wrong.
#[test]
fn stall_sweep_reports_no_feed_lag_for_a_leaf_that_has_not_been_visited_yet() {
    use super::support::wrapped_feed;

    let mut backend = SrtShardBackend::new(
        wrapped_feed(40, 5),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
    );
    let feed_lag_units = Arc::new(std::sync::atomic::AtomicU64::new(u64::MAX));
    let leaf = SrtFabricLeaf::new(
        common(7).with_progress_sink(crate::media::egress::leaf::EgressProgressSink {
            feed_lag_units: Some(feed_lag_units.clone()),
            ..Default::default()
        }),
        Box::new(ControllableSender {
            native_backlog: Arc::new(Mutex::new(None)),
        }) as Box<dyn SrtMessageSender + Send>,
    );
    backend.add_leaf(leaf);

    backend.sweep_stalled_leaves(Instant::now());

    assert_eq!(
        feed_lag_units.load(std::sync::atomic::Ordering::Relaxed),
        0,
        "an unvisited leaf has not started reading, so it is not behind"
    );
}
