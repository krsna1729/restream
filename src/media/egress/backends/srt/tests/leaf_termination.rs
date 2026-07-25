//! Regression coverage for `EgressProgressSink::terminated_unexpectedly`
//! (`docs/egress-implementation.md` Phase 4 status): a fabric leaf closed
//! for any reason the application did not request (stall recovery, peer
//! close, connect failure) used to vanish silently — nothing told the
//! application task, so the shared retry/backoff bookkeeping in
//! `src/infrastructure/bootstrap/egress.rs` never ran and the output just
//! sat stale forever instead of retrying. These tests prove the shard sets
//! the flag on the paths that actually reach it, and leaves it unset for a
//! leaf that stays healthy.

use super::super::*;
use super::support::{FakeReadinessPoller, FakeSocketConfigurator, common, feed};
use bytes::Bytes;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use crate::media::egress::backend::CloseReason;
use crate::media::egress::leaf::EgressProgressSink;
use crate::media::egress::policy::WorkBudget;

/// A sender that reports a fixed, never-draining native backlog — the same
/// shape `stall_sweep_closes_leaf_with_stuck_native_backlog` (`shard.rs`)
/// uses to drive a stall deterministically without needing a real visit to
/// populate the engine's own pending-message state (`observe_stall` reads
/// `app_pending_bytes` from `SrtEgressEngine::pending_message_bytes()`, not
/// from `LeafCommon.pending_application_bytes` — only a native backlog is
/// observable without actually visiting the leaf).
struct NeverDrainsSender;

impl SrtMessageSender for NeverDrainsSender {
    fn send_message(&mut self, _message: &Bytes) -> crate::media::srt::SrtSendResult {
        crate::media::srt::SrtSendResult::WouldBlock
    }

    fn close(&mut self, _reason: CloseReason) {}

    fn native_send_backlog(&mut self) -> Option<crate::media::srt::NativeSendBacklog> {
        Some(crate::media::srt::NativeSendBacklog {
            bytes: 4_096,
            packets: 3,
            ms: 500,
        })
    }
}

/// A sender with no native backlog at all — `observe_stall` sees zero
/// pending bytes and reports `Idle`, never `Stalled`.
struct NoBacklogSender;

impl SrtMessageSender for NoBacklogSender {
    fn send_message(&mut self, _message: &Bytes) -> crate::media::srt::SrtSendResult {
        crate::media::srt::SrtSendResult::WouldBlock
    }

    fn close(&mut self, _reason: CloseReason) {}
}

fn leaf_common_with_sink(generation: u64) -> (LeafCommon, Arc<AtomicBool>) {
    let terminated = Arc::new(AtomicBool::new(false));
    let leaf_common = common(generation).with_progress_sink(EgressProgressSink {
        terminated_unexpectedly: Some(terminated.clone()),
        ..Default::default()
    });
    (leaf_common, terminated)
}

#[test]
fn stall_sweep_marks_terminated_unexpectedly_on_the_closed_leaf() {
    let poller = FakeReadinessPoller::default();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
    );

    let (leaf_common, terminated) = leaf_common_with_sink(7);
    let deadline = leaf_common.limits.max_backpressure_duration;
    let leaf = SrtFabricLeaf::new(
        leaf_common,
        Box::new(NeverDrainsSender) as Box<dyn SrtMessageSender + Send>,
    );
    backend.add_leaf(42, leaf).unwrap();

    assert!(!terminated.load(Ordering::Relaxed));

    let start = Instant::now();
    backend.sweep_stalled_leaves(start + deadline + Duration::from_secs(2));

    assert!(
        terminated.load(Ordering::Relaxed),
        "stall-sweep close must mark the leaf's sink terminated so the \
         application task can return and let retry/backoff bookkeeping run"
    );
}

#[test]
fn stall_sweep_leaves_a_healthy_leaf_unmarked() {
    let poller = FakeReadinessPoller::default();
    let mut backend = SrtShardBackend::with_socket_configurator(
        poller,
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConfigurator::default(),
    );

    let (leaf_common, terminated) = leaf_common_with_sink(7);
    let deadline = leaf_common.limits.max_backpressure_duration;
    let leaf = SrtFabricLeaf::new(
        leaf_common,
        Box::new(NoBacklogSender) as Box<dyn SrtMessageSender + Send>,
    );
    backend.add_leaf(42, leaf).unwrap();

    backend.sweep_stalled_leaves(Instant::now() + deadline + Duration::from_secs(2));

    assert!(!terminated.load(Ordering::Relaxed));
}
