//! The lease's drop path and its attempt check, through the engine's own
//! registration and release task.

use std::time::Duration;

use super::*;

const PIPELINE: &str = "pipe-lease";

async fn register(engine: &Arc<MediaEngine>, input: &str) -> Option<IngestLease> {
    engine
        .try_register_pipeline_input_attempt(PIPELINE, input, input, "srt", true)
        .await
}

/// Waits until every release queued so far has been applied. The queue is
/// drained in order, so a probe attempt dropped now is released last.
async fn settle_releases(engine: &Arc<MediaEngine>) {
    let probe = engine
        .try_register_pipeline_input_attempt("pipe-probe", "probe", "probe", "srt", true)
        .await
        .expect("the probe registers");
    drop(probe);
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.input_has_session("pipe-probe", "probe").await {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the release task drains its queue");
}

#[tokio::test]
async fn a_dropped_lease_releases_its_attempt_as_abandoned() {
    let engine = Arc::new(MediaEngine::new());
    let lease = register(&engine, "input")
        .await
        .expect("the attempt registers");
    assert!(
        register(&engine, "input").await.is_none(),
        "a live lease holds its input"
    );

    drop(lease);
    let again = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if let Some(lease) = register(&engine, "input").await {
                break lease;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await;
    assert!(again.is_ok(), "a dropped lease left its input registered");
    let outcome = engine
        .recent_ingest_outcome(PIPELINE)
        .await
        .expect("the abandoned end is recorded");
    assert_eq!(outcome.failure_phase.as_deref(), Some(ABANDONED_PHASE));
    assert!(outcome.had_error);
}

/// A release task dies with its runtime (a secondary runtime faulting while
/// the engine lives on). A lease dropped meanwhile stays queued, and the next
/// release task started, on another runtime, ends it first.
#[test]
fn a_lease_dropped_while_no_release_task_runs_is_released_by_the_next_one() {
    let runtime = || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime")
    };
    let engine = Arc::new(MediaEngine::new());
    let first = runtime();
    let lease = first
        .block_on(register(&engine, "input"))
        .expect("the attempt registers");
    drop(first);
    drop(lease);

    runtime().block_on(async {
        engine.start_ingest_release_drain();
        let released = tokio::time::timeout(Duration::from_secs(5), async {
            while register(&engine, "input").await.is_none() {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await;
        assert!(
            released.is_ok(),
            "a lease dropped between release tasks leaked its input"
        );
    });
}

/// The operator cancels an input and its replacement registers before the
/// old owner lets go: neither the old lease's drop nor its finish may end
/// the replacement.
#[tokio::test]
async fn a_stale_lease_never_releases_a_newer_attempt_on_its_input() {
    let engine = Arc::new(MediaEngine::new());
    let stale = register(&engine, "input")
        .await
        .expect("the attempt registers");
    assert!(engine.cancel_pipeline_input("input").await);
    let current = register(&engine, "input")
        .await
        .expect("a cancelled attempt does not block its replacement");

    drop(stale);
    settle_releases(&engine).await;
    assert_current(&engine, &current).await;

    assert!(engine.cancel_pipeline_input("input").await);
    let newest = register(&engine, "input")
        .await
        .expect("the next replacement registers");
    current
        .finish(IngestEnd::Disconnected {
            phase: "disconnect",
            reason: "the old owner finished late".to_string(),
            had_error: false,
        })
        .await;
    assert_current(&engine, &newest).await;
}

async fn assert_current(engine: &Arc<MediaEngine>, lease: &IngestLease) {
    let registration = lease.registration();
    assert!(
        engine
            .with_ingest_session(registration, |_| ())
            .await
            .is_some(),
        "the newer attempt keeps its session"
    );
    assert!(
        engine
            .is_ingest_session_selected(PIPELINE, registration)
            .await,
        "the newer attempt stays selected"
    );
    assert!(!registration.cancel_token.is_cancelled());
}
