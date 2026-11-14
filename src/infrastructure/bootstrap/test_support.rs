//! Setup shared by the reconciler, egress and recording catalog writer
//! tests: production's two pools on a file database, held connections, and
//! a live output.

use std::sync::Arc;
use std::time::Duration;

use crate::db::{create_lifecycle_pool, create_pool, setup_database_schema};
use restream_engine::engine::MediaEngine;

pub(crate) async fn memory_pool() -> sqlx::SqlitePool {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    setup_database_schema(&pool).await.unwrap();
    pool
}

/// A file database with production's two pools (API and lifecycle).
pub(crate) async fn file_pools(
    tag: &str,
) -> (sqlx::SqlitePool, sqlx::SqlitePool, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "restream-bootstrap-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("sqlite://{}", dir.join("db.sqlite").display());
    let api = create_pool(&url).await.unwrap();
    setup_database_schema(&api).await.unwrap();
    let lifecycle = create_lifecycle_pool(&url).await.unwrap();
    (api, lifecycle, dir)
}

pub(super) async fn hold_every_connection(
    pool: &sqlx::SqlitePool,
) -> Vec<sqlx::pool::PoolConnection<sqlx::Sqlite>> {
    let mut held = Vec::new();
    while let Ok(connection) =
        tokio::time::timeout(Duration::from_millis(200), pool.acquire()).await
    {
        held.push(connection.unwrap());
    }
    assert!(!held.is_empty());
    held
}

/// The write lock another writer (an API transaction) holds until
/// [`release_write_lock`].
pub(crate) async fn hold_write_lock(
    pool: &sqlx::SqlitePool,
) -> sqlx::pool::PoolConnection<sqlx::Sqlite> {
    let mut connection = pool.acquire().await.unwrap();
    sqlx::query("BEGIN IMMEDIATE")
        .execute(&mut *connection)
        .await
        .unwrap();
    connection
}

pub(crate) async fn release_write_lock(mut connection: sqlx::pool::PoolConnection<sqlx::Sqlite>) {
    sqlx::query("ROLLBACK")
        .execute(&mut *connection)
        .await
        .unwrap();
}

/// A pipeline with a live, probed ingest and one output, `out-{pipeline}`,
/// that wants to run.
pub(super) async fn live_output(
    engine: &Arc<MediaEngine>,
    pool: &sqlx::SqlitePool,
    pipeline_id: &str,
    url: &str,
) -> restream_engine::ingest_lease::IngestLease {
    let stream_key = format!("key-{pipeline_id}");
    crate::db::create_pipeline(pool, pipeline_id, pipeline_id, &stream_key, None, None)
        .await
        .unwrap();
    crate::db::create_output(
        pool,
        &format!("out-{pipeline_id}"),
        pipeline_id,
        "out",
        url,
        None,
        restream_domain::domain::state::DesiredOutputState::Running,
        &restream_domain::domain::output_spec::OutputConfig::source(),
    )
    .await
    .unwrap();
    let lease = engine
        .try_register_ingest(pipeline_id, &stream_key, "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(restream_core::metadata::VideoMeta {
                codec: "h264".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    lease
}

/// The output rows, as a pass reads them.
pub(super) async fn outputs(pool: &sqlx::SqlitePool) -> Vec<restream_domain::models::Output> {
    crate::db::list_outputs(pool)
        .await
        .unwrap()
        .into_iter()
        .map(crate::application::outputs::from_record)
        .collect()
}

pub(super) async fn output_starts_within(
    engine: &MediaEngine,
    output_id: &str,
    within: Duration,
) -> bool {
    tokio::time::timeout(within, async {
        while !engine.has_active_egress(output_id).await {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .is_ok()
}
