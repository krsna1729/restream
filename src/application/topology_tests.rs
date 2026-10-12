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
    converge_topology(
        &pool,
        &engine,
        &ReconcileRequests::new(),
        &outputs(&pool).await,
    )
    .await;

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
    converge_topology(
        &pool,
        &engine,
        &ReconcileRequests::new(),
        &outputs(&pool).await,
    )
    .await;

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
    converge_topology(&pool, &engine, &requests, &[]).await;
    assert_eq!(
        forwarded_input(&engine, "pipe").as_deref(),
        Some("input_primary_pipe"),
        "selection changes only on request",
    );

    requests.input_selection_changed("pipe");
    converge_topology(&pool, &engine, &requests, &[]).await;
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

    converge_topology(&pool, &engine, &ReconcileRequests::new(), &[]).await;

    assert_eq!(forwarded_input(&engine, "pipe").as_deref(), Some("key-p"));
    assert!(!file_ingest.cancel_token.is_cancelled());
}
