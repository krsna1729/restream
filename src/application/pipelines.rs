//! Application-owned pipeline catalog helpers over concrete SQLite persistence.
//!
//! Catalog CRUD and ingest-host fallback live here. Narrow input-source
//! mutation stays on `PipelineStore` for ingest paths until that lattice
//! collapses. Persistence is `db::*` for these helpers — no PipelineService.

use sqlx::SqlitePool;

use crate::application::models::Pipeline;
use crate::application::services::error::{ServiceError, ServiceResult};

fn pipeline_from_record(record: crate::db::PipelineRecord) -> Pipeline {
    Pipeline {
        id: record.id,
        name: record.name,
        stream_key: record.stream_key,
        input_source: record.input_source,
        srt_ingest_policy: record.srt_ingest_policy,
    }
}

fn pipeline_not_found(id: &str) -> ServiceError {
    ServiceError::not_found(format!("pipeline {id} not found"))
}

/// Lists every persisted pipeline record without transport-level filtering.
pub async fn list_pipelines(pool: &SqlitePool) -> ServiceResult<Vec<Pipeline>> {
    crate::db::list_pipelines(pool)
        .await
        .map(|records| records.into_iter().map(pipeline_from_record).collect())
        .map_err(|e| ServiceError::internal(format!("list pipelines: {e}")))
}

/// Resolves one pipeline by ID; missing rows become a stable not-found error.
pub async fn get_by_id(pool: &SqlitePool, id: &str) -> ServiceResult<Pipeline> {
    crate::db::get_pipeline(pool, id)
        .await
        .map_err(|e| ServiceError::internal(format!("get pipeline: {e}")))?
        .map(pipeline_from_record)
        .ok_or_else(|| pipeline_not_found(id))
}

/// Looks up a pipeline by publish stream key for ingest routing.
pub async fn get_by_stream_key(
    pool: &SqlitePool,
    stream_key: &str,
) -> ServiceResult<Option<Pipeline>> {
    crate::db::get_pipeline_by_stream_key(pool, stream_key)
        .await
        .map(|record| record.map(pipeline_from_record))
        .map_err(|e| ServiceError::internal(format!("get pipeline by stream key: {e}")))
}

/// Persists a new pipeline record with the caller-provided stream key and policy.
pub async fn create_pipeline(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    stream_key: &str,
    input_source: Option<&str>,
    srt_ingest_policy: Option<&str>,
) -> ServiceResult<Pipeline> {
    crate::db::create_pipeline(pool, id, name, stream_key, input_source, srt_ingest_policy)
        .await
        .map(pipeline_from_record)
        .map_err(|e| ServiceError::internal(format!("create pipeline: {e}")))
}

/// Updates mutable fields of one persisted pipeline.
pub async fn update_pipeline(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    stream_key: &str,
    input_source: Option<&str>,
    srt_ingest_policy: Option<&str>,
) -> ServiceResult<Pipeline> {
    crate::db::update_pipeline(pool, id, name, stream_key, input_source, srt_ingest_policy)
        .await
        .map_err(|e| ServiceError::internal(format!("update pipeline: {e}")))?
        .map(pipeline_from_record)
        .ok_or_else(|| pipeline_not_found(id))
}

/// Deletes one pipeline record from the persisted catalog.
pub async fn delete_pipeline(pool: &SqlitePool, id: &str) -> ServiceResult<bool> {
    crate::db::delete_pipeline(pool, id)
        .await
        .map_err(|e| ServiceError::internal(format!("delete pipeline: {e}")))
}

/// Lists every pipeline ID for health/alerts surfaces.
pub async fn list_pipeline_ids(pool: &SqlitePool) -> ServiceResult<Vec<String>> {
    Ok(list_pipelines(pool)
        .await?
        .into_iter()
        .map(|pipeline| pipeline.id)
        .collect())
}

/// Returns the configured ingest host, or `localhost` when unset/empty.
pub async fn get_ingest_host(pool: &SqlitePool) -> String {
    crate::db::get_ingest_host(pool)
        .await
        .ok()
        .flatten()
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn get_by_id_maps_missing_rows_to_not_found() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        let error = get_by_id(&pool, "missing").await.unwrap_err();
        assert!(error.to_string().contains("not found"));
    }

    #[tokio::test]
    async fn get_ingest_host_defaults_to_localhost() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        assert_eq!(get_ingest_host(&pool).await, "localhost");
    }

    #[tokio::test]
    async fn create_and_list_round_trip() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        create_pipeline(&pool, "pipe-1", "Pipeline", "key-1", None, None)
            .await
            .unwrap();
        let listed = list_pipelines(&pool).await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].stream_key, "key-1");
        assert_eq!(list_pipeline_ids(&pool).await.unwrap(), vec!["pipe-1"]);
    }
}
