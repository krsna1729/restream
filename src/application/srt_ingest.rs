//! Application-layer SRT ingest configuration loading and policy-store refresh
//! that connect persisted settings and pipeline catalogs to runtime enforcement.

use std::collections::HashMap;

use crate::application::services::{ServiceError, ServiceResult};
use crate::db::{self, DeadlineDb};
use restream_domain::domain::pipeline_input::PipelineInput;
use restream_domain::domain::srt_ingest::{
    SrtGlobalIngestConfig, SrtGlobalIngestMode, SrtPipelineIngestConfig,
};
use restream_domain::models::Pipeline;
use restream_engine::srt::{SrtIngestPolicyEntry, SrtIngestPolicyStore};
use tracing::warn;

pub const SRT_INGEST_GLOBAL_CONFIG_META_KEY: &str = "srt_ingest_global_config";

pub fn parse_persisted_srt_ingest_policy(raw: Option<&str>) -> Option<SrtPipelineIngestConfig> {
    raw.and_then(|value| serde_json::from_str(value).ok())
}

pub fn serialize_persisted_srt_ingest_policy(
    config: &SrtPipelineIngestConfig,
) -> Result<String, serde_json::Error> {
    serde_json::to_string(config)
}

pub async fn load_global_srt_ingest_config(
    db: &DeadlineDb,
    srt_passphrase: Option<String>,
    srt_pbkeylen: i32,
) -> SrtGlobalIngestConfig {
    let from_store = db
        .read(|pool| crate::db::get_meta(pool, SRT_INGEST_GLOBAL_CONFIG_META_KEY))
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<SrtGlobalIngestConfig>(&raw).ok());
    let mut config = from_store
        .or_else(|| srt_global_config_from_appconfig(srt_passphrase, srt_pbkeylen))
        .unwrap_or_default();
    if let Err(error) = config.validate() {
        if matches!(config.mode, SrtGlobalIngestMode::Encrypted) {
            warn!(err = %error, "invalid encrypted SRT ingest config; preserving fail-closed policy");
        } else {
            warn!(err = %error, "invalid global SRT ingest config; falling back to plaintext");
            config = SrtGlobalIngestConfig::default();
        }
    }
    config
}

pub async fn load_policy_store(
    db: &DeadlineDb,
    srt_passphrase: Option<String>,
    srt_pbkeylen: i32,
) -> ServiceResult<SrtIngestPolicyStore> {
    let (global, entries) = load_policy_snapshot(db, srt_passphrase, srt_pbkeylen).await?;
    Ok(SrtIngestPolicyStore::new(global, &entries))
}

pub(super) async fn refresh_policy_store(
    policy_store: &SrtIngestPolicyStore,
    db: &DeadlineDb,
    srt_passphrase: Option<String>,
    srt_pbkeylen: i32,
) -> ServiceResult<()> {
    let (global, entries) = load_policy_snapshot(db, srt_passphrase, srt_pbkeylen).await?;
    policy_store.replace(global, &entries);
    Ok(())
}

async fn load_policy_snapshot(
    db: &DeadlineDb,
    srt_passphrase: Option<String>,
    srt_pbkeylen: i32,
) -> ServiceResult<(SrtGlobalIngestConfig, Vec<SrtIngestPolicyEntry>)> {
    let global = load_global_srt_ingest_config(db, srt_passphrase, srt_pbkeylen).await;
    let pipelines = db
        .read(db::list_pipelines)
        .await
        .map_err(|error| ServiceError::internal(format!("list pipelines: {error}")))?
        .into_iter()
        .map(Pipeline::from)
        .collect::<Vec<_>>();
    let mut inputs = Vec::new();
    for pipeline in &pipelines {
        inputs.extend(
            db.read(|pool| db::with_busy_retry(|| db::list_pipeline_inputs(pool, &pipeline.id)))
                .await
                .map_err(|error| {
                    ServiceError::internal(format!("list pipeline inputs: {error}"))
                })?,
        );
    }
    Ok((global, srt_ingest_policy_entries(&pipelines, &inputs)))
}

