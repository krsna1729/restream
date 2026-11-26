//! Application service wrapper for dashboard settings reads and writes.
//!
//! This module keeps settings-specific meta keys and cross-store refresh logic
//! on the application boundary so handlers can work in terms of operator
//! settings rather than persistence details.

use crate::application::recording::load_recording_enabled_map;
use crate::application::recording::save_recording_settings;
use crate::application::settings::load_settings_snapshot;
use crate::application::srt_ingest::refresh_policy_store;
use crate::application::{
    ingest_security::save_ingest_security_config, transcode_profiles::save_transcode_profiles,
};
use crate::db::DeadlineDb;
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_domain::domain::recording::RecordingSettings;
use restream_domain::domain::transcode_profile::TranscodeProfiles;
use restream_domain::models::{Job, Pipeline};
use restream_engine::security::IngestSecurityService;
use restream_engine::srt::SrtIngestPolicyStore;

use super::pipeline_service::PipelineService;
use crate::application::service_error::{ServiceError, ServiceResult};

const SERVER_NAME_META_KEY: &str = "server_name";

/// Application service that assembles the operator-facing settings surface from
/// meta/config stores plus pipeline and output catalog reads.
pub struct SettingsService {
    db: DeadlineDb,
    pipeline_service: PipelineService,
}

impl SettingsService {
    pub fn new(db: DeadlineDb, pipeline_service: PipelineService) -> Self {
        Self {
            db,
            pipeline_service,
        }
    }

    /// Loads the full dashboard settings snapshot by combining persisted meta
    /// values, ingest-host configuration, and runtime security defaults.
    pub async fn load_snapshot(
        &self,
        security: &IngestSecurityService,
    ) -> ServiceResult<crate::application::settings::SettingsSnapshot> {
        load_settings_snapshot(&self.db, security)
            .await
            .map_err(|e| ServiceError::internal(format!("load settings: {e}")))
    }

    /// Lists pipelines for settings views that need the current catalog while
    /// staying independent of pipeline-store details.
    pub async fn list_pipelines(&self) -> ServiceResult<Vec<Pipeline>> {
        self.pipeline_service.list_pipelines().await
    }

    /// Lists background jobs that should appear in the operator settings view.
    pub async fn list_jobs(&self) -> ServiceResult<Vec<Job>> {
        let jobs = self
            .db
            .read(crate::db::list_jobs)
            .await
            .map_err(|e| ServiceError::internal(format!("list jobs: {e}")))?;
        Ok(jobs.into_iter().map(Job::from).collect())
    }

    /// Returns the raw persisted ingest host value without applying the
    /// pipeline service's localhost fallback.
    pub async fn get_ingest_host_raw(&self) -> ServiceResult<String> {
        self.db
            .read(crate::db::get_ingest_host)
            .await
            .map(|h| h.unwrap_or_default())
            .map_err(|e| ServiceError::internal(format!("get ingest host: {e}")))
    }

    /// Persists the operator-visible server name using the shared meta write
    /// path used by other settings keys.
    pub async fn set_server_name(&self, name: &str) -> ServiceResult<()> {
        self.set_meta(SERVER_NAME_META_KEY, name).await
    }

    /// Persists the ingest host override that pipeline and dashboard views use
    /// for generated publish URLs.
    pub async fn set_ingest_host(&self, host: &str) -> ServiceResult<()> {
        let host = host.to_owned();
        self.db
            .write(move |pool| async move { crate::db::set_ingest_host(&pool, &host).await })
            .await
            .map(|_| ())
            .map_err(|e| ServiceError::internal(format!("set ingest host: {e}")))
    }

    /// Reads one arbitrary settings meta entry for callers that work with
    /// feature-specific persisted values.
    pub async fn get_meta(&self, key: &str) -> ServiceResult<Option<String>> {
        self.db
            .read(|pool| crate::db::get_meta(pool, key))
            .await
            .map_err(|e| ServiceError::internal(format!("get meta: {e}")))
    }

    /// Persists one arbitrary settings meta entry.
    pub async fn set_meta(&self, key: &str, value: &str) -> ServiceResult<()> {
        let (key, value) = (key.to_owned(), value.to_owned());
        self.db
            .write(move |pool| async move { crate::db::set_meta(&pool, &key, &value).await })
            .await
            .map(|_| ())
            .map_err(|e| ServiceError::internal(format!("set meta: {e}")))
    }

