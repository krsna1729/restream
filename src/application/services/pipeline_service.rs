//! Pipeline catalog reads and admission-aware mutations.

use std::sync::Arc;

use crate::application::catalog_auth::CatalogAuth;
use crate::db::{self, DeadlineDb};
use restream_domain::models::Pipeline;

use crate::application::service_error::{ServiceError, ServiceResult};

/// Application service for pipeline CRUD and read operations.
/// Each write runs with a reload of the ingest admission index as one task
/// ([`CatalogAuth::commit`]) and returns after the reload, so the next
/// connect sees it. If that reload fails, the write still returns its own
/// answer and the previous index stays in force until the retry task or the
/// periodic reload succeeds.
#[derive(Clone)]
pub struct PipelineService {
    db: DeadlineDb,
    catalog_auth: Arc<CatalogAuth>,
}

impl PipelineService {
    pub fn new(db: DeadlineDb, catalog_auth: Arc<CatalogAuth>) -> Self {
        Self { db, catalog_auth }
    }

    /// Builds the shared not-found error shape used when callers reference a
    /// pipeline ID that no longer exists in the catalog.
    fn pipeline_not_found(id: &str) -> ServiceError {
        ServiceError::not_found(format!("pipeline {id} not found"))
    }

    /// Lists every persisted pipeline record without applying transport-level
    /// filtering or shaping.
    pub async fn list_pipelines(&self) -> ServiceResult<Vec<Pipeline>> {
        self.db
            .read(db::list_pipelines)
            .await
            .map(|rows| rows.into_iter().map(Pipeline::from).collect())
            .map_err(|e| ServiceError::internal(format!("list pipelines: {e}")))
    }

    /// Resolves one pipeline by ID and upgrades missing-store results into the
    /// service layer's stable not-found error.
    pub async fn get_by_id(&self, id: &str) -> ServiceResult<Pipeline> {
        self.db
            .read(|pool| db::get_pipeline(pool, id))
            .await
            .map_err(|e| ServiceError::internal(format!("get pipeline: {e}")))?
            .ok_or_else(|| Self::pipeline_not_found(id))
            .map(Pipeline::from)
    }

    /// Looks up a pipeline by publish stream key for ingest routing and other
    /// catalog lookups that start from transport credentials.
    pub async fn get_by_stream_key(&self, stream_key: &str) -> ServiceResult<Option<Pipeline>> {
        self.db
            .read(|pool| db::get_pipeline_by_stream_key(pool, stream_key))
            .await
            .map(|row| row.map(Pipeline::from))
            .map_err(|e| ServiceError::internal(format!("get pipeline by stream key: {e}")))
    }

    /// Persists a new pipeline record with the caller-provided stream key,
    /// source, and optional serialized SRT ingest policy.
    pub async fn create_pipeline(
        &self,
        id: &str,
        name: &str,
        stream_key: &str,
        input_source: Option<&str>,
        srt_ingest_policy: Option<&str>,
    ) -> ServiceResult<Pipeline> {
        let db = self.db.clone();
        let (id, name, stream_key) = (id.to_owned(), name.to_owned(), stream_key.to_owned());
        let input_source = input_source.map(str::to_owned);
        let srt_ingest_policy = srt_ingest_policy.map(str::to_owned);
        self.catalog_auth
            .commit(async move {
                db.write(move |pool| async move {
                    db::create_pipeline(
                        &pool,
                        &id,
                        &name,
                        &stream_key,
                        input_source.as_deref(),
                        srt_ingest_policy.as_deref(),
                    )
                    .await
                })
                .await
            })
            .await
            .map(Pipeline::from)
            .map_err(|e| ServiceError::internal(format!("create pipeline: {e}")))
    }

    /// Updates the mutable fields of one persisted pipeline and normalizes a
    /// missing row into the service layer's stable not-found error.
    pub async fn update_pipeline(
        &self,
        id: &str,
        name: &str,
        stream_key: &str,
        input_source: Option<&str>,
        srt_ingest_policy: Option<&str>,
    ) -> ServiceResult<Pipeline> {
        let db = self.db.clone();
        let owned = (id.to_owned(), name.to_owned(), stream_key.to_owned());
        let input_source = input_source.map(str::to_owned);
        let srt_ingest_policy = srt_ingest_policy.map(str::to_owned);
        self.catalog_auth
            .commit(async move {
                db.write(move |pool| async move {
                    let (id, name, stream_key) = owned;
                    db::update_pipeline(
                        &pool,
                        &id,
                        &name,
                        &stream_key,
                        input_source.as_deref(),
                        srt_ingest_policy.as_deref(),
                    )
                    .await
                })
                .await
            })
            .await
            .map_err(|e| ServiceError::internal(format!("update pipeline: {e}")))?
            .ok_or_else(|| Self::pipeline_not_found(id))
            .map(Pipeline::from)
    }

    /// Deletes one pipeline record from the persisted catalog.
    pub async fn delete_pipeline(&self, id: &str) -> ServiceResult<bool> {
        let db = self.db.clone();
        let id = id.to_owned();
        self.catalog_auth
            .commit(async move {
                db.write(move |pool| async move { db::delete_pipeline(&pool, &id).await })
                    .await
            })
            .await
            .map_err(|e| ServiceError::internal(format!("delete pipeline: {e}")))
    }

    /// List all pipeline IDs (used by health and settings).
    pub async fn list_pipeline_ids(&self) -> ServiceResult<Vec<String>> {
        let pipelines = self.list_pipelines().await?;
        Ok(pipelines.into_iter().map(|p| p.id).collect())
    }

    /// Return the configured ingest host, or the default.
    pub async fn get_ingest_host(&self) -> String {
        ingest_host_or_default(self.db.read(db::get_ingest_host).await.ok().flatten())
    }
}

/// The stored ingest host, or `localhost` when none (or an empty one) is set.
pub fn ingest_host_or_default(stored: Option<String>) -> String {
    stored
        .filter(|host| !host.is_empty())
        .unwrap_or_else(|| "localhost".to_string())
}