fn srt_ingest_policy_entries(
    pipelines: &[Pipeline],
    inputs: &[PipelineInput],
) -> Vec<SrtIngestPolicyEntry> {
    let policies = pipelines
        .iter()
        .map(|pipeline| {
            (
                pipeline.id.as_str(),
                parse_persisted_srt_ingest_policy(pipeline.srt_ingest_policy.as_deref())
                    .unwrap_or_default(),
            )
        })
        .collect::<HashMap<_, _>>();
    inputs
        .iter()
        .filter(|input| input.enabled)
        // The policy store answers a publishing SRT peer by stream key, and a
        // pull input accepts no peer: its key must not resolve there.
        .filter(|input| {
            input.source_kind
                == restream_domain::domain::pipeline_input::PipelineInputSourceKind::Keyed
        })
        .filter_map(|input| {
            let policy = policies.get(input.pipeline_id.as_str())?;
            Some(SrtIngestPolicyEntry::new(
                input.pipeline_id.as_str(),
                input.stream_key.as_str(),
                policy.clone(),
            ))
        })
        .collect()
}

fn srt_global_config_from_appconfig(
    passphrase: Option<String>,
    pbkeylen: i32,
) -> Option<SrtGlobalIngestConfig> {
    let passphrase = passphrase?;
    if passphrase.is_empty() {
        return None;
    }
    Some(SrtGlobalIngestConfig {
        mode: SrtGlobalIngestMode::Encrypted,
        passphrase: Some(passphrase),
        pbkeylen,
        ..SrtGlobalIngestConfig::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::bootstrap::test_support::memory_pool;
    use restream_domain::domain::pipeline_input::PipelineInputRole;
    use restream_domain::domain::srt_ingest::{
        DEFAULT_SRT_INGEST_LATENCY_MS, ResolvedSrtCrypto, ResolvedSrtIngestConfig,
    };
    use restream_domain::models::Pipeline;

    /// An in-memory database whose global SRT config is `value`.
    async fn meta_db(value: Option<serde_json::Value>) -> DeadlineDb {
        let pool = memory_pool().await;
        if let Some(value) = value {
            crate::db::set_meta(&pool, SRT_INGEST_GLOBAL_CONFIG_META_KEY, &value.to_string())
                .await
                .unwrap();
        }
        DeadlineDb::api(pool)
    }

    async fn policy_db(value: serde_json::Value) -> DeadlineDb {
        let db = meta_db(Some(value)).await;
        db.write(|pool| async move {
            let policy =
                serialize_persisted_srt_ingest_policy(&SrtPipelineIngestConfig::default()).unwrap();
            db::create_pipeline(
                &pool,
                "pipeline-1",
                "Pipeline One",
                "stream-one",
                None,
                Some(&policy),
            )
            .await
        })
        .await
        .unwrap();
        db
    }

    #[tokio::test]
    async fn global_srt_ingest_config_loads_from_meta_store() {
        let store = meta_db(Some(serde_json::json!({
            "mode": "encrypted",
            "passphrase": "secret-pass-123",
            "pbkeylen": 24
        })))
        .await;

        let config = load_global_srt_ingest_config(&store, None, 16).await;

        assert_eq!(config.mode, SrtGlobalIngestMode::Encrypted);
        assert_eq!(config.passphrase.as_deref(), Some("secret-pass-123"));
        assert_eq!(config.pbkeylen, 24);
    }

    #[tokio::test]
    async fn invalid_encrypted_global_srt_ingest_config_fails_closed() {
        let store = meta_db(Some(serde_json::json!({
            "mode": "encrypted",
            "passphrase": "short",
            "pbkeylen": 99
        })))
        .await;

        let config = load_global_srt_ingest_config(&store, None, 16).await;

        assert_eq!(config.mode, SrtGlobalIngestMode::Encrypted);
        assert!(config.resolve().is_err());
    }

    #[tokio::test]
    async fn invalid_plaintext_global_srt_ingest_config_falls_back_to_default() {
        let store = meta_db(Some(serde_json::json!({
            "mode": "plaintext",
            "pbkeylen": 99
        })))
        .await;

        let config = load_global_srt_ingest_config(&store, None, 16).await;

        assert_eq!(config, SrtGlobalIngestConfig::default());
    }

    #[tokio::test]
    async fn load_policy_store_builds_store_from_meta_and_catalog() {
        let store = policy_db(serde_json::json!({
            "mode": "encrypted",
            "passphrase": "global-pass-123",
            "pbkeylen": 24
        }))
        .await;
        let policy_store = load_policy_store(&store, None, 16).await.unwrap();

        assert_eq!(
            policy_store.global_config().mode,
            SrtGlobalIngestMode::Encrypted
        );
        assert_eq!(
            policy_store.resolved_policy("stream-one"),
            Some(ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Encrypted {
                    passphrase: "global-pass-123".to_string(),
                    pbkeylen: 24,
                },
                latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
            })
        );
    }

    #[tokio::test]
    async fn refresh_policy_store_replaces_existing_policies() {
        let initial_store = policy_db(serde_json::json!({
            "mode": "plaintext"
        }))
        .await;
        let updated_store = policy_db(serde_json::json!({
            "mode": "encrypted",
            "passphrase": "updated-pass-123",
            "pbkeylen": 32
        }))
        .await;
        let policy_store = load_policy_store(&initial_store, None, 16).await.unwrap();

        refresh_policy_store(&policy_store, &updated_store, None, 16)
            .await
            .unwrap();

        assert_eq!(
            policy_store.global_config().mode,
            SrtGlobalIngestMode::Encrypted
        );
        assert_eq!(
            policy_store.resolved_policy("stream-one"),
            Some(ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Encrypted {
                    passphrase: "updated-pass-123".to_string(),
                    pbkeylen: 32,
                },
                latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
            })
        );
    }

    #[test]
    fn srt_policy_entries_cover_every_enabled_keyed_input() {
        let pipeline = Pipeline {
            id: "pipeline-1".to_string(),
            name: "Pipeline One".to_string(),
            stream_key: "primary-key".to_string(),
            input_source: None,
            srt_ingest_policy: Some(
                serialize_persisted_srt_ingest_policy(&SrtPipelineIngestConfig::default())
                    .expect("serialize inherited policy"),
            ),
        };
        let inputs = vec![
            PipelineInput {
                id: "input-primary".to_string(),
                pipeline_id: pipeline.id.clone(),
                label: "Primary".to_string(),
                stream_key: "primary-key".to_string(),
                role: PipelineInputRole::Primary,
                enabled: true,
                selected: true,
                ..PipelineInput::keyed_default()
            },
            PipelineInput {
                id: "input-standby".to_string(),
                pipeline_id: pipeline.id.clone(),
                label: "Standby".to_string(),
                stream_key: "standby-key".to_string(),
                role: PipelineInputRole::Backup,
                enabled: true,
                selected: false,
                ..PipelineInput::keyed_default()
            },
            PipelineInput {
                id: "input-disabled".to_string(),
                pipeline_id: pipeline.id.clone(),
                label: "Disabled".to_string(),
                stream_key: "disabled-key".to_string(),
                role: PipelineInputRole::Backup,
                enabled: false,
                selected: false,
                ..PipelineInput::keyed_default()
            },
            PipelineInput {
                id: "input-pull".to_string(),
                pipeline_id: pipeline.id.clone(),
                label: "Pull".to_string(),
                stream_key: "pull-key".to_string(),
                role: PipelineInputRole::Backup,
                enabled: true,
                selected: false,
                source_kind:
                    restream_domain::domain::pipeline_input::PipelineInputSourceKind::HlsPull,
                source_url: Some("http://origin.example/master.m3u8".to_string()),
                ..PipelineInput::keyed_default()
            },
        ];

        let entries = srt_ingest_policy_entries(&[pipeline], &inputs);
        let stream_keys = entries
            .iter()
            .map(|entry| entry.stream_key.as_str())
            .collect::<Vec<_>>();

        // An enabled pull input is left out: the policy store answers a
        // publishing SRT peer by stream key, and a pull input takes no peer.
        assert_eq!(stream_keys, vec!["primary-key", "standby-key"]);
        assert!(
            entries
                .iter()
                .all(|entry| entry.policy == SrtPipelineIngestConfig::default())
        );
    }

    #[test]
    fn persisted_policy_translation_distinguishes_valid_json_from_invalid_or_absent_values() {
        let policy = SrtPipelineIngestConfig::default();
        let serialized = serialize_persisted_srt_ingest_policy(&policy).expect("serialize policy");

        assert_eq!(
            parse_persisted_srt_ingest_policy(Some(&serialized)),
            Some(policy)
        );
        assert_eq!(parse_persisted_srt_ingest_policy(Some("{ not json")), None);
        assert_eq!(parse_persisted_srt_ingest_policy(None), None);
    }

    #[test]
    fn malformed_and_absent_persisted_policies_both_become_inherited_typed_entries() {
        let pipelines = [
            Pipeline {
                id: "malformed".to_string(),
                name: "Malformed".to_string(),
                stream_key: "malformed-key".to_string(),
                input_source: None,
                srt_ingest_policy: Some("{ not json".to_string()),
            },
            Pipeline {
                id: "absent".to_string(),
                name: "Absent".to_string(),
                stream_key: "absent-key".to_string(),
                input_source: None,
                srt_ingest_policy: None,
            },
        ];
        let inputs = [
            pipeline_input("malformed", "malformed-key"),
            pipeline_input("absent", "absent-key"),
        ];

        let entries = srt_ingest_policy_entries(&pipelines, &inputs);

        assert_eq!(entries.len(), 2);
        assert!(
            entries
                .iter()
                .all(|entry| entry.policy == SrtPipelineIngestConfig::default())
        );
    }

    fn pipeline_input(pipeline_id: &str, stream_key: &str) -> PipelineInput {
        PipelineInput {
            id: format!("input-{stream_key}"),
            pipeline_id: pipeline_id.to_string(),
            label: "Input".to_string(),
            stream_key: stream_key.to_string(),
            role: PipelineInputRole::Primary,
            enabled: true,
            selected: true,
            ..PipelineInput::keyed_default()
        }
    }

    #[test]
    fn srt_global_config_from_appconfig_returns_none_without_passphrase() {
        assert!(srt_global_config_from_appconfig(None, 16).is_none());
    }

    #[test]
    fn srt_global_config_from_appconfig_treats_empty_passphrase_as_absent() {
        assert!(srt_global_config_from_appconfig(Some(String::new()), 16).is_none());
    }

    #[test]
    fn srt_global_config_from_appconfig_builds_encrypted_config_from_passphrase() {
        let config = srt_global_config_from_appconfig(Some("app-pass-123".to_string()), 24)
            .expect("passphrase present");

        assert_eq!(config.mode, SrtGlobalIngestMode::Encrypted);
        assert_eq!(config.passphrase.as_deref(), Some("app-pass-123"));
        assert_eq!(config.pbkeylen, 24);
    }

    #[tokio::test]
    async fn global_srt_ingest_config_falls_back_to_appconfig_passphrase_when_meta_store_empty() {
        let store = meta_db(None).await;

        let config =
            load_global_srt_ingest_config(&store, Some("app-pass-123".to_string()), 24).await;

        assert_eq!(config.mode, SrtGlobalIngestMode::Encrypted);
        assert_eq!(config.passphrase.as_deref(), Some("app-pass-123"));
        assert_eq!(config.pbkeylen, 24);
    }

    #[tokio::test]
    async fn global_srt_ingest_config_falls_back_to_default_when_no_meta_and_no_passphrase() {
        let store = meta_db(None).await;

        let config = load_global_srt_ingest_config(&store, None, 16).await;

        assert_eq!(config, SrtGlobalIngestConfig::default());
    }

    #[tokio::test]
    async fn global_srt_ingest_config_prefers_meta_store_over_appconfig_passphrase() {
        let store = meta_db(Some(serde_json::json!({
            "mode": "plaintext"
        })))
        .await;

        let config =
            load_global_srt_ingest_config(&store, Some("app-pass-123".to_string()), 24).await;

        assert_eq!(config.mode, SrtGlobalIngestMode::Plaintext);
    }
}
