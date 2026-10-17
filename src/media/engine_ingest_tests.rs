//! The ingest registration's cancel safety: a caller whose future is dropped
//! inside it must leave no half-written state behind.

use std::future::Future;
use std::task::{Context, Waker};

use super::*;

/// A registration waits on a `RwLock` write, and the input-status JSON holds
/// that lock as a reader, so a caller can be dropped at that await — a pull's
/// cancel lands exactly there. It must leave neither a token nothing will
/// cancel (the input would read as connected until the next registration) nor
/// a session with no owner.
#[tokio::test]
async fn a_registration_dropped_at_a_write_guard_leaves_nothing_behind() {
    let engine = MediaEngine::new();
    let held = engine.ingests.sessions.read().await;
    let mut attempt =
        Box::pin(engine.try_register_pipeline_input_attempt("pipe", "input", "key", "hls", true));
    assert!(
        attempt
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the registration must wait for the held reader"
    );
    drop(attempt);
    drop(held);

    assert!(
        !engine.input_has_session("pipe", "input").await,
        "a dropped registration left a session behind"
    );
    assert!(
        engine
            .try_register_pipeline_input_attempt("pipe", "input", "key", "hls", true)
            .await
            .is_some(),
        "a dropped registration left a token that blocks the input"
    );
}

/// A registration that replaces the active input demotes it before it takes its
/// write guards. A caller dropped after the demote must leave that input
/// forwarding as it was, not registered and silent.
#[tokio::test]
async fn a_registration_dropped_after_the_demote_restores_the_previous_input() {
    let engine = MediaEngine::new();
    engine
        .try_register_pipeline_input_attempt("pipe", "first", "key-1", "hls", true)
        .await
        .expect("the first input registers");
    let previous = engine
        .ingests
        .active
        .read()
        .await
        .get("pipe")
        .cloned()
        .expect("the first input is active");
    let before = previous.gate.state();
    engine
        .ingests
        .selected_inputs
        .write()
        .await
        .insert("pipe".to_string(), "second".to_string());

    // Park the replacement on `timelines`, which it takes after the demote.
    let held = engine.ingests.timelines.read().await;
    let mut attempt = Box::pin(
        engine.try_register_pipeline_input_attempt("pipe", "second", "key-2", "hls", true),
    );
    assert!(
        attempt
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()))
            .is_pending(),
        "the replacement must wait for the held reader"
    );
    assert_ne!(
        previous.gate.state(),
        before,
        "the replacement demoted the previous input before parking"
    );
    drop(attempt);
    drop(held);

    assert_eq!(
        previous.gate.state(),
        before,
        "a dropped replacement left the previous input demoted"
    );
}
