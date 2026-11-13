//! The SRT session loop awaits no engine lock, and ingest selection is per
//! pipeline: one pipeline's stuck selection, or one peer's stalled probe
//! application, delays nothing on another pipeline or peer. Production
//! `SrtServer` over a real Owner and real srt-rs callers.
//!
//! The stuck holder is a real registration: pipeline A's selection names a
//! new input (as a persisted promotion does) whose publisher registers while
//! A's current input has a packet in flight (the test holds its gate), so
//! the registration waits for that gate to go idle inside A's selection.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::engine::{ActiveIngest, MediaEngine};
use crate::ingest_lease::IngestEnd;
use crate::input_gate::{InputForwardState, InputPacketBoundary, InputPacketGate};

use super::super::ingress_test_support::*;

fn publish_id(key: &str) -> String {
    format!("#!::r={key},m=publish")
}

/// Connect, then publish the fixture once `go` is set, then leave.
fn publish_when(attach: Attach, go: Arc<AtomicBool>) -> tokio::task::JoinHandle<CallerReport> {
    tokio::task::spawn_blocking(move || {
        let mut step = publish_step(ts_chunks(400));
        run_caller(attach, Duration::from_secs(30), move |ctx| {
            go.load(Ordering::Acquire) && step(ctx)
        })
    })
}

async fn session(engine: &MediaEngine, input_id: &str) -> Option<Arc<ActiveIngest>> {
    engine.ingests.sessions.read().await.get(input_id).cloned()
}

/// Start a registration of `input_id`, the input `pipeline_id`'s selection
/// names, while the selected input whose `gate` the caller holds busy is
/// still active, and wait until it is stuck inside the pipeline's selection
/// (it demoted the gate and waits for it to go idle).
async fn stuck_registration(
    server: &TestServer,
    gate: &InputPacketGate,
    pipeline_id: &'static str,
    input_id: &'static str,
) -> tokio::task::JoinHandle<Option<crate::ingest_lease::IngestLease>> {
    let engine = server.engine.clone();
    engine
        .ingests
        .selected_inputs
        .write()
        .await
        .insert(pipeline_id.to_string(), input_id.to_string());
    let stuck = tokio::spawn(async move {
        engine
            .try_register_pipeline_input_attempt(pipeline_id, input_id, input_id, "test", false)
            .await
    });
    server
        .wait_until(
            "the replacing registration waits inside the selection",
            Duration::from_secs(5),
            || async { gate.state() == InputForwardState::Standby },
        )
        .await;
    stuck
}

async fn finish(lease: crate::ingest_lease::IngestLease) {
    lease.finish(IngestEnd::Released).await;
}

/// Pipeline A's selection is held by a stuck registration; an SRT
/// publisher on pipeline B still registers, gets its probe answered (its
/// media reaches the ring only after the answer) and is finished on time.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_stuck_selection_on_one_pipeline_leaves_another_pipelines_publisher_on_time() {
    let server = TestServer::start(&["sel-b"], vec![plain("sel-b")]).await;
    let engine = server.engine.clone();
    engine.get_or_create_pipeline("pipeline-sel-a").await;
    let a1 = engine
        .try_register_pipeline_input_attempt("pipeline-sel-a", "input-a1", "a1", "test", true)
        .await
        .expect("A's first input registers");
    let gate = session(&engine, "input-a1").await.unwrap().gate.clone();
    let in_flight = gate
        .try_enter(InputPacketBoundary::Other)
        .expect("A's selected input forwards");
    let stuck = stuck_registration(&server, &gate, "pipeline-sel-a", "input-a2").await;

    let b = publish_when(
        direct(server.remote, &publish_id("sel-b"), None),
        Arc::new(AtomicBool::new(true)),
    );
    server
        .wait_until(
            "B registers and its probe is answered while A's selection is stuck",
            Duration::from_secs(5),
            || async { server.ring_progress("pipeline-sel-b").await > 0 },
        )
        .await;
    assert!(b.await.unwrap().connected);
    server
        .wait_until(
            "B's disconnect is finished while A's selection is stuck",
            Duration::from_secs(3),
            || async { session(&server.engine, "input-sel-b").await.is_none() },
        )
        .await;
    assert!(!stuck.is_finished(), "A's registration was still stuck");

    drop(in_flight);
    let a2 = tokio::time::timeout(Duration::from_secs(5), stuck)
        .await
        .expect("A's registration completes once the gate is idle")
        .unwrap()
        .expect("A's replacing input registers");
    finish(a2).await;
    finish(a1).await;
    server.stop().await;
}

