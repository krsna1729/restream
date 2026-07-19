//! Application-layer SRT ingest configuration loading and policy-store refresh
//! that connect persisted settings and pipeline catalogs to runtime enforcement.

use std::collections::HashMap;

use crate::application::models::Pipeline;
use crate::application::pipeline_inputs::{PipelineInputStore, PipelineInputStoreError};
use crate::application::ports::{MetaStore, PipelineStore, PipelineStoreError};
use crate::domain::pipeline_input::PipelineInput;
use crate::domain::srt_ingest::{SrtGlobalIngestConfig, SrtGlobalIngestMode};
use crate::media::srt::{SrtIngestPolicyEntry, SrtIngestPolicyStore};
use tracing::warn;

pub const SRT_INGEST_GLOBAL_CONFIG_META_KEY: &str = "srt_ingest_global_config";

pub async fn load_global_srt_ingest_config(
    meta_store: &dyn MetaStore,
    srt_passphrase: Option<String>,
    srt_pbkeylen: i32,
) -> SrtGlobalIngestConfig {
    let from_store = meta_store
        .get_meta(SRT_INGEST_GLOBAL_CONFIG_META_KEY)
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
    meta_store: &dyn MetaStore,
    pipeline_catalog: &dyn PipelineStore,
    input_store: &dyn PipelineInputStore,
    srt_passphrase: Option<String>,
    srt_pbkeylen: i32,
) -> Result<SrtIngestPolicyStore, PipelineStoreError> {
    let global = load_global_srt_ingest_config(meta_store, srt_passphrase, srt_pbkeylen).await;
    let pipelines = pipeline_catalog.list_pipelines().await?;
    let inputs = list_pipeline_inputs(input_store, &pipelines).await?;
    let entries = srt_ingest_policy_entries(&pipelines, &inputs);
    Ok(SrtIngestPolicyStore::new(global, &entries))
}

pub async fn refresh_policy_store(
    policy_store: &SrtIngestPolicyStore,
    meta_store: &dyn MetaStore,
    pipeline_catalog: &dyn PipelineStore,
    input_store: &dyn PipelineInputStore,
    srt_passphrase: Option<String>,
    srt_pbkeylen: i32,
) -> Result<(), PipelineStoreError> {
    let global = load_global_srt_ingest_config(meta_store, srt_passphrase, srt_pbkeylen).await;
    let pipelines = pipeline_catalog.list_pipelines().await?;
    let inputs = list_pipeline_inputs(input_store, &pipelines).await?;
    let entries = srt_ingest_policy_entries(&pipelines, &inputs);
    policy_store.replace(global, &entries);
    Ok(())
}

pub async fn list_pipeline_inputs(
    input_store: &dyn PipelineInputStore,
    pipelines: &[Pipeline],
) -> Result<Vec<PipelineInput>, PipelineStoreError> {
    let mut inputs = Vec::new();
    for pipeline in pipelines {
        inputs.extend(
            input_store
                .list(&pipeline.id)
                .await
                .map_err(map_input_store_error)?,
        );
    }
    Ok(inputs)
}

