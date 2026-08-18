use std::collections::HashMap;
use std::sync::RwLock;

use tracing::warn;

use crate::domain::srt_ingest::{
    ResolvedSrtIngestConfig, SrtGlobalIngestConfig, SrtPipelineIngestConfig,
};
use crate::secret_display::redact_secret;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SrtIngestPolicyEntry {
    pub pipeline_id: String,
    pub stream_key: String,
    pub policy: SrtPipelineIngestConfig,
}

impl SrtIngestPolicyEntry {
    pub fn new(
        pipeline_id: impl Into<String>,
        stream_key: impl Into<String>,
        policy: SrtPipelineIngestConfig,
    ) -> Self {
        Self {
            pipeline_id: pipeline_id.into(),
            stream_key: stream_key.into(),
            policy,
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
        match entry.policy.resolve(&global) {
            Ok(resolved) => {
                per_stream_key.insert(entry.stream_key.clone(), resolved);
            }
            Err(error) => {
                warn!(
                    pipeline_id = %entry.pipeline_id,
                    stream_key = %redact_secret(&entry.stream_key),
                    err = %error,
                    "ignoring invalid SRT ingest policy"
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
        DEFAULT_SRT_INGEST_LATENCY_MS, ResolvedSrtCrypto, SrtGlobalIngestMode,
        SrtPipelineIngestConfig, SrtPipelineIngestMode,
    };

    fn policy_entry(policy: SrtPipelineIngestConfig) -> SrtIngestPolicyEntry {
        SrtIngestPolicyEntry::new("pipeline-1", "stream-one", policy)
    }

    #[test]
    fn invalid_encrypted_global_policy_does_not_fall_back_to_plaintext() {
        let global = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Encrypted,
            passphrase: Some("short".to_string()),
            pbkeylen: 16,
            latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
        };
        let store =
            SrtIngestPolicyStore::new(global, &[policy_entry(SrtPipelineIngestConfig::default())]);

        assert_eq!(store.resolved_policy("stream-one"), None);
    }

    #[test]
    fn inherited_typed_policy_uses_global_policy() {
        let global = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Encrypted,
            passphrase: Some("global-pass-123".to_string()),
            pbkeylen: 16,
            latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
        };
        let expected = global.resolve().expect("valid global resolves");

        let store =
            SrtIngestPolicyStore::new(global, &[policy_entry(SrtPipelineIngestConfig::default())]);

        assert_eq!(store.resolved_policy("stream-one"), Some(expected));
    }

    #[test]
    fn invalid_per_entry_policy_falls_back_to_valid_global_policy() {
        let global = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Encrypted,
            passphrase: Some("global-pass-123".to_string()),
            pbkeylen: 16,
            latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
        };
        let expected = global.resolve().expect("valid global resolves");

        let invalid_pipeline_policy = SrtPipelineIngestConfig {
            mode: SrtPipelineIngestMode::Encrypted,
            passphrase: Some("short".to_string()),
            pbkeylen: Some(16),
            latency_ms: None,
        };
        let store = SrtIngestPolicyStore::new(global, &[policy_entry(invalid_pipeline_policy)]);

        assert_eq!(store.resolved_policy("stream-one"), Some(expected));
    }

    #[test]
    fn duplicate_stream_key_entries_last_one_wins() {
        let global = SrtGlobalIngestConfig::default();
        let encrypted_policy = SrtPipelineIngestConfig {
            mode: SrtPipelineIngestMode::Encrypted,
            passphrase: Some("pipeline-pass-123".to_string()),
            pbkeylen: Some(32),
            latency_ms: None,
        };

        let entries = [
            SrtIngestPolicyEntry::new(
                "pipeline-1",
                "stream-one",
                SrtPipelineIngestConfig::default(),
            ),
            SrtIngestPolicyEntry::new("pipeline-2", "stream-one", encrypted_policy),
        ];
        let store = SrtIngestPolicyStore::new(global, &entries);

        assert_eq!(
            store.resolved_policy("stream-one"),
            Some(ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Encrypted {
                    passphrase: "pipeline-pass-123".to_string(),
                    pbkeylen: 32,
                },
                latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
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
        let store = SrtIngestPolicyStore::new(
            global.clone(),
            &[policy_entry(SrtPipelineIngestConfig::default())],
        );
        assert_eq!(
            store.resolved_policy("stream-one"),
            Some(ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Plaintext,
                latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
            })
        );

        let other_entry = SrtIngestPolicyEntry::new(
            "pipeline-2",
            "stream-two",
            SrtPipelineIngestConfig::default(),
        );
        store.replace(global, &[other_entry]);

        assert_eq!(store.resolved_policy("stream-one"), None);
        assert_eq!(
            store.resolved_policy("stream-two"),
            Some(ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Plaintext,
                latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
            })
        );
    }

    #[test]
    fn global_config_returns_the_stored_global_policy() {
        let global = SrtGlobalIngestConfig {
            mode: SrtGlobalIngestMode::Encrypted,
            passphrase: Some("global-pass-123".to_string()),
            pbkeylen: 32,
            latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
        };
        let store = SrtIngestPolicyStore::new(global.clone(), &[]);
        assert_eq!(store.global_config(), global);
    }

    #[test]
    fn poisoned_lock_recovers_instead_of_panicking() {
        let _expected_panic_silencer = crate::media::test_support::silence_expected_panics();
        let store = SrtIngestPolicyStore::new(
            SrtGlobalIngestConfig::default(),
            &[policy_entry(SrtPipelineIngestConfig::default())],
        );

        // Simulate a panic on another writer while it holds the lock; the
        // guard's Drop marks the RwLock poisoned on unwind.
        let poison_result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _guard = store.inner.write().unwrap();
            panic!("simulated writer panic while holding the lock");
        }));
        assert!(poison_result.is_err());

        // Both reads and writes must recover via into_inner() rather than
        // propagating the poison as a panic.
        assert_eq!(
            store.resolved_policy("stream-one"),
            Some(ResolvedSrtIngestConfig {
                crypto: ResolvedSrtCrypto::Plaintext,
                latency_ms: DEFAULT_SRT_INGEST_LATENCY_MS,
            })
        );
        store.replace(SrtGlobalIngestConfig::default(), &[]);
        assert_eq!(store.resolved_policy("stream-one"), None);
    }
}
