//! Regression test for `on_media_tick`'s connect-completion scheduling —
//! the SRT counterpart of `rtmp_shard_media_tick_tests.rs`. See the doc
//! comment on `EgressShardBackend::on_media_tick` (`src/media/egress/shard.rs`)
//! for the full story: nothing previously told the shard runtime to give a
//! freshly-connected leaf its first readiness check, so it sat registered
//! but unvisited until an unrelated `FeedWake` happened to arrive.

use super::super::*;
use super::support::{FakeSocketConnector, feed};
use crate::media::egress::command::{EgressCommand, FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::policy::{LeafPolicy, WorkBudget};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use bytes::Bytes;
use std::time::Duration;

fn output_spec(id: &str, generation: u64) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation,
        feed: FeedId::new("feed-srt"),
        protocol: ProtocolSpec::Srt {
            url: "srt://127.0.0.1:9000?streamid=publish%3Akey".to_string(),
        },
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

#[test]
fn on_media_tick_schedules_ready_work_when_a_connect_completes() {
    let (sender, queue) = srt_resolve_completion_queue(4);
    let mut backend = SrtShardBackend::with_runtime_components(
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConnector::returning(),
        queue,
    );
    let output_id = OutputId::new("media-tick-leaf");
    backend.on_command(EgressCommand::Add(output_spec("media-tick-leaf", 1)));
    sender
        .send(SrtResolvedConnect {
            output_id: output_id.clone(),
            generation: 1,
            peer_addrs: vec!["127.0.0.1:9000".parse().unwrap()],
        })
        .unwrap();

    let effect = backend.on_media_tick();

    assert_eq!(
        effect,
        EgressShardCommandEffect::ScheduleReady { count: 1 },
        "a completed connect must schedule ready work so the new leaf gets \
         its first readiness check without waiting on an unrelated FeedWake"
    );
    assert!(
        backend.output_sockets.contains_key(&output_id),
        "the connect must have actually produced a registered leaf"
    );
}

#[test]
fn on_media_tick_is_a_no_op_when_nothing_resolved() {
    let (_sender, queue) = srt_resolve_completion_queue(4);
    let mut backend = SrtShardBackend::with_runtime_components(
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConnector::returning(),
        queue,
    );

    let effect = backend.on_media_tick();

    assert_eq!(
        effect,
        EgressShardCommandEffect::Continue,
        "an idle tick with no resolved connects must not schedule ready work"
    );
}

/// Connect admission gates concurrent in-flight *handshakes*, not just
/// concurrent `connect()` calls -- the fix for a real live-harness failure
/// where draining every resolved connect unconditionally concentrated a
/// mass output-creation burst onto too few libsrt multiplexers (see the
/// module doc on `srt_connect_admission.rs`). SRT's `connect()` is
/// non-blocking-initiate (confirmed against
/// `connect_single_srt_egress_socket`), so a permit released right after
/// that call would throttle nothing; it must be held on the leaf itself
/// until its first visit resolves the async handshake. This proves the
/// full lifecycle: exhausted -> backlogged; freed -> connects and *keeps
/// holding* the permit; visited -> releases it, letting the next
/// backlogged completion through.
#[test]
fn on_media_tick_backlogs_resolved_connects_once_admission_is_exhausted() {
    let (sender, queue) = srt_resolve_completion_queue(4);
    let admission = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let mut backend = SrtShardBackend::with_runtime_components(
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConnector::returning(),
        queue,
    )
    .with_connect_admission(Some(admission.clone()));

    for id in ["leaf-a", "leaf-b"] {
        backend.on_command(EgressCommand::Add(output_spec(id, 1)));
        sender
            .send(SrtResolvedConnect {
                output_id: OutputId::new(id),
                generation: 1,
                peer_addrs: vec!["127.0.0.1:9000".parse().unwrap()],
            })
            .unwrap();
    }
    // Hold the only permit (standing in for another shard's own in-flight
    // handshake) so the tick below finds the semaphore exhausted -- proving
    // admission actually gates the connect, not just that a semaphore
    // field exists.
    let held = admission.clone().try_acquire_owned().unwrap();

    let effect = backend.on_media_tick();

    assert_eq!(
        effect,
        EgressShardCommandEffect::ScheduleReady { count: 1 },
        "a nonempty backlog must keep the shard loop from idle-waiting"
    );
    assert!(
        !backend
            .output_sockets
            .contains_key(&OutputId::new("leaf-a")),
        "no permit was available -- neither leaf should have connected yet"
    );
    assert!(
        !backend
            .output_sockets
            .contains_key(&OutputId::new("leaf-b"))
    );
    assert_eq!(
        backend.connect_backlog_len(),
        2,
        "both resolved completions must be retained for the next tick, not dropped"
    );

    // Releasing the externally-held permit lets exactly one backlogged
    // completion through -- and that leaf must keep holding the permit
    // (not release it back for leaf-b) until it is actually visited.
    drop(held);
    let effect = backend.on_media_tick();

    assert_eq!(effect, EgressShardCommandEffect::ScheduleReady { count: 1 });
    let leaf_a = OutputId::new("leaf-a");
    let leaf_b = OutputId::new("leaf-b");
    assert!(backend.output_sockets.contains_key(&leaf_a));
    assert!(
        backend.leaf_holds_handshake_permit(&leaf_a),
        "leaf-a's handshake is unresolved -- it must still hold the permit"
    );
    assert!(
        !backend.output_sockets.contains_key(&leaf_b),
        "no permit was available for leaf-b while leaf-a's handshake is pending"
    );
    assert_eq!(backend.connect_backlog_len(), 1);

    // Once leaf-a's handshake is (simulated as) resolved, its permit frees
    // up for leaf-b.
    backend.simulate_first_visit_for_test(&leaf_a);
    let effect = backend.on_media_tick();

    assert_eq!(effect, EgressShardCommandEffect::ScheduleReady { count: 1 });
    assert!(backend.output_sockets.contains_key(&leaf_b));
    assert!(
        backend.leaf_holds_handshake_permit(&leaf_b),
        "leaf-b's own handshake is now unresolved -- it must hold the permit in turn"
    );
    assert_eq!(backend.connect_backlog_len(), 0);
}

#[test]
fn on_media_tick_connects_every_resolved_completion_without_admission_configured() {
    // `connect_admission: None` (every existing constructor) must behave
    // exactly as before this change: fully unthrottled.
    let (sender, queue) = srt_resolve_completion_queue(4);
    let mut backend = SrtShardBackend::with_runtime_components(
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConnector::returning(),
        queue,
    );

    for id in ["leaf-a", "leaf-b", "leaf-c"] {
        backend.on_command(EgressCommand::Add(output_spec(id, 1)));
        sender
            .send(SrtResolvedConnect {
                output_id: OutputId::new(id),
                generation: 1,
                peer_addrs: vec!["127.0.0.1:9000".parse().unwrap()],
            })
            .unwrap();
    }

    backend.on_media_tick();

    for id in ["leaf-a", "leaf-b", "leaf-c"] {
        assert!(backend.output_sockets.contains_key(&OutputId::new(id)));
    }
    assert_eq!(backend.connect_backlog_len(), 0);
}

/// A leaf removed before it is ever visited must still release its
/// connect-admission permit -- proving the RAII design (the permit lives on
/// the leaf, not a side-map some removal path could forget to clear).
#[test]
fn removing_an_unvisited_leaf_releases_its_handshake_permit() {
    let (sender, queue) = srt_resolve_completion_queue(4);
    let admission = std::sync::Arc::new(tokio::sync::Semaphore::new(1));
    let mut backend = SrtShardBackend::with_runtime_components(
        feed([Bytes::from_static(b"abc")]),
        WorkBudget::new(8, 1024, Duration::from_millis(1)),
        FakeSocketConnector::returning(),
        queue,
    )
    .with_connect_admission(Some(admission.clone()));

    let leaf_a = OutputId::new("leaf-a");
    backend.on_command(EgressCommand::Add(output_spec("leaf-a", 1)));
    sender
        .send(SrtResolvedConnect {
            output_id: leaf_a.clone(),
            generation: 1,
            peer_addrs: vec!["127.0.0.1:9000".parse().unwrap()],
        })
        .unwrap();
    backend.on_media_tick();

    assert!(backend.output_sockets.contains_key(&leaf_a));
    assert!(backend.leaf_holds_handshake_permit(&leaf_a));
    assert_eq!(admission.available_permits(), 0);

    backend.on_command(EgressCommand::Remove(leaf_a.clone()));
    // No queued send-path bytes on a leaf that never got its first visit,
    // so `begin_graceful_close` removes it immediately rather than
    // draining -- see its doc comment.
    assert!(!backend.output_sockets.contains_key(&leaf_a));
    assert_eq!(
        admission.available_permits(),
        1,
        "the removed leaf's permit must be released, not leaked"
    );
}
