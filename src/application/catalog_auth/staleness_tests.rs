//! How stale the index can get: a committed write lands even when its caller
//! is cancelled, and the periodic reload enforces a change no write path
//! reported.

use super::write_visibility_tests::Catalog;
use super::*;
/// Polls admission until `key` is refused or `within` passes.
async fn eventually_refused(c: &Catalog, key: &str, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if c.admitted(PipelineAccessMode::RtmpPublish, key)
            .await
            .is_none()
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A key rotation whose caller is cancelled after SQLite committed it (a
/// dropped HTTP request) is still enforced: the write and its refresh run as
/// one task the caller cannot cancel.
#[tokio::test]
async fn a_rotation_whose_caller_is_cancelled_after_commit_is_still_enforced() {
    let c = Catalog::open("cancelled-rotation").await;
    c.pipelines
        .create_pipeline("pipe", "Pipe", "key-a", None, None)
        .await
        .unwrap();
    // The independent reload pool cannot serve a read until released, but
    // the API pool can both commit the update and prove it is visible.
    let mut held = Vec::new();
    for _ in 0..c.lifecycle.options().get_max_connections() {
        held.push(c.lifecycle.acquire().await.unwrap());
    }
    let service = c.pipelines.clone();

    let caller = tokio::spawn(async move {
        service
            .update_pipeline("pipe", "Pipe", "key-b", None, None)
            .await
    });
    tokio::time::timeout(Duration::from_secs(1), async {
        while c
            .pipelines
            .get_by_stream_key("key-b")
            .await
            .unwrap()
            .is_none()
        {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the key rotation commits before admission reloads");
    assert!(!caller.is_finished());
    assert!(
        c.admitted(PipelineAccessMode::RtmpPublish, "key-a")
            .await
            .is_some()
    );
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    drop(held);

    assert!(
        eventually_refused(&c, "key-a", Duration::from_secs(2)).await,
        "the committed rotation was never enforced: the old key still connects"
    );
    assert!(
        c.admitted(PipelineAccessMode::RtmpPublish, "key-b")
            .await
            .is_some()
    );
}

/// A row changed in SQLite behind the services' back (no refresh asked) is
/// enforced by the periodic reload within its interval, and the reload stops
/// when told to.
#[tokio::test]
async fn the_periodic_reload_enforces_a_change_no_write_reported() {
    const INTERVAL: Duration = Duration::from_millis(50);
    let c = Catalog::open("periodic").await;
    c.pipelines
        .create_pipeline("pipe", "Pipe", "key-a", None, None)
        .await
        .unwrap();
    let reloads = tokio::spawn(c.catalog.clone().reload_periodically(INTERVAL));

    sqlx::query("UPDATE pipeline_inputs SET enabled = 0 WHERE pipeline_id = 'pipe'")
        .execute(&c.pool)
        .await
        .unwrap();

    assert!(
        eventually_refused(&c, "key-a", INTERVAL * 4).await,
        "a disabled key still connects after four reload intervals"
    );
    c.catalog.shutdown();
    tokio::time::timeout(Duration::from_secs(1), reloads)
        .await
        .expect("the periodic reload stops when told to")
        .unwrap();
}
