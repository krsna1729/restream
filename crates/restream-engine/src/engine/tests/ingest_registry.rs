use super::*;
use crate::ingest_lease::IngestEnd;

#[tokio::test]
async fn rejects_a_second_independent_publisher_for_the_same_pipeline() {
    let engine = Arc::new(MediaEngine::new());

    let first = engine
        .try_register_ingest("pipeline-1", "stream-key", "srt")
        .await;
    assert!(first.is_some());
    assert!(
        engine
            .try_register_ingest("pipeline-1", "stream-key", "srt")
            .await
            .is_none()
    );

    engine.unregister_ingest("pipeline-1").await;
    let again = engine
        .try_register_ingest("pipeline-1", "stream-key", "srt")
        .await;
    assert!(again.is_some());
}

#[tokio::test]
async fn concurrent_publishers_cannot_both_reserve_the_same_pipeline() {
    let engine = Arc::new(MediaEngine::new());
    let first_engine = engine.clone();
    let second_engine = engine.clone();

    let (first, second) = tokio::join!(
        async move {
            first_engine
                .try_register_ingest("pipeline-race", "stream-key", "srt")
                .await
        },
        async move {
            second_engine
                .try_register_ingest("pipeline-race", "stream-key", "srt")
                .await
        }
    );

    assert_ne!(
        first.is_some(),
        second.is_some(),
        "exactly one publisher must win reservation"
    );
    assert_eq!(engine.ingests.active.read().await.len(), 1);
}

#[tokio::test]
async fn stale_ingest_unregister_cannot_clobber_replacement_attempt() {
    let engine = Arc::new(MediaEngine::new());

    let first = engine
        .try_register_ingest("pipeline-race", "stream-key", "rtmp")
        .await
        .expect("first ingest should register");
    engine.unregister_ingest("pipeline-race").await;

    let replacement = engine
        .try_register_ingest("pipeline-race", "stream-key", "srt")
        .await
        .expect("replacement ingest should register");

    first.finish(IngestEnd::Released).await;
    assert!(
        engine
            .with_active_ingest("pipeline-race", |ingest| ingest.attempt_id)
            .await
            .is_some_and(|attempt_id| attempt_id == replacement.registration().attempt_id),
        "replacement ingest must remain active after stale unregister"
    );
}
