use super::*;

#[tokio::test]
async fn pipeline_create_and_remove() {
    let engine = MediaEngine::new();
    let rb1 = engine.get_or_create_pipeline("p1").await;
    let rb2 = engine.get_or_create_pipeline("p1").await;
    // Same pipeline returns same buffer
    assert!(Arc::ptr_eq(&rb1, &rb2));

    engine.remove_pipeline("p1").await;
    let rb3 = engine.get_or_create_pipeline("p1").await;
    // After removal, new buffer is created
    assert!(!Arc::ptr_eq(&rb1, &rb3));
}

#[tokio::test]
async fn recording_lifecycle() {
    let engine = MediaEngine::new();
    assert!(!engine.is_recording_active("p1").await);

    let token = engine.register_recording("p1").await.unwrap();
    assert!(engine.is_recording_active("p1").await);
    assert!(!token.is_cancelled());

    engine.unregister_recording("p1").await;
    assert!(!engine.is_recording_active("p1").await);
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn concurrent_register_recording_calls_never_both_succeed() {
    // Two near-simultaneous recording-start requests for the same pipeline
    // (e.g. a double-click or a retried API call) must not both register a
    // token: the loser would otherwise silently orphan the winner's token
    // (leaking its writer thread/file handle with no way to cancel it) and
    // both would go on to race `std::fs::File::create` on the same output
    // filename. Exactly one of the two racing calls must receive `Some`.
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "p-racing-recorders";

    let barrier = Arc::new(tokio::sync::Barrier::new(2));
    let mut handles = Vec::new();
    for _ in 0..2 {
        let engine = engine.clone();
        let barrier = barrier.clone();
        handles.push(tokio::spawn(async move {
            barrier.wait().await;
            engine.register_recording(pipeline_id).await
        }));
    }

    let mut results = Vec::new();
    for handle in handles {
        results.push(handle.await.unwrap());
    }

    let successes = results.iter().filter(|r| r.is_some()).count();
    assert_eq!(
        successes, 1,
        "exactly one of two racing register_recording calls must succeed, got {successes}"
    );

    let winner = results.into_iter().flatten().next().unwrap();
    assert!(engine.is_recording_active(pipeline_id).await);
    winner.cancel();
    assert!(!engine.is_recording_active(pipeline_id).await);
}

#[tokio::test]
async fn cancelled_recording_token_is_not_active() {
    let engine = MediaEngine::new();
    let token = engine.register_recording("p-cancelled-rec").await.unwrap();

    assert!(engine.is_recording_active("p-cancelled-rec").await);
    token.cancel();

    assert!(
        !engine.is_recording_active("p-cancelled-rec").await,
        "cancelled recording token must not be reported as active"
    );
}
