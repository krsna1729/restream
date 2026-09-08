//! Application-owned output catalog helpers over concrete SQLite persistence.
//!
//! Desired-state start/stop stays here as thin domain writes; reconciler owns
//! runtime egress. Persistence is `db::*` — no OutputStore port.

use sqlx::SqlitePool;

use crate::application::models::Output;
use crate::application::services::error::{ServiceError, ServiceResult};
use crate::domain::output_spec::OutputConfig;
use crate::domain::state::DesiredOutputState;

fn output_from_record(record: crate::db::OutputRecord) -> Output {
    Output {
        id: record.id,
        pipeline_id: record.pipeline_id,
        name: record.name,
        url: record.url,
        monitoring_url: record.monitoring_url,
        desired_state: record.desired_state,
        config: record.config,
    }
}

/// Maps a persisted output row into the application catalog model.
pub(crate) fn from_record(record: crate::db::OutputRecord) -> Output {
    output_from_record(record)
}

fn output_not_found(id: &str) -> ServiceError {
    ServiceError::not_found(format!("output {id} not found"))
}

/// Lists every persisted output record without applying pipeline filters.
pub async fn list_outputs(pool: &SqlitePool) -> ServiceResult<Vec<Output>> {
    crate::db::list_outputs(pool)
        .await
        .map(|records| records.into_iter().map(output_from_record).collect())
        .map_err(|e| ServiceError::internal(format!("list outputs: {e}")))
}

/// Lists the outputs attached to one pipeline for dashboard detail views
/// and runtime coordination.
pub async fn list_for_pipeline(pool: &SqlitePool, pipeline_id: &str) -> ServiceResult<Vec<Output>> {
    crate::db::list_outputs_for_pipeline(pool, pipeline_id)
        .await
        .map(|records| records.into_iter().map(output_from_record).collect())
        .map_err(|e| ServiceError::internal(format!("list outputs for pipeline: {e}")))
}

/// Resolves one persisted output by composite pipeline/output identity.
pub async fn get_by_id(pool: &SqlitePool, pipeline_id: &str, id: &str) -> ServiceResult<Output> {
    crate::db::get_output(pool, pipeline_id, id)
        .await
        .map_err(|e| ServiceError::internal(format!("get output: {e}")))?
        .map(output_from_record)
        .ok_or_else(|| output_not_found(id))
}

/// Persists a new output with an already-typed desired-state value.
#[allow(clippy::too_many_arguments)]
pub async fn create_output(
    pool: &SqlitePool,
    id: &str,
    pipeline_id: &str,
    name: &str,
    url: &str,
    monitoring_url: Option<&str>,
    desired_state: DesiredOutputState,
    config: &OutputConfig,
) -> ServiceResult<Output> {
    crate::db::create_output(
        pool,
        id,
        pipeline_id,
        name,
        url,
        monitoring_url,
        desired_state,
        config,
    )
    .await
    .map(output_from_record)
    .map_err(|e| ServiceError::internal(format!("create output: {e}")))
}

/// Updates the mutable settings for one persisted output while preserving
/// its desired-state lifecycle fields.
pub async fn update_output(
    pool: &SqlitePool,
    pipeline_id: &str,
    id: &str,
    name: &str,
    url: &str,
    monitoring_url: Option<&str>,
    config: &OutputConfig,
) -> ServiceResult<Output> {
    crate::db::update_output(pool, pipeline_id, id, name, url, monitoring_url, config)
        .await
        .map_err(|e| ServiceError::internal(format!("update output: {e}")))?
        .map(output_from_record)
        .ok_or_else(|| output_not_found(id))
}

/// Deletes one persisted output record by pipeline/output identity.
pub async fn delete_output(pool: &SqlitePool, pipeline_id: &str, id: &str) -> ServiceResult<bool> {
    crate::db::delete_output(pool, pipeline_id, id)
        .await
        .map_err(|e| ServiceError::internal(format!("delete output: {e}")))
}

async fn desired_state_request(
    pool: &SqlitePool,
    pipeline_id: &str,
    id: &str,
    desired_state: DesiredOutputState,
    action: &'static str,
) -> ServiceResult<Output> {
    crate::db::set_output_desired_state(pool, pipeline_id, id, desired_state)
        .await
        .map(output_from_record)
        .map_err(|e| ServiceError::internal(format!("{action}: {e}")))
}

/// Set the output's desired state to `running`, resuming any stopped egress.
pub async fn request_start(
    pool: &SqlitePool,
    pipeline_id: &str,
    id: &str,
) -> ServiceResult<Output> {
    desired_state_request(
        pool,
        pipeline_id,
        id,
        DesiredOutputState::Running,
        "request start",
    )
    .await
}

/// Set the output's desired state to `stopped`, halting any active egress.
pub async fn request_stop(pool: &SqlitePool, pipeline_id: &str, id: &str) -> ServiceResult<Output> {
    desired_state_request(
        pool,
        pipeline_id,
        id,
        DesiredOutputState::Stopped,
        "request stop",
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn seed_output(pool: &SqlitePool) {
        crate::db::create_pipeline(pool, "pipe-1", "Pipeline", "key-1", None, None)
            .await
            .unwrap();
        crate::db::create_output(
            pool,
            "out-1",
            "pipe-1",
            "Output",
            "rtmp://localhost/live/key",
            None,
            DesiredOutputState::Running,
            &OutputConfig::default(),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn lifecycle_start_stop_updates_desired_state() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        seed_output(&pool).await;

        let stopped = request_stop(&pool, "pipe-1", "out-1").await.unwrap();
        assert_eq!(stopped.desired_state, DesiredOutputState::Stopped);

        let running = request_start(&pool, "pipe-1", "out-1").await.unwrap();
        assert_eq!(running.desired_state, DesiredOutputState::Running);
    }

    #[tokio::test]
    async fn get_by_id_maps_missing_rows_to_not_found() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        let error = get_by_id(&pool, "pipe-1", "missing").await.unwrap_err();
        assert!(error.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn create_output_persists_typed_desired_state() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::create_pipeline(&pool, "pipe-1", "Pipeline", "key-1", None, None)
            .await
            .unwrap();

        let created = create_output(
            &pool,
            "out-2",
            "pipe-1",
            "Created",
            "rtmp://localhost/live/two",
            None,
            DesiredOutputState::Stopped,
            &OutputConfig::default(),
        )
        .await
        .unwrap();
        assert_eq!(created.desired_state, DesiredOutputState::Stopped);
        assert_eq!(list_for_pipeline(&pool, "pipe-1").await.unwrap().len(), 1);
    }
}
