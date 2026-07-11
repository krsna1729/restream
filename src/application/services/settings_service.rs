use std::sync::Arc;

use crate::application::ports::{IngestHostStore, JobStore, MetaStore, MetaStoreWriter};
use crate::application::recording::load_recording_enabled_map;
use crate::application::recording::{RecordingSettings, save_recording_settings};
use crate::application::settings::{BACKEND_POLICY_META_KEY, load_settings_snapshot};
use crate::application::srt_ingest::load_global_srt_ingest_config;
use crate::application::{
    ingest_security::save_ingest_security_config, transcode_profiles::save_transcode_profiles,
};
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::domain::transcode_profile::TranscodeProfiles;
use crate::media::security::IngestSecurityService;
use crate::media::srt::SrtIngestPolicyStore;
use crate::planner::backend_policy::BackendPolicy;
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
        default_backend_policy: BackendPolicy,
    ) -> ApiResult<crate::application::settings::SettingsSnapshot> {
        load_settings_snapshot(
            &*self.meta_store,
            &*self.ingest_host_store,
            security,
            default_backend_policy,
        )
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

    pub async fn save_ingest_security_config(
        &self,
        config: &IngestSecurityConfig,
    ) -> ApiResult<()> {
        save_ingest_security_config(self.meta_writer.as_ref(), config)
            .await
            .map_err(|e| ApiError::internal(format!("save ingest security config: {e}")))
    }

    pub async fn save_recording_settings(&self, settings: &RecordingSettings) -> ApiResult<()> {
        save_recording_settings(self.meta_writer.as_ref(), settings)
            .await
            .map_err(|e| ApiError::internal(format!("save recording settings: {e}")))
    }

    pub async fn save_transcode_profiles(&self, profiles: &TranscodeProfiles) -> ApiResult<()> {
        save_transcode_profiles(self.meta_writer.as_ref(), profiles)
            .await
            .map_err(|e| ApiError::internal(format!("save transcode profiles: {e}")))
    }

    pub async fn save_backend_policy(&self, policy: BackendPolicy) -> ApiResult<()> {
        let raw = serde_json::to_string(&policy)
            .map_err(|e| ApiError::internal(format!("serialize backend policy: {e}")))?;
        self.set_meta(BACKEND_POLICY_META_KEY, &raw).await
    }

    pub async fn refresh_srt_ingest_policy_store(
        &self,
        policy_store: &SrtIngestPolicyStore,
        srt_passphrase: Option<String>,
        srt_pbkeylen: i32,
    ) -> ApiResult<()> {
        let global =
            load_global_srt_ingest_config(self.meta_store.as_ref(), srt_passphrase, srt_pbkeylen)
                .await;
        let pipelines = self.list_pipelines().await?;
        policy_store.replace(global, &pipelines);
        Ok(())
    }

    pub async fn recording_enabled_map(
        &self,
        pipeline_ids: &[String],
    ) -> std::collections::HashMap<String, bool> {
        load_recording_enabled_map(self.meta_store.as_ref(), pipeline_ids).await
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
    use crate::application::srt_ingest::SRT_INGEST_GLOBAL_CONFIG_META_KEY;
    use crate::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
    use crate::domain::srt_ingest::{SrtGlobalIngestConfig, SrtGlobalIngestMode};
    use crate::media::security::IngestSecurityService;
    use crate::media::srt::SrtIngestPolicyStore;
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
        service
            .save_recording_settings(&RecordingSettings {
                retain_source_ts: true,
            })
            .await
            .unwrap();

        let security = IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG);
        let snapshot = service
            .load_snapshot(&security, BackendPolicy::default())
            .await
            .unwrap();

        assert_eq!(snapshot.server_name, "Studio");
        assert_eq!(snapshot.ingest_host, "edge.local");
        assert!(snapshot.recording_settings.retain_source_ts);
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

        let recording_enabled = service.recording_enabled_map(&["pipe-1".to_string()]).await;
        assert_eq!(recording_enabled.get("pipe-1"), Some(&false));

        service
            .set_meta(
                SRT_INGEST_GLOBAL_CONFIG_META_KEY,
                r#"{"mode":"encrypted","passphrase":"global-pass-123","pbkeylen":24}"#,
            )
            .await
            .unwrap();
        let policy_store = SrtIngestPolicyStore::new(SrtGlobalIngestConfig::default(), &[]);
        service
            .refresh_srt_ingest_policy_store(&policy_store, None, 16)
            .await
            .unwrap();
        assert_eq!(
            policy_store.global_config().mode,
            SrtGlobalIngestMode::Encrypted
        );
    }
}
