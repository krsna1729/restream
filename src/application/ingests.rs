//! Application-owned ingest catalog helpers over concrete SQLite persistence.
//!
//! Catalog CRUD and filename-scoped mutation live here. File-ingest runtime
//! coordination stays on `FileIngestService` / `application::ingest` until that
//! lattice collapses. Persistence is `db::*` — no IngestService.

use sqlx::SqlitePool;

use crate::application::models::Ingest;
use crate::application::services::error::{ServiceError, ServiceResult};

fn ingest_from_record(record: crate::db::IngestRecord) -> Ingest {
    Ingest {
        id: record.id,
        filename: record.filename,
        stream_key: record.stream_key,
        loop_flag: record.loop_flag,
        start_time: record.start_time,
        live_optimized: record.live_optimized,
        target_gop_seconds: record.target_gop_seconds,
    }
}

fn ingest_not_found(id: &str) -> ServiceError {
    ServiceError::not_found(format!("ingest {id} not found"))
}

/// Lists every persisted ingest record without transport-level filtering.
pub async fn list_ingests(pool: &SqlitePool) -> ServiceResult<Vec<Ingest>> {
    crate::db::list_ingests(pool)
        .await
        .map(|records| records.into_iter().map(ingest_from_record).collect())
        .map_err(|e| ServiceError::internal(format!("list ingests: {e}")))
}

/// Resolves one ingest by ID; missing rows become a stable not-found error.
pub async fn get_by_id(pool: &SqlitePool, id: &str) -> ServiceResult<Ingest> {
    crate::db::get_ingest(pool, id)
        .await
        .map_err(|e| ServiceError::internal(format!("get ingest: {e}")))?
        .map(ingest_from_record)
        .ok_or_else(|| ingest_not_found(id))
}

/// Persists a new ingest record with the caller-provided media source and flags.
#[allow(clippy::too_many_arguments)]
pub async fn create_ingest(
    pool: &SqlitePool,
    id: &str,
    filename: &str,
    stream_key: &str,
    loop_flag: bool,
    start_time: &str,
    live_optimized: bool,
    target_gop_seconds: u32,
) -> ServiceResult<Ingest> {
    crate::db::create_ingest(
        pool,
        id,
        filename,
        stream_key,
        loop_flag,
        start_time,
        live_optimized,
        target_gop_seconds,
    )
    .await
    .map(ingest_from_record)
    .map_err(|e| ServiceError::internal(format!("create ingest: {e}")))
}

/// Updates one persisted ingest; missing rows become a stable not-found error.
#[allow(clippy::too_many_arguments)]
pub async fn update_ingest(
    pool: &SqlitePool,
    id: &str,
    filename: &str,
    stream_key: &str,
    loop_flag: bool,
    start_time: &str,
    live_optimized: bool,
    target_gop_seconds: u32,
) -> ServiceResult<Ingest> {
    crate::db::update_ingest(
        pool,
        id,
        filename,
        stream_key,
        loop_flag,
        start_time,
        live_optimized,
        target_gop_seconds,
    )
    .await
    .map_err(|e| ServiceError::internal(format!("update ingest: {e}")))?
    .map(ingest_from_record)
    .ok_or_else(|| ingest_not_found(id))
}

/// Renames one ingest's backing filename without disturbing concurrent fields.
pub async fn update_ingest_filename(
    pool: &SqlitePool,
    id: &str,
    filename: &str,
) -> ServiceResult<Ingest> {
    crate::db::update_ingest_filename(pool, id, filename)
        .await
        .map_err(|e| ServiceError::internal(format!("update ingest filename: {e}")))?
        .map(ingest_from_record)
        .ok_or_else(|| ingest_not_found(id))
}

/// Lists all ingest records that point at one media-library filename.
pub async fn list_for_filename(pool: &SqlitePool, filename: &str) -> ServiceResult<Vec<Ingest>> {
    crate::db::list_ingests_for_filename(pool, filename)
        .await
        .map(|records| records.into_iter().map(ingest_from_record).collect())
        .map_err(|e| ServiceError::internal(format!("list ingests for filename: {e}")))
}

/// Deletes one persisted ingest record from the catalog.
pub async fn delete_ingest(pool: &SqlitePool, id: &str) -> ServiceResult<bool> {
    crate::db::delete_ingest(pool, id)
        .await
        .map_err(|e| ServiceError::internal(format!("delete ingest: {e}")))
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
    async fn create_list_update_filename_and_delete_round_trip() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        create_ingest(
            &pool,
            "ing-1",
            "clip.mp4",
            "stream-key",
            true,
            "now",
            false,
            2,
        )
        .await
        .unwrap();
        assert_eq!(list_ingests(&pool).await.unwrap().len(), 1);
        let updated = update_ingest_filename(&pool, "ing-1", "clip2.mp4")
            .await
            .unwrap();
        assert_eq!(updated.filename, "clip2.mp4");
        assert_eq!(
            list_for_filename(&pool, "clip2.mp4").await.unwrap().len(),
            1
        );
        assert!(delete_ingest(&pool, "ing-1").await.unwrap());
        assert!(list_ingests(&pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn missing_update_is_not_found() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        let err = update_ingest(
            &pool,
            "missing",
            "clip.mp4",
            "stream-key",
            false,
            "",
            false,
            2,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, ServiceError::NotFound(_)));
    }
}
