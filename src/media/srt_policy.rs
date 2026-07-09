use std::collections::HashMap;
use std::sync::RwLock;

use tracing::warn;

use crate::domain::srt_ingest::{ResolvedSrtIngestConfig, SrtGlobalIngestConfig};
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
                    stream_key = %pipeline.stream_key,
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
