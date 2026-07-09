use std::sync::Arc;

use crate::application::ports::{
    IngestHostStore, JobStore, MetaStore, MetaStoreWriter, SqliteJobStore, SqliteMetaStore,
};
use crate::application::settings::load_settings_snapshot;
use crate::media::security::IngestSecurityService;
use crate::types::{Job, Output, Pipeline};

use super::error::{ApiError, ApiResult};
use super::output_service::OutputService;
use super::pipeline_service::PipelineService;

pub struct SettingsService {
    meta_store: Arc<dyn MetaStore>,
    meta_writer: Arc<dyn MetaStoreWriter>,
    ingest_host_store: Arc<dyn IngestHostStore>,
    job_store: Arc<dyn JobStore>,
    pipeline_service: PipelineService,
    output_service: OutputService,
}

impl SettingsService {
    pub fn new(db: sqlx::SqlitePool) -> Self {
        let meta_store = Arc::new(SqliteMetaStore::new(db.clone()));
        Self {
            pipeline_service: PipelineService::new(db.clone()),
            output_service: OutputService::new(db.clone()),
            meta_writer: meta_store.clone(),
            ingest_host_store: meta_store.clone(),
            meta_store,
            job_store: Arc::new(SqliteJobStore::new(db)),
        }
    }

    pub fn with_stores(
        meta_store: Arc<dyn MetaStore>,
        meta_writer: Arc<dyn MetaStoreWriter>,
        ingest_host_store: Arc<dyn IngestHostStore>,
        job_store: Arc<dyn JobStore>,
        pipeline_service: PipelineService,
        output_service: OutputService,
    ) -> Self {
        Self {
            meta_store,
            meta_writer,
            ingest_host_store,
            job_store,
            pipeline_service,
            output_service,
        }
    }

    pub async fn load_snapshot(
        &self,
        security: &IngestSecurityService,
    ) -> ApiResult<crate::application::settings::SettingsSnapshot> {
        load_settings_snapshot(&*self.meta_store, &*self.ingest_host_store, security)
            .await
            .map_err(|e| ApiError::internal(format!("load settings: {e}")))
    }

    pub async fn list_pipelines(&self) -> ApiResult<Vec<Pipeline>> {
        self.pipeline_service.list_pipelines().await
    }

    pub async fn list_outputs(&self) -> ApiResult<Vec<Output>> {
        self.output_service.list_outputs().await
    }

    pub async fn list_jobs(&self) -> ApiResult<Vec<Job>> {
        self.job_store
            .list_jobs()
            .await
            .map_err(|e| ApiError::internal(format!("list jobs: {e}")))
    }

    pub async fn get_ingest_host_raw(&self) -> ApiResult<String> {
        self.ingest_host_store
            .get_ingest_host()
            .await
            .map(|h| h.unwrap_or_default())
            .map_err(|e| ApiError::internal(format!("get ingest host: {e}")))
    }

    pub async fn set_server_name(&self, name: &str) -> ApiResult<()> {
        self.meta_writer
            .set_meta("server_name", name)
            .await
            .map(|_| ())
            .map_err(|e| ApiError::internal(format!("set server name: {e}")))
    }

    pub async fn set_ingest_host(&self, host: &str) -> ApiResult<()> {
        self.ingest_host_store
            .set_ingest_host(host)
            .await
            .map(|_| ())
            .map_err(|e| ApiError::internal(format!("set ingest host: {e}")))
    }

    pub async fn get_meta(&self, key: &str) -> ApiResult<Option<String>> {
        self.meta_store
            .get_meta(key)
            .await
            .map_err(|e| ApiError::internal(format!("get meta: {e}")))
    }

