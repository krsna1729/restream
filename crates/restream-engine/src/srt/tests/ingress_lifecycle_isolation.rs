//! Ingest selection is per pipeline: one pipeline's stuck selection delays
//! nothing on another pipeline. Production `SrtServer` over a real Owner and
//! real srt-rs callers.
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
