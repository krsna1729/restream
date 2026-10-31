//! Session-start tests: the window between an attempt's registration and the
//! session owner that takes it over.

use super::super::*;
use crate::hls_ingest::test_support::{PIPELINE_ID, STREAM_KEY, test_ingest};

/// A dispatch dropped after it registered the attempt, but before the owner
/// that would release it is spawned, must release the attempt itself. Without
/// the guard the engine keeps an ingest nothing will ever end — a disabled
/// input still shown as connected — and the ring is recreated under it, which
/// is the state a cancelled pull used to leave behind.
#[tokio::test]
async fn a_dispatch_dropped_before_its_owner_spawns_releases_the_attempt() {
    let (engine, ingest) = test_ingest(HlsIngestLimits::default());
    let pipeline = ingest
        .authorize(STREAM_KEY, "127.0.0.1")
        .await
        .expect("the key resolves while the row stands");

    // Hold the pipeline registry: `session_for` registers the attempt first and
    // then asks for the ring, so the dispatch parks exactly in the window this
    // test is about.
    let held = engine.ingests.pipelines.write().await;

    let mut body = ingest
        .object_body(ObjectKind::Playlist, None, &pipeline.input_id)
        .await
        .unwrap();
    body.push(b"#EXTM3U\n#EXT-X-VERSION:3\n").await.unwrap();
    let dispatching = tokio::spawn({
        let ingest = Arc::clone(&ingest);
        let pipeline = pipeline.clone();
        async move {
            ingest
                .dispatch(
                    &pipeline,
                    STREAM_KEY,
                    "127.0.0.1",
                    DispatchOrigin::Push,
                    ObjectKind::Playlist,
                    "index.m3u8".to_string(),
                    body,
                )
                .await
        }
    });

    // The registration is in the engine before the lock is released.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while !engine
            .input_has_session(PIPELINE_ID, &pipeline.input_id)
            .await
        {
            tokio::time::sleep(std::time::Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("the attempt registers before the ring is asked for");

    dispatching.abort();
    drop(held);

    // The release runs on its own task (a drop cannot await), so allow it to
    // land before asserting.
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        while engine
            .input_has_session(PIPELINE_ID, &pipeline.input_id)
            .await
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("a dropped dispatch leaves no registered attempt behind");
}
