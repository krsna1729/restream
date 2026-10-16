use super::*;
use crate::domain::output_spec::OutputConfig;
use crate::domain::state::DesiredOutputState;

async fn pool() -> SqlitePool {
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    pool
}

async fn outputs(pool: &SqlitePool) -> Vec<Output> {
    crate::db::list_outputs(pool)
        .await
        .unwrap()
        .into_iter()
        .map(crate::application::outputs::from_record)
        .collect()
}

async fn create_output(pool: &SqlitePool, id: &str, pipeline_id: &str) {
    crate::db::create_output(
        pool,
        id,
        pipeline_id,
        id,
        "rtmp://dest.example/live/k",
        None,
        DesiredOutputState::Running,
        &OutputConfig::source(),
    )
    .await
    .unwrap();
}

fn forwarded_input(engine: &MediaEngine, pipeline_id: &str) -> Option<String> {
    engine
        .ingests
        .active
        .try_read()
        .unwrap()
        .get(pipeline_id)
        .map(|ingest| ingest.input_id.clone())
}

#[tokio::test]
async fn a_deleted_output_loses_its_egress_and_its_sibling_keeps_running() {
    let pool = pool().await;
    let engine = MediaEngine::new();
    crate::db::create_pipeline(&pool, "pipe", "P", "key-p", None, None)
        .await
        .unwrap();
    create_output(&pool, "out-deleted", "pipe").await;
    create_output(&pool, "out-kept", "pipe").await;
    let deleted = engine
        .register_egress("out-deleted", "pipe", "rtmp://a")
        .await;
    let kept = engine.register_egress("out-kept", "pipe", "rtmp://b").await;

    assert!(
        crate::db::delete_output(&pool, "pipe", "out-deleted")
            .await
            .unwrap()
    );
    converge_topology(&pool, &engine, &outputs(&pool).await).await;

    assert!(deleted.is_cancelled());
    assert!(!kept.is_cancelled());
    assert_eq!(
        engine.registered_egress_output_ids().await,
        vec!["out-kept".to_string()]
    );
}

#[tokio::test]
async fn a_deleted_pipeline_loses_its_runtime_and_other_pipelines_keep_theirs() {
    let pool = pool().await;
    let engine = MediaEngine::new();
    for (id, key) in [("pipe-gone", "key-gone"), ("pipe-kept", "key-kept")] {
        crate::db::create_pipeline(&pool, id, id, key, None, None)
            .await
            .unwrap();
        let _ = engine.get_or_create_pipeline(id).await;
    }
    create_output(&pool, "out-gone", "pipe-gone").await;
    let egress = engine
        .register_egress("out-gone", "pipe-gone", "rtmp://a")
        .await;
    let gone_ingest = engine
        .try_register_ingest_attempt("pipe-gone", "key-gone", "rtmp")
        .await
        .unwrap();
    let kept_ingest = engine
        .try_register_ingest_attempt("pipe-kept", "key-kept", "rtmp")
        .await
        .unwrap();
    // An input preview can key a ring; it is not a pipeline.
    let preview = crate::media::engine_hls::input_hls_preview_resource_id("input-x");
    let _ = engine.get_or_create_pipeline(&preview).await;

    assert!(
        crate::db::delete_pipeline(&pool, "pipe-gone")
            .await
            .unwrap()
    );
    converge_topology(&pool, &engine, &outputs(&pool).await).await;

    assert!(gone_ingest.cancel_token.is_cancelled());
    assert!(egress.is_cancelled(), "the output row cascaded away");
    assert!(!kept_ingest.cancel_token.is_cancelled());
    assert_eq!(
        engine.runtime_pipeline_ids().await,
        HashSet::from(["pipe-kept".to_string()])
    );
    assert!(engine.ingests.pipelines.read().await.contains_key(&preview));
}

#[tokio::test]
async fn a_persisted_promotion_switches_the_forwarded_input_on_request() {
    let pool = pool().await;
    let engine = MediaEngine::new();
    let requests = ReconcileRequests::new();
    crate::db::create_pipeline(&pool, "pipe", "P", "key-p", None, None)
        .await
        .unwrap();
    crate::db::create_pipeline_input(&pool, "input-backup", "pipe", "Backup", "key-b")
        .await
        .unwrap();
    let primary = engine
        .try_register_pipeline_input_attempt("pipe", "input_primary_pipe", "key-p", "rtmp", true)
        .await
        .unwrap();
    let _backup = engine
        .try_register_pipeline_input_attempt("pipe", "input-backup", "key-b", "rtmp", false)
        .await
        .unwrap();

    crate::db::promote_pipeline_input(&pool, "pipe", "input-backup")
        .await
        .unwrap()
        .unwrap();
    apply_requested_selections(&pool, &engine, &requests).await;
    converge_topology(&pool, &engine, &[]).await;
    assert_eq!(
        forwarded_input(&engine, "pipe").as_deref(),
        Some("input_primary_pipe"),
        "selection changes only on request",
    );

    requests.input_selection_changed("pipe");
    apply_requested_selections(&pool, &engine, &requests).await;
    assert_eq!(
        forwarded_input(&engine, "pipe").as_deref(),
        Some("input-backup")
    );
    assert!(!primary.cancel_token.is_cancelled(), "demoted, not dropped");
}

