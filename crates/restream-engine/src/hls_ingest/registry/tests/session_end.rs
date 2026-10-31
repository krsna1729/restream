//! How a session ends: whatever ends one — here a panic — ends only that
//! session and releases its input.

use super::super::*;
use crate::hls_ingest::test_support::test_ingest;

/// A panic inside one session is contained by the MEDIA executor: the
/// supervisor records the failure, releases that session's input, and
/// leaves another pipeline's ingest registered.
#[tokio::test]
async fn a_panic_in_one_session_ends_only_that_session_and_releases_its_input() {
    let (engine, ingest) = test_ingest(HlsIngestLimits::default());
    let panicking = engine
        .try_register_pipeline_input_attempt("pipe-a", "input-a", "key-a", "hls", true)
        .await
        .expect("the first attempt registers");
    let _other = engine
        .try_register_pipeline_input_attempt("pipe-b", "input-b", "key-b", "hls", true)
        .await
        .expect("an attempt on another pipeline registers");

    ingest
        .clone()
        .supervise_session("pipe-a".to_string(), panicking, async {
            crate::test_support::with_expected_panic_suppressed(|| {
                panic!("a session bug");
            })
        })
        .await;

    assert!(
        !engine.input_has_session("pipe-a", "input-a").await,
        "the panicked session's input is released"
    );
    let outcome = engine
        .recent_ingest_outcome("pipe-a")
        .await
        .expect("the end is recorded");
    assert_eq!(outcome.failure_phase.as_deref(), Some("media-executor"));
    assert!(outcome.had_error, "a panic is recorded as an error");
    assert!(
        engine.input_has_session("pipe-b", "input-b").await,
        "another pipeline's ingest is untouched"
    );
}
