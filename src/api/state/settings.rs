//! Dashboard settings operations: metadata, ingest host, jobs, recording,
//! security, and transcode profiles.

use super::AppState;
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_domain::domain::recording::RecordingSettings;
use restream_domain::domain::transcode_profile::TranscodeProfiles;

impl AppState {
    /// Lists background conversion jobs.
    pub(crate) async fn settings_jobs(
        &self,
    ) -> crate::application::services::ServiceResult<Vec<restream_domain::models::Job>> {
        self.settings_service.list_jobs().await
    }

    /// Returns the raw configured ingest host.
    pub(crate) async fn ingest_host_raw(
        &self,
    ) -> crate::application::services::ServiceResult<String> {
        self.settings_service.get_ingest_host_raw().await
    }

    /// Lists the pipelines shown in settings.
    pub(crate) async fn settings_pipelines(
        &self,
    ) -> crate::application::services::ServiceResult<Vec<restream_domain::models::Pipeline>> {
        self.settings_service.list_pipelines().await
    }

    /// Reads one settings metadata value.
    pub(crate) async fn settings_meta(
        &self,
        key: &str,
    ) -> crate::application::services::ServiceResult<Option<String>> {
        self.settings_service.get_meta(key).await
    }

    /// Writes one settings metadata value.
    pub(crate) async fn set_settings_meta(
        &self,
        key: &str,
        value: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.settings_service.set_meta(key, value).await
    }

    /// Stores the server name.
    pub(crate) async fn set_server_name(
        &self,
        name: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.settings_service.set_server_name(name).await
    }

    /// Stores the ingest host.
    pub(crate) async fn set_ingest_host(
        &self,
        host: &str,
    ) -> crate::application::services::ServiceResult<()> {
        self.settings_service.set_ingest_host(host).await
    }

    /// Stores the ingest security configuration.
    pub(crate) async fn save_ingest_security_config(
        &self,
        config: &IngestSecurityConfig,
    ) -> crate::application::services::ServiceResult<()> {
        self.settings_service
            .save_ingest_security_config(config)
            .await
    }

    /// Stores recording settings.
    pub(crate) async fn save_recording_settings(
        &self,
        settings: &RecordingSettings,
    ) -> crate::application::services::ServiceResult<()> {
        self.settings_service
            .save_recording_settings(settings)
            .await
    }

    /// Returns which pipelines record to the media library.
    pub(crate) async fn recording_enabled_by_pipeline(
        &self,
        pipeline_ids: &[String],
    ) -> std::collections::HashMap<String, bool> {
        self.settings_service
            .recording_enabled_map(&self.engine, pipeline_ids)
            .await
    }

    /// Stores transcode profiles.
    pub(crate) async fn save_transcode_profiles(
        &self,
        profiles: &TranscodeProfiles,
    ) -> crate::application::services::ServiceResult<()> {
        self.settings_service
            .save_transcode_profiles(profiles)
            .await
    }
}