    /// Saves the ingest security configuration through the shared meta-backed
    /// application helper.
    pub async fn save_ingest_security_config(
        &self,
        config: &IngestSecurityConfig,
    ) -> ServiceResult<()> {
        save_ingest_security_config(&self.db, config)
            .await
            .map_err(|e| ServiceError::internal(format!("save ingest security config: {e}")))
    }

    /// Saves recording settings that influence operator-visible recording
    /// behavior across pipelines.
    pub async fn save_recording_settings(&self, settings: &RecordingSettings) -> ServiceResult<()> {
        save_recording_settings(&self.db, settings)
            .await
            .map_err(|e| ServiceError::internal(format!("save recording settings: {e}")))
    }

    /// Saves the transcode profile catalog exposed through dashboard settings.
    pub async fn save_transcode_profiles(&self, profiles: &TranscodeProfiles) -> ServiceResult<()> {
        save_transcode_profiles(&self.db, profiles)
            .await
            .map_err(|e| ServiceError::internal(format!("save transcode profiles: {e}")))
    }

    /// Rebuilds the in-memory SRT ingest policy store from persisted global
    /// settings plus the current pipeline catalog so runtime lookups see one
    /// coherent snapshot.
    pub async fn refresh_srt_ingest_policy_store(
        &self,
        policy_store: &SrtIngestPolicyStore,
        srt_passphrase: Option<String>,
        srt_pbkeylen: i32,
    ) -> ServiceResult<()> {
        refresh_policy_store(policy_store, &self.db, srt_passphrase, srt_pbkeylen).await
    }

    /// Loads the per-pipeline recording toggle map used by health and settings
    /// views that need to annotate many pipelines at once; a toggle that
    /// cannot be read keeps the pipeline's current recording state.
    pub async fn recording_enabled_map(
        &self,
        engine: &restream_engine::engine::MediaEngine,
        pipeline_ids: &[String],
    ) -> std::collections::HashMap<String, bool> {
        load_recording_enabled_map(&self.db, engine, pipeline_ids).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::catalog_auth::CatalogAuth;
    use crate::application::srt_ingest::SRT_INGEST_GLOBAL_CONFIG_META_KEY;
    use restream_domain::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
    use restream_domain::domain::output_spec::OutputConfig;
    use restream_domain::domain::srt_ingest::{SrtGlobalIngestConfig, SrtGlobalIngestMode};
    use restream_domain::domain::state::DesiredOutputState;
    use restream_domain::models::JobStatus;
    use restream_engine::security::IngestSecurityService;
    use restream_engine::srt::SrtIngestPolicyStore;

    #[tokio::test]
    async fn settings_service_reads_and_writes_the_settings_surface() {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        crate::db::set_meta(&pool, SERVER_NAME_META_KEY, "Control")
            .await
            .unwrap();
        crate::db::create_pipeline(&pool, "pipe-1", "Pipeline", "key-1", None, None)
            .await
            .unwrap();
        crate::db::create_output(
            &pool,
            "out-1",
            "pipe-1",
            "Out",
            "rtmp://127.0.0.1:9/live/out",
            None,
            DesiredOutputState::Stopped,
            &OutputConfig::source(),
        )
        .await
        .unwrap();
        crate::db::create_job(
            &pool,
            "job-1",
            "pipe-1",
            "out-1",
            JobStatus::Running,
            "2026-07-09T00:00:00Z",
        )
        .await
        .unwrap();
        let db = DeadlineDb::api(pool.clone());
        let pipelines = PipelineService::new(db.clone(), CatalogAuth::new(db.clone()));
        let service = SettingsService::new(db, pipelines);

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
        let snapshot = service.load_snapshot(&security).await.unwrap();

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
        let pipelines = service.list_pipelines().await.unwrap();
        assert_eq!(pipelines.len(), 1);
        assert_eq!(pipelines[0].id, "pipe-1");

        let recording_enabled = service
            .recording_enabled_map(
                &restream_engine::engine::MediaEngine::new(),
                &["pipe-1".to_string()],
            )
            .await;
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