    pub async fn set_meta(&self, key: &str, value: &str) -> ApiResult<()> {
        self.meta_writer
            .set_meta(key, value)
            .await
            .map(|_| ())
            .map_err(|e| ApiError::internal(format!("set meta: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use std::sync::Mutex;

    use crate::application::ports::{
        JobListFuture, MetaLookupError, MetaLookupFuture, MetaWriteFuture,
    };
    use crate::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
    use crate::media::security::IngestSecurityService;
    use crate::types::JobStatus;

    #[derive(Default)]
    struct FakeSettingsStore {
        meta: Mutex<BTreeMap<String, String>>,
        jobs: Mutex<Vec<Job>>,
    }

    impl FakeSettingsStore {
        fn with_defaults() -> Self {
            let store = Self::default();
            store
                .meta
                .lock()
                .unwrap()
                .insert("server_name".to_string(), "Control".to_string());
            store
                .meta
                .lock()
                .unwrap()
                .insert("ingest_host".to_string(), "ingest.local".to_string());
            store.jobs.lock().unwrap().push(Job {
                id: "job-1".to_string(),
                pipeline_id: "pipe-1".to_string(),
                output_id: "out-1".to_string(),
                pid: Some(42),
                status: JobStatus::Running,
                started_at: "2026-07-09T00:00:00Z".to_string(),
                ended_at: None,
                exit_code: None,
                exit_signal: None,
            });
            store
        }
    }

    impl MetaStore for FakeSettingsStore {
        fn get_meta<'a>(&'a self, key: &'a str) -> MetaLookupFuture<'a> {
            Box::pin(async move { Ok(self.meta.lock().unwrap().get(key).cloned()) })
        }
    }

    impl MetaStoreWriter for FakeSettingsStore {
        fn set_meta<'a>(&'a self, key: &'a str, value: &'a str) -> MetaWriteFuture<'a> {
            Box::pin(async move {
                self.meta
                    .lock()
                    .unwrap()
                    .insert(key.to_string(), value.to_string());
                Ok(value.to_string())
            })
        }
    }

    impl IngestHostStore for FakeSettingsStore {
        fn get_ingest_host<'a>(&'a self) -> MetaLookupFuture<'a> {
            Box::pin(async move { Ok(self.meta.lock().unwrap().get("ingest_host").cloned()) })
        }

        fn set_ingest_host<'a>(&'a self, host: &'a str) -> MetaWriteFuture<'a> {
            Box::pin(async move {
                if host == "fail" {
                    return Err(MetaLookupError::new("ingest host failed"));
                }
                let trimmed = host.trim().to_string();
                self.meta
                    .lock()
                    .unwrap()
                    .insert("ingest_host".to_string(), trimmed.clone());
                Ok(trimmed)
            })
        }
    }

    impl JobStore for FakeSettingsStore {
        fn list_jobs<'a>(&'a self) -> JobListFuture<'a> {
            Box::pin(async move { Ok(self.jobs.lock().unwrap().clone()) })
        }
    }

    #[tokio::test]
    async fn settings_service_uses_injected_stores_for_snapshot_and_writes() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        let store = Arc::new(FakeSettingsStore::with_defaults());
        let service = SettingsService::with_stores(
            store.clone(),
            store.clone(),
            store.clone(),
            store,
            PipelineService::new(pool.clone()),
            OutputService::new(pool),
        );

        service.set_server_name("Studio").await.unwrap();
        service.set_ingest_host(" edge.local ").await.unwrap();
        service
            .set_meta("custom_encoding", "-c:v copy")
            .await
            .unwrap();

        let security = IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG);
        let snapshot = service.load_snapshot(&security).await.unwrap();

        assert_eq!(snapshot.server_name, "Studio");
        assert_eq!(snapshot.ingest_host, "edge.local");
        assert_eq!(
            service
                .get_meta("custom_encoding")
                .await
                .unwrap()
                .as_deref(),
            Some("-c:v copy")
        );
        assert_eq!(service.get_ingest_host_raw().await.unwrap(), "edge.local");
        assert_eq!(service.list_jobs().await.unwrap()[0].id, "job-1");
        assert!(service.list_pipelines().await.unwrap().is_empty());
        assert!(service.list_outputs().await.unwrap().is_empty());
    }
}