#[tokio::test]
async fn a_pass_never_demotes_a_stream_key_session_without_a_request() {
    // File ingests and legacy publishers register under the stream key, so
    // the engine's selected id differs from the persisted input row id.
    let pool = pool().await;
    let engine = MediaEngine::new();
    crate::db::create_pipeline(&pool, "pipe", "P", "key-p", None, None)
        .await
        .unwrap();
    let file_ingest = engine
        .try_register_ingest_attempt("pipe", "key-p", "file")
        .await
        .unwrap();

    apply_requested_selections(&pool, &engine, &ReconcileRequests::new()).await;
    converge_topology(&pool, &engine, &[]).await;

    assert_eq!(forwarded_input(&engine, "pipe").as_deref(), Some("key-p"));
    assert!(!file_ingest.cancel_token.is_cancelled());
}

#[test]
fn a_wake_makes_exactly_one_pass_due() {
    let requests = ReconcileRequests::new();
    let now = Instant::now();
    assert!(!requests.take_due(now), "idle: no pass");
    requests.wake();
    requests.wake();
    assert!(requests.take_due(now), "wakes coalesce into one pass");
    assert!(!requests.take_due(now));
}

#[test]
fn a_deadline_makes_a_pass_due_only_once_it_is_reached() {
    let requests = ReconcileRequests::new();
    let now = Instant::now();
    let soon = now + std::time::Duration::from_secs(1);
    let later = now + std::time::Duration::from_secs(5);
    requests.wake_at(later);
    requests.wake_at(soon);
    assert_eq!(requests.next_deadline(), Some(soon));
    assert!(!requests.take_due(now));
    assert!(requests.take_due(soon), "a deadline is due at its instant");
    assert_eq!(
        requests.next_deadline(),
        Some(later),
        "later deadlines survive"
    );
    assert!(!requests.take_due(soon));
    assert!(requests.take_due(later + std::time::Duration::from_secs(1)));
    assert_eq!(requests.next_deadline(), None);
}

#[test]
fn lifecycle_events_request_the_passes_their_effects_need() {
    use crate::events::EventKind;
    let grace = std::time::Duration::from_secs(5);
    let requests = ReconcileRequests::new();
    let before = Instant::now();
    request_pass_for_event(
        &requests,
        &EventKind::IngestDisconnected {
            pipeline_id: "pipe".into(),
            protocol: "rtmp".into(),
        },
        grace,
    );
    assert!(requests.take_due(Instant::now()), "outputs react at once");
    let grace_end = requests
        .next_deadline()
        .expect("a pass when the grace ends");
    assert!(
        grace_end > before + grace,
        "after the grace, not at its edge"
    );
    assert!(grace_end < Instant::now() + grace + std::time::Duration::from_secs(1));

    let requests = ReconcileRequests::new();
    request_pass_for_event(
        &requests,
        &EventKind::EgressStopped {
            pipeline_id: "pipe".into(),
            output_id: "out".into(),
        },
        grace,
    );
    assert!(
        requests.take_due(Instant::now()),
        "an ended egress is retried or swept"
    );

    let requests = ReconcileRequests::new();
    request_pass_for_event(
        &requests,
        &EventKind::IngestProbed {
            pipeline_id: "pipe".into(),
            protocol: "hls".into(),
        },
        grace,
    );
    assert!(
        requests.take_due(Instant::now()),
        "an output held for the probe starts on the next pass"
    );

    let requests = ReconcileRequests::new();
    request_pass_for_event(
        &requests,
        &EventKind::EgressStarted {
            pipeline_id: "pipe".into(),
            output_id: "out".into(),
        },
        grace,
    );
    assert!(
        !requests.take_due(Instant::now()),
        "a start changes nothing to converge"
    );
    assert_eq!(requests.next_deadline(), None);

    // Each error of a still-active egress (every upload retry) must not
    // drive a whole pass; its end does, through EgressStopped.
    request_pass_for_event(
        &requests,
        &EventKind::EgressFailed {
            pipeline_id: "pipe".into(),
            output_id: "out".into(),
            phase: "upload_segment".into(),
            error: "upload failed".into(),
        },
        grace,
    );
    assert!(
        !requests.take_due(Instant::now()),
        "an egress error is not a pass"
    );
}
