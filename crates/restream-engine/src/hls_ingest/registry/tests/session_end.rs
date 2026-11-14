//! How a session ends: whatever ends one (a panic, a cancelled supervisor)
//! ends only that session and releases its input; a panicking playlist parse
//! refuses only that playlist.

use super::super::*;
use crate::hls_ingest::test_support::test_ingest;

/// A panic inside one session is contained by the MEDIA executor: counted,
/// the supervisor records the failure, releases that session's input, and
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

    let before = crate::panic_boundary::contained_panics();
    ingest
        .clone()
        .supervise_session(panicking, async {
            crate::test_support::with_expected_panic_suppressed(|| {
                panic!("a session bug");
            })
        })
        .await;

    assert!(
        crate::panic_boundary::contained_panics() > before,
        "the session panic is counted as contained"
    );
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

/// A supervisor cancelled mid-session (an abort, its runtime stopping) must
/// release its input, not leave it shown as connected until another attempt
/// overwrites it.
#[tokio::test]
async fn a_cancelled_supervisor_releases_its_input() {
    let (engine, ingest) = test_ingest(HlsIngestLimits::default());
    let attempt = engine
        .try_register_pipeline_input_attempt("pipe-a", "input-a", "key-a", "hls", true)
        .await
        .expect("the first attempt registers");
    let supervisor = tokio::spawn(
        ingest
            .clone()
            .supervise_session(attempt, std::future::pending::<(SessionEnd, Counters)>()),
    );
    tokio::task::yield_now().await;
    supervisor.abort();
    assert!(supervisor.await.unwrap_err().is_cancelled());

    let released = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while engine.input_has_session("pipe-a", "input-a").await {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await;
    assert!(
        released.is_ok(),
        "a cancelled supervisor left its input registered"
    );
    let _next = engine
        .try_register_pipeline_input_attempt("pipe-a", "input-a", "key-a", "hls", true)
        .await
        .expect("the next attempt on the input registers");
}

/// A panic in the playlist parser, on the blocking pool, refuses only that
/// playlist: counted as contained, answered as unavailable, and the next
/// playlist parses.
#[tokio::test]
async fn a_panicking_playlist_parse_refuses_only_that_playlist() {
    let before = crate::panic_boundary::contained_panics();
    let refused = parse_playlist_off_worker(
        bytes::Bytes::from_static(b"#EXTM3U\n"),
        INJECTED_PANIC_PLAYLIST_PATH.to_string(),
    )
    .await;
    assert!(matches!(refused, Err(DispatchError::Unavailable)));
    assert!(
        crate::panic_boundary::contained_panics() > before,
        "the parser panic is counted as contained"
    );
    let next = parse_playlist_off_worker(
        bytes::Bytes::from_static(b"#EXTM3U\n#EXT-X-TARGETDURATION:2\n#EXTINF:2.0,\na.ts\n"),
        "live/index.m3u8".to_string(),
    )
    .await;
    assert!(
        matches!(next, Ok(Ok(_))),
        "the next playlist parses: {:?}",
        next.map(|parsed| parsed.is_ok())
    );
}