/// A stalled engine lock for peer A's probe application delays neither
/// peer B's probe answer nor B's disconnect on the shared loop; once the
/// lock frees, A's queued probe is answered and A is finished.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_stalled_probe_on_one_peer_leaves_another_peers_probe_and_disconnect_on_time() {
    let server = TestServer::start(
        &["probe-a", "probe-b"],
        vec![plain("probe-a"), plain("probe-b")],
    )
    .await;
    let go_a = Arc::new(AtomicBool::new(false));
    let a = publish_when(
        direct(server.remote, &publish_id("probe-a"), None),
        go_a.clone(),
    );
    server
        .wait_until("A registers", Duration::from_secs(5), || async {
            server.ingest_active("pipeline-probe-a").await
        })
        .await;
    let a_ingest = session(&server.engine, "input-probe-a").await.unwrap();
    let gate = a_ingest.gate.clone();
    let in_flight = gate
        .try_enter(InputPacketBoundary::Other)
        .expect("A's selected input forwards");
    let stuck = stuck_registration(&server, &gate, "pipeline-probe-a", "input-probe-a2").await;

    // A publishes and leaves: the Owner emits A's Probe, then A's
    // Disconnected; applying the probe waits for A's stuck selection.
    go_a.store(true, Ordering::Release);
    assert!(a.await.unwrap().connected);

    // B connects after both of A's events: one Owner's events reach Tokio
    // in order, so B is served only if the loop is not waiting on A.
    let b = publish_when(
        direct(server.remote, &publish_id("probe-b"), None),
        Arc::new(AtomicBool::new(true)),
    );
    server
        .wait_until(
            "B's probe is answered while A's probe application is stalled",
            Duration::from_secs(5),
            || async { server.ring_progress("pipeline-probe-b").await > 0 },
        )
        .await;
    assert!(b.await.unwrap().connected);
    server
        .wait_until(
            "B's disconnect is finished while A's probe application is stalled",
            Duration::from_secs(3),
            || async { session(&server.engine, "input-probe-b").await.is_none() },
        )
        .await;
    assert!(
        session(&server.engine, "input-probe-a").await.is_some(),
        "A's finish waits behind its stalled probe"
    );

    drop(in_flight);
    let a2 = tokio::time::timeout(Duration::from_secs(5), stuck)
        .await
        .expect("A's replacing registration completes once the gate is idle")
        .unwrap()
        .expect("A's replacing input registers");
    server
        .wait_until(
            "A's queued probe is answered and A is finished",
            Duration::from_secs(5),
            || async { session(&server.engine, "input-probe-a").await.is_none() },
        )
        .await;
    assert!(
        !a_ingest.metadata().tracks_authoritative,
        "A's disconnect cancelled the attempt before its queued probe"
    );
    finish(a2).await;
    server.stop().await;
}

/// With `ingests.sessions` held for writing, an attached publisher's
/// probe is still applied and answered inside the Owner's 2 s probe hold:
/// neither the loop (telemetry folding, deletion checks) nor the probe
/// application reads that map.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
#[expect(
    clippy::await_holding_invalid_type,
    reason = "the test holds the session map for writing on purpose"
)]
async fn a_held_session_map_leaves_a_probe_answered_inside_the_owners_hold() {
    let server = TestServer::start(&["held"], vec![plain("held")]).await;
    let go = Arc::new(AtomicBool::new(false));
    let caller = publish_when(direct(server.remote, &publish_id("held"), None), go.clone());
    server
        .wait_until(
            "the publisher registers",
            Duration::from_secs(5),
            || async { server.ingest_active("pipeline-held").await },
        )
        .await;
    let ingest = session(&server.engine, "input-held").await.unwrap();
    let owner = server.engine.listener_stats_handle();

    let held = server.engine.ingests.sessions.write().await;
    go.store(true, Ordering::Release);
    let answered = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let ack_us = owner.ingress_owner.snapshot().media_probe_ack_max_us;
            if ack_us > 0 {
                return ack_us;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the probe is answered while the session map is held");
    assert!(
        answered < 2_000_000,
        "answered inside the Owner's 2 s probe hold, after {answered} µs"
    );
    assert!(ingest.metadata().tracks_authoritative);
    drop(held);

    assert!(caller.await.unwrap().connected);
    server.stop().await;
}

/// A peer's disconnect frees its input at once, even while its task still
/// waits to apply a queued probe: a fast reconnect on the same key, whose
/// registration queues on the pipeline's selection ahead of the old peer's
/// finish, registers instead of being refused as a duplicate.
#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn a_fast_reconnect_behind_a_stalled_probe_is_not_refused_as_a_duplicate() {
    let server = TestServer::start(&["again"], vec![plain("again")]).await;
    let go = Arc::new(AtomicBool::new(false));
    let a = publish_when(
        direct(server.remote, &publish_id("again"), None),
        go.clone(),
    );
    server
        .wait_until("A registers", Duration::from_secs(5), || async {
            server.ingest_active("pipeline-again").await
        })
        .await;
    let a_ingest = session(&server.engine, "input-again").await.unwrap();
    let gate = a_ingest.gate.clone();
    let in_flight = gate
        .try_enter(InputPacketBoundary::Other)
        .expect("A's selected input forwards");
    let stuck = stuck_registration(&server, &gate, "pipeline-again", "input-again2").await;

    // A publishes and leaves: its probe waits for the stuck selection.
    go.store(true, Ordering::Release);
    assert!(a.await.unwrap().connected);

    // A' reconnects on the key; its registration queues behind A's probe.
    let stop = Arc::new(AtomicBool::new(false));
    let again = {
        let stop = stop.clone();
        let remote = server.remote;
        tokio::task::spawn_blocking(move || {
            run_caller(
                direct(remote, &publish_id("again"), None),
                Duration::from_secs(20),
                move |ctx| ctx.report.disconnected || stop.load(Ordering::Acquire),
            )
        })
    };
    server
        .wait_until(
            "A' queues on the pipeline's selection behind A's probe",
            Duration::from_secs(5),
            || async { server.engine.ingests.selection.users("pipeline-again") >= 3 },
        )
        .await;

    drop(in_flight);
    let a2 = tokio::time::timeout(Duration::from_secs(5), stuck)
        .await
        .expect("the stuck registration completes once the gate is idle")
        .unwrap()
        .expect("the replacing input registers");
    server
        .wait_until(
            "A' registers on the key",
            Duration::from_secs(5),
            || async {
                session(&server.engine, "input-again")
                    .await
                    .is_some_and(|current| current.attempt_id != a_ingest.attempt_id)
            },
        )
        .await;
    stop.store(true, Ordering::Release);
    let again = again.await.unwrap();
    assert!(
        again.connected && !again.disconnected,
        "A' was never refused: {again:?}"
    );
    finish(a2).await;
    server.stop().await;
}
