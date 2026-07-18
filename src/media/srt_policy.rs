use std::collections::HashMap;
use std::sync::RwLock;

use tracing::warn;

use crate::domain::srt_ingest::{ResolvedSrtIngestConfig, SrtGlobalIngestConfig};
use crate::secret_display::redact_secret;

use super::parse_pipeline_srt_ingest_policy;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrtIngestPolicyEntry {
    pub pipeline_id: String,
    pub stream_key: String,
    pub serialized_policy: Option<String>,
}

impl SrtIngestPolicyEntry {
    pub fn new(
        pipeline_id: impl Into<String>,
        stream_key: impl Into<String>,
        serialized_policy: Option<String>,
    ) -> Self {
        Self {
            pipeline_id: pipeline_id.into(),
            stream_key: stream_key.into(),
            serialized_policy,
        }
    }
}

pub struct SrtIngestPolicyStore {
    inner: RwLock<SrtIngestPolicySnapshot>,
}

#[derive(Clone)]
struct SrtIngestPolicySnapshot {
    global: SrtGlobalIngestConfig,
    per_stream_key: HashMap<String, ResolvedSrtIngestConfig>,
}

impl SrtIngestPolicyStore {
    pub fn new(global: SrtGlobalIngestConfig, entries: &[SrtIngestPolicyEntry]) -> Self {
        Self {
            inner: RwLock::new(build_policy_snapshot(global, entries)),
        }
    }

    pub fn replace(&self, global: SrtGlobalIngestConfig, entries: &[SrtIngestPolicyEntry]) {
        let mut guard = self.inner.write().unwrap_or_else(|e| e.into_inner());
        *guard = build_policy_snapshot(global, entries);
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
    entries: &[SrtIngestPolicyEntry],
) -> SrtIngestPolicySnapshot {
    let mut per_stream_key = HashMap::with_capacity(entries.len());
    for entry in entries {
        let pipeline_policy = parse_pipeline_srt_ingest_policy(entry.serialized_policy.as_deref())
            .unwrap_or_default();
        match pipeline_policy.resolve(&global) {
            Ok(resolved) => {
                per_stream_key.insert(entry.stream_key.clone(), resolved);
            }
            Err(error) => {
                warn!(
                    pipeline_id = %entry.pipeline_id,
                    stream_key = %redact_secret(&entry.stream_key),
                    err = %error,
                    "ignoring invalid persisted SRT ingest policy"
                );
                if let Ok(resolved) = global.resolve() {
                    per_stream_key.insert(entry.stream_key.clone(), resolved);
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
    use crate::domain::srt_ingest::{
        SrtGlobalIngestMode, SrtPipelineIngestConfig, SrtPipelineIngestMode,
    };
    use crate::media::srt::serialize_pipeline_srt_ingest_policy;

    fn policy_entry(policy: Option<String>) -> SrtIngestPolicyEntry {
        SrtIngestPolicyEntry::new("pipeline-1", "stream-one", policy)
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
        let store = SrtIngestPolicyStore::new(global, &[policy_entry(Some(policy))]);

        assert_eq!(store.resolved_policy("stream-one"), None);
    }

    #[test]
    fn malformed_and_absent_serialized_policy_both_silently_inherit_global() {
        let global = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Encrypted,
            passphrase: Some("global-pass-123".to_string()),
            pbkeylen: 16,
        };
        let expected = global.resolve().expect("valid global resolves");

        let malformed_store = SrtIngestPolicyStore::new(
            global.clone(),
            &[policy_entry(Some("{ not json".to_string()))],
        );
        let absent_store = SrtIngestPolicyStore::new(global, &[policy_entry(None)]);

        // A corrupted persisted policy and a genuinely-absent policy are
        // indistinguishable: both fall back to `SrtPipelineIngestConfig::default()`
        // (mode = Inherit) with no warning logged, silently adopting whatever
        // the global policy currently is.
        assert_eq!(
            malformed_store.resolved_policy("stream-one"),
            Some(expected.clone())
        );
        assert_eq!(absent_store.resolved_policy("stream-one"), Some(expected));
    }

    #[test]
    fn invalid_per_entry_policy_falls_back_to_valid_global_policy() {
        let global = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Encrypted,
            passphrase: Some("global-pass-123".to_string()),
            pbkeylen: 16,
        };
        let expected = global.resolve().expect("valid global resolves");

        let invalid_pipeline_policy = SrtPipelineIngestConfig {
            mode: SrtPipelineIngestMode::Encrypted,
            passphrase: Some("short".to_string()),
            pbkeylen: Some(16),
        };
        let policy = serialize_pipeline_srt_ingest_policy(&invalid_pipeline_policy)
            .expect("serialize invalid pipeline policy");
        let store = SrtIngestPolicyStore::new(global, &[policy_entry(Some(policy))]);

        assert_eq!(store.resolved_policy("stream-one"), Some(expected));
    }

    #[test]
    fn duplicate_stream_key_entries_last_one_wins() {
        let global = SrtGlobalIngestConfig::default();
        let plaintext_policy =
            serialize_pipeline_srt_ingest_policy(&SrtPipelineIngestConfig::default())
                .expect("serialize plaintext policy");
        let encrypted_policy = serialize_pipeline_srt_ingest_policy(&SrtPipelineIngestConfig {
            mode: SrtPipelineIngestMode::Encrypted,
            passphrase: Some("pipeline-pass-123".to_string()),
            pbkeylen: Some(32),
        })
        .expect("serialize encrypted policy");

        let entries = [
            SrtIngestPolicyEntry::new("pipeline-1", "stream-one", Some(plaintext_policy)),
            SrtIngestPolicyEntry::new("pipeline-2", "stream-one", Some(encrypted_policy)),
        ];
        let store = SrtIngestPolicyStore::new(global, &entries);

        assert_eq!(
            store.resolved_policy("stream-one"),
            Some(ResolvedSrtIngestConfig::Encrypted {
                passphrase: "pipeline-pass-123".to_string(),
                pbkeylen: 32,
            })
        );
    }

    #[test]
    fn empty_entries_slice_produces_empty_snapshot_without_panicking() {
        let store = SrtIngestPolicyStore::new(SrtGlobalIngestConfig::default(), &[]);
        assert_eq!(store.resolved_policy("stream-one"), None);
    }

    #[test]
    fn replace_atomically_drops_stream_keys_missing_from_new_entries() {
        let global = SrtGlobalIngestConfig::default();
        let store = SrtIngestPolicyStore::new(global.clone(), &[policy_entry(None)]);
        assert_eq!(
            store.resolved_policy("stream-one"),
            Some(ResolvedSrtIngestConfig::Plaintext)
        );

        let other_entry = SrtIngestPolicyEntry::new("pipeline-2", "stream-two", None);
        store.replace(global, &[other_entry]);

        assert_eq!(store.resolved_policy("stream-one"), None);
        assert_eq!(
            store.resolved_policy("stream-two"),
            Some(ResolvedSrtIngestConfig::Plaintext)
        );
    }
}