pub fn srt_ingest_policy_entries(
    pipelines: &[Pipeline],
    inputs: &[PipelineInput],
) -> Vec<SrtIngestPolicyEntry> {
    let policies = pipelines
        .iter()
        .map(|pipeline| (pipeline.id.as_str(), pipeline.srt_ingest_policy.clone()))
        .collect::<HashMap<_, _>>();
    inputs
        .iter()
        .filter(|input| input.enabled)
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

fn map_input_store_error(error: PipelineInputStoreError) -> PipelineStoreError {
    PipelineStoreError::new(format!("list pipeline inputs: {error}"))
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
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::models::Pipeline;
    use crate::application::pipeline_inputs::{
        InputDeleteFuture, InputListFuture, InputLookupFuture, InputUpdateFuture, InputWriteFuture,
        PipelineInputStore, PipelineInputStoreError,
    };
    use crate::application::ports::{
        MetaLookupError, MetaLookupFuture, MetaStore, PipelineListFuture, PipelineStore,
    };
    use crate::domain::pipeline_input::{PipelineInput, PipelineInputRole};
    use crate::domain::srt_ingest::ResolvedSrtIngestConfig;
    use crate::media::srt::serialize_pipeline_srt_ingest_policy;

    struct FakeMetaStore {
        value: Option<String>,
    }

    impl MetaStore for FakeMetaStore {
        fn get_meta<'a>(&'a self, key: &'a str) -> MetaLookupFuture<'a> {
            Box::pin(async move {
                if key != SRT_INGEST_GLOBAL_CONFIG_META_KEY {
                    return Err(MetaLookupError::new("unexpected key"));
                }
                Ok(self.value.clone())
            })
        }
    }

    struct FakePipelineStore {
        pipelines: Vec<Pipeline>,
    }

    struct FakePipelineInputStore {
        inputs: Vec<PipelineInput>,
    }

    impl PipelineInputStore for FakePipelineInputStore {
        fn get<'a>(&'a self, _pipeline_id: &'a str, _input_id: &'a str) -> InputLookupFuture<'a> {
            Box::pin(async { Ok(None) })
        }

        fn get_by_stream_key<'a>(&'a self, stream_key: &'a str) -> InputLookupFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .inputs
                    .iter()
                    .find(|input| input.stream_key == stream_key)
                    .cloned())
            })
        }

        fn list<'a>(&'a self, pipeline_id: &'a str) -> InputListFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .inputs
                    .iter()
                    .filter(|input| input.pipeline_id == pipeline_id)
                    .cloned()
                    .collect())
            })
        }

        fn create<'a>(
            &'a self,
            _id: &'a str,
            _pipeline_id: &'a str,
            _label: &'a str,
            _stream_key: &'a str,
        ) -> InputWriteFuture<'a> {
            Box::pin(async {
                Err(PipelineInputStoreError::Internal(
                    "not implemented".to_string(),
                ))
            })
        }

        fn update<'a>(
            &'a self,
            _pipeline_id: &'a str,
            _input_id: &'a str,
            _label: &'a str,
            _enabled: bool,
        ) -> InputUpdateFuture<'a> {
            Box::pin(async { Ok(None) })
        }

        fn delete<'a>(
            &'a self,
            _pipeline_id: &'a str,
            _input_id: &'a str,
        ) -> InputDeleteFuture<'a> {
            Box::pin(async { Ok(false) })
        }

        fn promote<'a>(
            &'a self,
            _pipeline_id: &'a str,
            _input_id: &'a str,
        ) -> InputUpdateFuture<'a> {
            Box::pin(async { Ok(None) })
        }
    }

    impl PipelineStore for FakePipelineStore {
        fn get_pipeline_by_stream_key<'a>(
            &'a self,
            stream_key: &'a str,
        ) -> crate::application::ports::PipelineLookupFuture<'a> {
            Box::pin(async move {
                Ok(self
                    .pipelines
                    .iter()
                    .find(|pipeline| pipeline.stream_key == stream_key)
                    .cloned())
            })
        }

        fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a> {
            Box::pin(async move { Ok(self.pipelines.clone()) })
        }

        fn get_pipeline<'a>(
            &'a self,
            id: &'a str,
        ) -> crate::application::ports::PipelineLookupFuture<'a> {
            Box::pin(async move { Ok(self.pipelines.iter().find(|p| p.id == id).cloned()) })
        }

        fn create_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> crate::application::ports::PipelineCreateFuture<'a> {
            Box::pin(async move {
                Err(crate::application::ports::PipelineStoreError::new(
                    "not implemented",
                ))
            })
        }

        fn update_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> crate::application::ports::PipelineUpdateFuture<'a> {
            Box::pin(async move {
                Err(crate::application::ports::PipelineStoreError::new(
                    "not implemented",
                ))
            })
        }

        fn delete_pipeline<'a>(
            &'a self,
            _id: &'a str,
        ) -> crate::application::ports::PipelineDeleteFuture<'a> {
            Box::pin(async move {
                Err(crate::application::ports::PipelineStoreError::new(
                    "not implemented",
                ))
            })
        }

        fn get_ingest_host<'a>(
            &'a self,
        ) -> crate::application::ports::PipelineIngestHostFuture<'a> {
            Box::pin(async move { Ok(None) })
        }

        fn update_pipeline_input_source<'a>(
            &'a self,
            pipeline: &'a Pipeline,
            input_source: Option<&'a str>,
        ) -> crate::application::ports::PipelineUpdateFuture<'a> {
            Box::pin(async move {
                let mut updated = pipeline.clone();
                updated.input_source = input_source.map(ToOwned::to_owned);
                Ok(Some(updated))
            })
        }
    }

    #[tokio::test]
    async fn global_srt_ingest_config_loads_from_meta_store() {
        let store = FakeMetaStore {
            value: Some(
                serde_json::json!({
                    "mode": "encrypted",
                    "passphrase": "secret-pass-123",
                    "pbkeylen": 24
                })
                .to_string(),
            ),
        };

        let config = load_global_srt_ingest_config(&store, None, 16).await;

        assert_eq!(config.mode, SrtGlobalIngestMode::Encrypted);
        assert_eq!(config.passphrase.as_deref(), Some("secret-pass-123"));
        assert_eq!(config.pbkeylen, 24);
    }

    #[tokio::test]
    async fn invalid_encrypted_global_srt_ingest_config_fails_closed() {
        let store = FakeMetaStore {
            value: Some(
                serde_json::json!({
                    "mode": "encrypted",
                    "passphrase": "short",
                    "pbkeylen": 99
                })
                .to_string(),
            ),
        };

        let config = load_global_srt_ingest_config(&store, None, 16).await;

        assert_eq!(config.mode, SrtGlobalIngestMode::Encrypted);
        assert!(config.resolve().is_err());
    }

    #[tokio::test]
    async fn invalid_plaintext_global_srt_ingest_config_falls_back_to_default() {
        let store = FakeMetaStore {
            value: Some(
                serde_json::json!({
                    "mode": "plaintext",
                    "pbkeylen": 99
                })
                .to_string(),
            ),
        };

        let config = load_global_srt_ingest_config(&store, None, 16).await;

        assert_eq!(config, SrtGlobalIngestConfig::default());
    }

    #[tokio::test]
    async fn load_policy_store_builds_store_from_meta_and_catalog() {
        let store = FakeMetaStore {
            value: Some(
                serde_json::json!({
                    "mode": "encrypted",
                    "passphrase": "global-pass-123",
                    "pbkeylen": 24
                })
                .to_string(),
            ),
        };
        let catalog = FakePipelineStore {
            pipelines: vec![Pipeline {
                id: "pipeline-1".to_string(),
                name: "Pipeline One".to_string(),
                stream_key: "stream-one".to_string(),
                input_source: None,
                srt_ingest_policy: Some(
                    serialize_pipeline_srt_ingest_policy(
                        &crate::domain::srt_ingest::SrtPipelineIngestConfig::default(),
                    )
                    .unwrap(),
                ),
            }],
        };

        let input_store = FakePipelineInputStore {
            inputs: vec![pipeline_input("pipeline-1", "stream-one")],
        };

        let policy_store = load_policy_store(&store, &catalog, &input_store, None, 16)
            .await
            .unwrap();

        assert_eq!(
            policy_store.global_config().mode,
            SrtGlobalIngestMode::Encrypted
        );
        assert_eq!(
            policy_store.resolved_policy("stream-one"),
            Some(ResolvedSrtIngestConfig::Encrypted {
                passphrase: "global-pass-123".to_string(),
                pbkeylen: 24,
            })
        );
    }

    #[tokio::test]
    async fn refresh_policy_store_replaces_existing_policies() {
        let initial_store = FakeMetaStore {
            value: Some(
                serde_json::json!({
                    "mode": "plaintext"
                })
                .to_string(),
            ),
        };
        let updated_store = FakeMetaStore {
            value: Some(
                serde_json::json!({
                    "mode": "encrypted",
                    "passphrase": "updated-pass-123",
                    "pbkeylen": 32
                })
                .to_string(),
            ),
        };
        let catalog = FakePipelineStore {
            pipelines: vec![Pipeline {
                id: "pipeline-1".to_string(),
                name: "Pipeline One".to_string(),
                stream_key: "stream-one".to_string(),
                input_source: None,
                srt_ingest_policy: Some(
                    serialize_pipeline_srt_ingest_policy(
                        &crate::domain::srt_ingest::SrtPipelineIngestConfig::default(),
                    )
                    .unwrap(),
                ),
            }],
        };
        let input_store = FakePipelineInputStore {
            inputs: vec![pipeline_input("pipeline-1", "stream-one")],
        };
        let policy_store = load_policy_store(&initial_store, &catalog, &input_store, None, 16)
            .await
            .unwrap();

        refresh_policy_store(
            &policy_store,
            &updated_store,
            &catalog,
            &input_store,
            None,
            16,
        )
        .await
        .unwrap();

        assert_eq!(
            policy_store.global_config().mode,
            SrtGlobalIngestMode::Encrypted
        );
        assert_eq!(
            policy_store.resolved_policy("stream-one"),
            Some(ResolvedSrtIngestConfig::Encrypted {
                passphrase: "updated-pass-123".to_string(),
                pbkeylen: 32,
            })
        );
    }

    #[test]
    fn srt_policy_entries_include_every_enabled_pipeline_input() {
        let pipeline = Pipeline {
            id: "pipeline-1".to_string(),
            name: "Pipeline One".to_string(),
            stream_key: "primary-key".to_string(),
            input_source: None,
            srt_ingest_policy: Some("inherited-policy".to_string()),
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
            },
            PipelineInput {
                id: "input-standby".to_string(),
                pipeline_id: pipeline.id.clone(),
                label: "Standby".to_string(),
                stream_key: "standby-key".to_string(),
                role: PipelineInputRole::Backup,
                enabled: true,
                selected: false,
            },
            PipelineInput {
                id: "input-disabled".to_string(),
                pipeline_id: pipeline.id.clone(),
                label: "Disabled".to_string(),
                stream_key: "disabled-key".to_string(),
                role: PipelineInputRole::Backup,
                enabled: false,
                selected: false,
            },
        ];

        let entries = srt_ingest_policy_entries(&[pipeline], &inputs);
        let stream_keys = entries
            .iter()
            .map(|entry| entry.stream_key.as_str())
            .collect::<Vec<_>>();

        assert_eq!(stream_keys, vec!["primary-key", "standby-key"]);
        assert!(
            entries
                .iter()
                .all(|entry| entry.serialized_policy.as_deref() == Some("inherited-policy"))
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
        let store = FakeMetaStore { value: None };

        let config =
            load_global_srt_ingest_config(&store, Some("app-pass-123".to_string()), 24).await;

        assert_eq!(config.mode, SrtGlobalIngestMode::Encrypted);
        assert_eq!(config.passphrase.as_deref(), Some("app-pass-123"));
        assert_eq!(config.pbkeylen, 24);
    }

    #[tokio::test]
    async fn global_srt_ingest_config_falls_back_to_default_when_no_meta_and_no_passphrase() {
        let store = FakeMetaStore { value: None };

        let config = load_global_srt_ingest_config(&store, None, 16).await;

        assert_eq!(config, SrtGlobalIngestConfig::default());
    }

    #[tokio::test]
    async fn global_srt_ingest_config_prefers_meta_store_over_appconfig_passphrase() {
        let store = FakeMetaStore {
            value: Some(
                serde_json::json!({
                    "mode": "plaintext"
                })
                .to_string(),
            ),
        };

        let config =
            load_global_srt_ingest_config(&store, Some("app-pass-123".to_string()), 24).await;

        assert_eq!(config.mode, SrtGlobalIngestMode::Plaintext);
    }
}
