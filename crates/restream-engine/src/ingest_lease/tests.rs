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
    wait_until_released(&engine, "input").await;
    let outcome = engine
        .recent_ingest_outcome(PIPELINE)
        .await
        .expect("the abandoned end is recorded");
    assert_eq!(outcome.failure_phase.as_deref(), Some(ABANDONED_PHASE));
    assert!(outcome.had_error);
    assert!(register(&engine, "input").await.is_some());
}

async fn wait_until_released(engine: &Arc<MediaEngine>, input: &str) {
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.input_has_session(PIPELINE, input).await {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("a dropped lease's attempt is released");
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("tokio runtime")
}

/// A release task dies with its runtime (a secondary runtime faulting while
/// the engine lives on). A lease dropped meanwhile stays queued, and the next
/// release task started, on another runtime, ends it first.
#[test]
fn a_lease_dropped_while_no_release_task_runs_is_released_by_the_next_one() {
    let engine = Arc::new(MediaEngine::new());
    let first = runtime();
    let lease = first
        .block_on(register(&engine, "input"))
        .expect("the attempt registers");
    drop(first);
    drop(lease);

    runtime().block_on(async {
        engine.start_ingest_release_drain();
        wait_until_released(&engine, "input").await;
    });
}

/// A dropped lease cancels its attempt at once: the next publisher on the
/// input registers before any release task runs, and the dropped attempt's
/// later release, its token being its own, leaves the newer attempt alone.
#[test]
fn a_dropped_lease_frees_its_input_before_its_release_runs() {
    let engine = Arc::new(MediaEngine::new());
    let first = runtime();
    let lease = first
        .block_on(register(&engine, "input"))
        .expect("the attempt registers");
    drop(first);
    drop(lease);

    runtime().block_on(async {
        let current = register(&engine, "input")
            .await
            .expect("the dropped attempt's input admits the next publisher");
        settle_releases(&engine).await;
        assert_current(&engine, &current).await;
    });
}

/// A finish cancelled after recording its outcome, before its release:
/// the drop releases the attempt without recording a second outcome.
#[tokio::test]
async fn a_finish_cancelled_between_record_and_release_records_one_outcome() {
    let engine = Arc::new(MediaEngine::new());
    let lease = register(&engine, "input")
        .await
        .expect("the attempt registers");
    // Holding the token map parks the release at its first write lock.
    let tokens = engine.ingests.cancel_tokens.read().await;
    let finish = tokio::spawn(lease.finish(IngestEnd::Disconnected {
        phase: "disconnect",
        reason: "the publisher left".to_string(),
        had_error: false,
    }));
    tokio::time::timeout(Duration::from_secs(5), async {
        while engine.recent_ingest_outcome(PIPELINE).await.is_none() {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("the finish records its outcome");
    finish.abort();
    let _ = finish.await;
    drop(tokens);

    wait_until_released(&engine, "input").await;
    let outcome = engine
        .recent_ingest_outcome(PIPELINE)
        .await
        .expect("the finish's outcome stays");
    assert_eq!(outcome.failure_phase.as_deref(), Some("disconnect"));
    assert_eq!(outcome.disconnect_count, 1, "one end, one outcome");
    assert!(register(&engine, "input").await.is_some());
}

/// A release cancelled while it waits for a registry lock leaves the attempt
/// whole, so the owner's later finish still ends it and frees the input.
#[tokio::test]
async fn a_release_cancelled_at_a_lock_leaves_its_attempt_whole() {
    let engine = Arc::new(MediaEngine::new());
    let lease = register(&engine, "input")
        .await
        .expect("the attempt registers");
    let tokens = engine.ingests.cancel_tokens.read().await;
    let release = tokio::spawn({
        let engine = engine.clone();
        let registration = lease.registration().clone();
        async move {
            engine
                .unregister_ingest_if_current(PIPELINE, &registration)
                .await
        }
    });
    tokio::time::sleep(Duration::from_millis(20)).await;
    release.abort();
    let _ = release.await;
    drop(tokens);

    lease.finish(IngestEnd::Released).await;
    assert!(
        register(&engine, "input").await.is_some(),
        "a cancelled release left the input refused as a duplicate"
    );
}

/// Two release passes that both took the same front entry each complete it:
/// the second completion must not remove the next, unreleased entry.
#[tokio::test]
async fn two_passes_over_one_queue_release_every_entry() {
    let engine = Arc::new(MediaEngine::new());
    let first = register(&engine, "first").await.expect("first registers");
    let second = register(&engine, "second").await.expect("second registers");
    let queue = ReleaseQueue::default();
    for lease in [&first, &second] {
        queue.push(AbandonedAttempt {
            pipeline_id: PIPELINE.to_string(),
            registration: lease.registration().clone(),
            record: false,
        });
    }
    let (pass_one, pass_two) = (queue.front(), queue.front());
    for taken in [pass_one, pass_two] {
        let taken = taken.expect("both passes see the first entry");
        engine
            .unregister_ingest_if_current(PIPELINE, &taken.registration)
            .await;
        queue.complete(&taken);
    }
    release_backlog(&engine, &queue).await;

    assert!(queue.front().is_none());
    assert!(!engine.input_has_session(PIPELINE, "first").await);
    assert!(
        !engine.input_has_session(PIPELINE, "second").await,
        "the second entry was dropped unreleased"
    );
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
