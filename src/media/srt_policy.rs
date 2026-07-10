use std::collections::HashMap;
use std::sync::RwLock;

use tracing::warn;

use crate::domain::srt_ingest::{ResolvedSrtIngestConfig, SrtGlobalIngestConfig};
use crate::secret_display::redact_secret;
use crate::types::Pipeline;

use super::parse_pipeline_srt_ingest_policy;

pub struct SrtIngestPolicyStore {
    inner: RwLock<SrtIngestPolicySnapshot>,
}

#[derive(Clone)]
struct SrtIngestPolicySnapshot {
    global: SrtGlobalIngestConfig,
    per_stream_key: HashMap<String, ResolvedSrtIngestConfig>,
}

impl SrtIngestPolicyStore {
    pub fn new(global: SrtGlobalIngestConfig, pipelines: &[Pipeline]) -> Self {
        Self {
            inner: RwLock::new(build_policy_snapshot(global, pipelines)),
        }
    }

    pub fn replace(&self, global: SrtGlobalIngestConfig, pipelines: &[Pipeline]) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = build_policy_snapshot(global, pipelines);
    }

    pub fn global_config(&self) -> SrtGlobalIngestConfig {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .global
            .clone()
    }

    pub(crate) fn resolved_policy(&self, stream_key: &str) -> Option<ResolvedSrtIngestConfig> {
        self.inner
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .per_stream_key
            .get(stream_key)
            .cloned()
    }
}

fn build_policy_snapshot(
    global: SrtGlobalIngestConfig,
    pipelines: &[Pipeline],
) -> SrtIngestPolicySnapshot {
    let mut per_stream_key = HashMap::with_capacity(pipelines.len());
    for pipeline in pipelines {
        let pipeline_policy =
            parse_pipeline_srt_ingest_policy(pipeline.srt_ingest_policy.as_deref())
                .unwrap_or_default();
        match pipeline_policy.resolve(&global) {
            Ok(resolved) => {
                per_stream_key.insert(pipeline.stream_key.clone(), resolved);
            }
            Err(error) => {
                warn!(
                    pipeline_id = %pipeline.id,
                    stream_key = %redact_secret(&pipeline.stream_key),
                    err = %error,
                    "ignoring invalid persisted SRT ingest policy"
                );
                if let Ok(resolved) = global.resolve() {
                    per_stream_key.insert(pipeline.stream_key.clone(), resolved);
                }
            }
        }
    }
    SrtIngestPolicySnapshot {
        global,
        per_stream_key,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::srt_ingest::{SrtGlobalIngestMode, SrtPipelineIngestConfig};
    use crate::media::srt::serialize_pipeline_srt_ingest_policy;

    fn pipeline_with_policy(policy: Option<String>) -> Pipeline {
        Pipeline {
            id: "pipeline-1".to_string(),
            name: "Pipeline One".to_string(),
            stream_key: "stream-one".to_string(),
            input_source: None,
            srt_ingest_policy: policy,
        }
    }

    #[test]
    fn invalid_encrypted_global_policy_does_not_fall_back_to_plaintext() {
        let global = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Encrypted,
            passphrase: Some("short".to_string()),
            pbkeylen: 16,
        };
        let policy = serialize_pipeline_srt_ingest_policy(&SrtPipelineIngestConfig::default())
            .expect("serialize inherited policy");
        let store = SrtIngestPolicyStore::new(global, &[pipeline_with_policy(Some(policy))]);

        assert_eq!(store.resolved_policy("stream-one"), None);
    }
}
