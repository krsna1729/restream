//! Application-layer settings orchestration that assembles persisted config and
//! runtime-backed defaults into the snapshot exposed by the API layer.
//! This file owns cross-source settings reads; HTTP response shaping remains in
//! `crate::api`, while persistence details stay behind the existing ports.

use crate::application::ports::{IngestHostStore, MetaStore};
use crate::application::recording::load_recording_settings;
use crate::application::srt_ingest::load_global_srt_ingest_config;
use crate::domain::ingest_security::IngestSecurityConfig;
use crate::domain::recording::RecordingSettings;
use crate::domain::srt_ingest::SrtGlobalIngestConfig;
use crate::domain::transcode_profile::TranscodeProfiles;
use crate::media::security::IngestSecurityService;
use crate::planner::backend_policy::BackendPolicy;

pub const BACKEND_POLICY_META_KEY: &str = "backend_policy";

#[derive(Clone, Debug)]
pub struct SettingsSnapshot {
    pub server_name: String,
    pub ingest_host: String,
    pub ingest_security: IngestSecurityConfig,
    pub recording_settings: RecordingSettings,
    pub srt_ingest: SrtGlobalIngestConfig,
    pub transcode_profiles: TranscodeProfiles,
    pub backend_policy: BackendPolicy,
}

pub async fn load_backend_policy(
    meta_store: &dyn MetaStore,
    default_policy: BackendPolicy,
) -> BackendPolicy {
    meta_store
        .get_meta(BACKEND_POLICY_META_KEY)
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<BackendPolicy>(&raw).ok())
        .unwrap_or(default_policy)
}

pub async fn load_settings_snapshot(
    meta_store: &dyn MetaStore,
    ingest_host_store: &dyn IngestHostStore,
    security: &IngestSecurityService,
    default_backend_policy: BackendPolicy,
) -> Result<SettingsSnapshot, Box<dyn std::error::Error + Send + Sync>> {
    let server_name = meta_store
        .get_meta("server_name")
        .await?
        .unwrap_or_else(|| "Name".to_string());
    let ingest_host = ingest_host_store
        .get_ingest_host()
        .await?
        .unwrap_or_default();
    let recording_settings = load_recording_settings(meta_store).await;
    let srt_ingest = load_global_srt_ingest_config(meta_store, None, 16).await;
    let transcode_profiles = crate::media::profiles::current_effective().await;
    let backend_policy = load_backend_policy(meta_store, default_backend_policy).await;

    Ok(SettingsSnapshot {
        server_name,
        ingest_host,
        ingest_security: security.get_config(),
        recording_settings,
        srt_ingest,
        transcode_profiles,
        backend_policy,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;
    use crate::domain::ingest_security::DEFAULT_INGEST_SECURITY_CONFIG;
    use crate::infrastructure::sqlite_ports::SqliteMetaStore;
    use crate::media::security::IngestSecurityService;

    #[tokio::test]
    async fn load_settings_snapshot_combines_db_meta_and_runtime_defaults() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        db::setup_database_schema(&pool).await.unwrap();
        db::set_meta(&pool, "server_name", "Restream Control")
            .await
            .unwrap();
        db::set_ingest_host(&pool, "ingest.example.com")
            .await
            .unwrap();

        let security = IngestSecurityService::new(DEFAULT_INGEST_SECURITY_CONFIG);
        let store = SqliteMetaStore::new(pool);
        let snapshot = load_settings_snapshot(&store, &store, &security, BackendPolicy::default())
            .await
            .unwrap();

        assert_eq!(snapshot.server_name, "Restream Control");
        assert_eq!(snapshot.ingest_host, "ingest.example.com");
        assert_eq!(
            snapshot.ingest_security.failure_limit,
            DEFAULT_INGEST_SECURITY_CONFIG.failure_limit
        );
        assert_eq!(snapshot.recording_settings, RecordingSettings::default());
        assert_eq!(snapshot.srt_ingest, SrtGlobalIngestConfig::default());
        assert!(snapshot.transcode_profiles.contains_key("h264"));
        assert_eq!(snapshot.backend_policy, BackendPolicy::default());
    }

    #[tokio::test]
    async fn load_backend_policy_prefers_persisted_operator_policy() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        db::setup_database_schema(&pool).await.unwrap();
        db::set_meta(
            &pool,
            BACKEND_POLICY_META_KEY,
            r#"{"internalVideoPresets":true,"internalHevcToH264":false,"internalHlsPreview":true,"internalComplexAudio":false}"#,
        )
        .await
        .unwrap();

        let store = SqliteMetaStore::new(pool);
        let fallback = BackendPolicy {
            internal_video_presets: false,
            internal_hevc_to_h264: true,
            internal_hls_preview: false,
            internal_complex_audio: true,
        };
        let policy = load_backend_policy(&store, fallback).await;

        assert_eq!(
            policy,
            BackendPolicy {
                internal_video_presets: true,
                internal_hevc_to_h264: false,
                internal_hls_preview: true,
                internal_complex_audio: false,
            }
        );
    }

    #[tokio::test]
    async fn load_backend_policy_falls_back_to_default_when_no_meta_is_persisted() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        db::setup_database_schema(&pool).await.unwrap();
        let store = SqliteMetaStore::new(pool);
        let fallback = BackendPolicy {
            internal_video_presets: false,
            internal_hevc_to_h264: true,
            internal_hls_preview: false,
            internal_complex_audio: true,
        };

        let policy = load_backend_policy(&store, fallback).await;

        assert_eq!(policy, fallback);
    }

    #[tokio::test]
    async fn load_backend_policy_falls_back_to_default_when_persisted_json_is_malformed() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        db::setup_database_schema(&pool).await.unwrap();
        db::set_meta(&pool, BACKEND_POLICY_META_KEY, "{not valid json")
            .await
            .unwrap();
        let store = SqliteMetaStore::new(pool);
        let fallback = BackendPolicy {
            internal_video_presets: false,
            internal_hevc_to_h264: true,
            internal_hls_preview: false,
            internal_complex_audio: true,
        };

        let policy = load_backend_policy(&store, fallback).await;

        assert_eq!(policy, fallback);
    }

    #[tokio::test]
    async fn load_backend_policy_falls_back_to_default_when_persisted_json_is_wrong_shape() {
        let pool = db::create_pool("sqlite::memory:").await.unwrap();
        db::setup_database_schema(&pool).await.unwrap();
        db::set_meta(&pool, BACKEND_POLICY_META_KEY, r#"["not", "an", "object"]"#)
            .await
            .unwrap();
        let store = SqliteMetaStore::new(pool);
        let fallback = BackendPolicy {
            internal_video_presets: true,
            internal_hevc_to_h264: true,
            internal_hls_preview: true,
            internal_complex_audio: true,
        };

        let policy = load_backend_policy(&store, fallback).await;

        assert_eq!(policy, fallback);
    }
}
