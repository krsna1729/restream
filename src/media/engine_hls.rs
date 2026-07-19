use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::media::engine::MediaEngine;
use crate::media::hls::HlsStore;
use crate::media::hls_fmp4::Fmp4HlsStore;

const HLS_PREVIEW_KEY_PREFIX: &str = "__preview__:";
const INPUT_PREVIEW_RESOURCE_PREFIX: &str = "__input__:";

pub(crate) fn hls_preview_registry_key(pipeline_id: &str) -> String {
    format!("{HLS_PREVIEW_KEY_PREFIX}{pipeline_id}")
}

fn pipeline_id_from_hls_preview_registry_key(key: &str) -> Option<&str> {
    key.strip_prefix(HLS_PREVIEW_KEY_PREFIX)
}

pub(crate) fn input_hls_preview_resource_id(input_id: &str) -> String {
    format!("{INPUT_PREVIEW_RESOURCE_PREFIX}{input_id}")
}

pub(crate) fn input_id_from_hls_preview_resource_id(resource_id: &str) -> Option<&str> {
    resource_id.strip_prefix(INPUT_PREVIEW_RESOURCE_PREFIX)
}

/// Tracks HLS consumers for a pipeline. Persistent consumers (egress outputs)
/// register/unregister explicitly. Transient consumers (browser preview) keep
/// the segmenter alive via playlist fetch heartbeats.
pub struct HlsConsumers {
    /// Number of persistent consumers (HLS egress outputs).
    pub persistent: AtomicU64,
    /// Monotonic reference time.
    pub reference_instant: Instant,
    /// Monotonic elapsed millis since reference_instant for the last access.
    pub last_access_ms: AtomicU64,
    /// Cancel token for the segmenter task.
    pub cancel_token: CancellationToken,
}

impl HlsConsumers {
    pub fn new(cancel_token: CancellationToken) -> Self {
        Self {
            persistent: AtomicU64::new(0),
            reference_instant: Instant::now(),
            last_access_ms: AtomicU64::new(0),
            cancel_token,
        }
    }

    fn now_ms(&self) -> u64 {
        self.reference_instant.elapsed().as_millis() as u64
    }

    pub fn touch(&self) {
        self.last_access_ms.store(self.now_ms(), Ordering::Relaxed);
    }

    pub fn add_persistent(&self) {
        self.persistent.fetch_add(1, Ordering::Relaxed);
    }

    pub fn remove_persistent(&self) {
        self.persistent.fetch_sub(1, Ordering::Relaxed);
    }

    pub fn is_idle(&self, timeout_ms: u64) -> bool {
        let persistent = self.persistent.load(Ordering::Relaxed);
        if persistent > 0 {
            return false;
        }
        let last = self.last_access_ms.load(Ordering::Relaxed);
        let now = self.now_ms();
        now.saturating_sub(last) >= timeout_ms
    }
}

impl MediaEngine {
    pub(crate) async fn get_input_sequence_headers(
        &self,
        input_id: &str,
    ) -> (Option<bytes::Bytes>, Option<bytes::Bytes>) {
        let ingest = self.ingests.sessions.read().await.get(input_id).cloned();
        let Some(ingest) = ingest else {
            return (None, None);
        };
        let video = ingest
            .video_sequence_header
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let audio = ingest
            .audio_sequence_header
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        (video, audio)
    }

    pub async fn ensure_hls_segmenter(&self, pipeline_id: &str) -> (Arc<HlsStore>, bool) {
        let mut consumers = self.hls.consumers.write().await;
        let already_running = consumers.contains_key(pipeline_id);
        if !already_running {
            let token = CancellationToken::new();
            consumers.insert(pipeline_id.to_string(), HlsConsumers::new(token));
        }
        drop(consumers);

        let store = self.get_or_create_hls_store(pipeline_id).await;
        (store, already_running)
    }

    /// Ensure a browser-preview HLS segmenter is running for this pipeline.
    /// Preview segmenters are isolated from HLS egress so preview-only H.264
    /// conversion does not change upload/output behavior.
    ///
    /// Returns the cancel token for the (newly created or pre-existing)
    /// consumer entry directly, rather than requiring a caller to re-acquire
    /// the consumers lock afterward. A separate re-read would race a
    /// concurrent `shutdown_hls_preview_segmenter` call, which can remove the
    /// entry between the insert and the re-read.
    pub async fn ensure_hls_preview_segmenter(
        &self,
        pipeline_id: &str,
    ) -> (Arc<Fmp4HlsStore>, bool, CancellationToken) {
        let preview_key = hls_preview_registry_key(pipeline_id);
        let mut consumers = self.hls.consumers.write().await;
        let already_running = consumers.contains_key(&preview_key);
        let cancel_token = if already_running {
            consumers
                .get(&preview_key)
                .map(|c| c.cancel_token.clone())
                .unwrap_or_else(CancellationToken::new)
        } else {
            let token = CancellationToken::new();
            consumers.insert(preview_key.clone(), HlsConsumers::new(token.clone()));
            token
        };
        drop(consumers);

        let store = self.get_or_create_hls_preview_store(pipeline_id).await;
        (store, already_running, cancel_token)
    }

    /// Touch the HLS consumer heartbeat (called on playlist/segment fetch).
    pub async fn touch_hls(&self, pipeline_id: &str) {
        let consumers = self.hls.consumers.read().await;
        if let Some(c) = consumers.get(pipeline_id) {
            c.touch();
        }
    }

    pub async fn touch_hls_preview(&self, pipeline_id: &str) {
        let preview_key = hls_preview_registry_key(pipeline_id);
        let consumers = self.hls.consumers.read().await;
        if let Some(c) = consumers.get(&preview_key) {
            c.touch();
        }
    }

    /// Register a persistent HLS consumer (e.g. HLS egress output).
    pub async fn add_hls_persistent_consumer(&self, pipeline_id: &str) {
        let consumers = self.hls.consumers.read().await;
        if let Some(c) = consumers.get(pipeline_id) {
            c.add_persistent();
        }
    }

    /// Unregister a persistent HLS consumer.
    pub async fn remove_hls_persistent_consumer(&self, pipeline_id: &str) {
        let consumers = self.hls.consumers.read().await;
        if let Some(c) = consumers.get(pipeline_id) {
            c.remove_persistent();
        }
    }

    /// Shut down an idle HLS segmenter and clean up its store.
    pub async fn shutdown_hls_segmenter(&self, pipeline_id: &str) {
        let mut consumers = self.hls.consumers.write().await;
        if let Some(c) = consumers.remove(pipeline_id) {
            c.cancel_token.cancel();
        }
        drop(consumers);
        self.hls.stores.write().await.remove(pipeline_id);
    }

    pub async fn shutdown_hls_preview_segmenter(&self, pipeline_id: &str) {
        let preview_key = hls_preview_registry_key(pipeline_id);
        let mut consumers = self.hls.consumers.write().await;
        if let Some(c) = consumers.remove(&preview_key) {
            c.cancel_token.cancel();
        }
        drop(consumers);
        self.hls.preview_stores.write().await.remove(&preview_key);
        if let Some(input_id) = input_id_from_hls_preview_resource_id(pipeline_id) {
            self.release_input_preview_ring(input_id).await;
        }
    }

    /// Get the cancel token for a running HLS segmenter (used to spawn the task).
    pub async fn get_hls_cancel_token(&self, pipeline_id: &str) -> Option<CancellationToken> {
        let consumers = self.hls.consumers.read().await;
        consumers.get(pipeline_id).map(|c| c.cancel_token.clone())
    }

    pub async fn get_hls_preview_cancel_token(
        &self,
        pipeline_id: &str,
    ) -> Option<CancellationToken> {
        let preview_key = hls_preview_registry_key(pipeline_id);
        let consumers = self.hls.consumers.read().await;
        consumers.get(&preview_key).map(|c| c.cancel_token.clone())
    }

    pub async fn get_or_create_hls_store(&self, pipeline_id: &str) -> Arc<HlsStore> {
        let hls_config = crate::media::hls::HlsConfig::from_app_config(&self.config);
        let mut stores = self.hls.stores.write().await;
        stores
            .entry(pipeline_id.to_string())
            .or_insert_with(|| Arc::new(HlsStore::with_config(hls_config)))
            .clone()
    }

    pub async fn get_or_create_hls_preview_store(&self, pipeline_id: &str) -> Arc<Fmp4HlsStore> {
        let preview_key = hls_preview_registry_key(pipeline_id);
        let hls_config = crate::media::hls::HlsConfig::from_app_config(&self.config);
        let mut stores = self.hls.preview_stores.write().await;
        stores
            .entry(preview_key)
            .or_insert_with(|| Arc::new(Fmp4HlsStore::with_config(hls_config)))
            .clone()
    }

    pub async fn remove_hls_store(&self, pipeline_id: &str) {
        let mut stores = self.hls.stores.write().await;
        stores.remove(pipeline_id);
    }

    pub async fn get_hls_store(&self, pipeline_id: &str) -> Option<Arc<HlsStore>> {
        let stores = self.hls.stores.read().await;
        stores.get(pipeline_id).cloned()
    }

    pub async fn get_hls_preview_store(&self, pipeline_id: &str) -> Option<Arc<Fmp4HlsStore>> {
        let preview_key = hls_preview_registry_key(pipeline_id);
        let stores = self.hls.preview_stores.read().await;
        stores.get(&preview_key).cloned()
    }
    pub async fn hls_pipeline_ids(&self) -> Vec<String> {
        self.hls
            .consumers
            .read()
            .await
            .keys()
            .filter(|key| pipeline_id_from_hls_preview_registry_key(key).is_none())
            .cloned()
            .collect()
    }

    pub async fn hls_preview_pipeline_ids(&self) -> Vec<String> {
        self.hls
            .consumers
            .read()
            .await
            .keys()
            .filter_map(|key| pipeline_id_from_hls_preview_registry_key(key).map(str::to_string))
            .collect()
    }

    pub async fn should_shutdown_hls_segmenter(&self, pipeline_id: &str, timeout_ms: u64) -> bool {
        let has_ingest = self.has_active_ingest(pipeline_id).await;
        let consumers = self.hls.consumers.read().await;
        match consumers.get(pipeline_id) {
            Some(consumer) => !has_ingest || consumer.is_idle(timeout_ms),
            None => false,
        }
    }

    pub async fn should_shutdown_hls_preview_segmenter(
        &self,
        pipeline_id: &str,
        timeout_ms: u64,
    ) -> bool {
        let has_ingest = if let Some(input_id) = input_id_from_hls_preview_resource_id(pipeline_id)
        {
            self.ingests.sessions.read().await.contains_key(input_id)
        } else {
            self.has_active_ingest(pipeline_id).await
        };
        let preview_key = hls_preview_registry_key(pipeline_id);
        let consumers = self.hls.consumers.read().await;
        match consumers.get(&preview_key) {
            Some(consumer) => !has_ingest || consumer.is_idle(timeout_ms),
            None => false,
        }
    }

    pub async fn shutdown_all_hls_segmenters(&self) {
        let pipeline_ids = self.hls_pipeline_ids().await;
        for pipeline_id in pipeline_ids {
            self.shutdown_hls_segmenter(&pipeline_id).await;
        }
        let preview_ids = self.hls_preview_pipeline_ids().await;
        for pipeline_id in preview_ids {
            self.shutdown_hls_preview_segmenter(&pipeline_id).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hls_preview_registry_key_roundtrips_through_extraction() {
        let key = hls_preview_registry_key("abc");
        assert_eq!(key, "__preview__:abc");
        assert_eq!(pipeline_id_from_hls_preview_registry_key(&key), Some("abc"));
    }

    #[test]
    fn hls_preview_registry_key_roundtrips_for_empty_pipeline_id() {
        let key = hls_preview_registry_key("");
        assert_eq!(pipeline_id_from_hls_preview_registry_key(&key), Some(""));
    }

    #[test]
    fn pipeline_id_from_hls_preview_registry_key_rejects_unprefixed_key() {
        assert_eq!(pipeline_id_from_hls_preview_registry_key("abc"), None);
        // The prefix must anchor at the start, not just appear somewhere.
        assert_eq!(
            pipeline_id_from_hls_preview_registry_key("notpreview__preview__:abc"),
            None
        );
    }

    // A pipeline id that itself contains the preview prefix as a literal
    // substring only has the outer, registry-added prefix stripped: the
    // extraction is not recursive, so the caller-supplied id survives intact.
    #[test]
    fn hls_preview_registry_key_roundtrips_when_pipeline_id_contains_prefix() {
        let pipeline_id = "__preview__:foo";
        let key = hls_preview_registry_key(pipeline_id);
        assert_eq!(key, "__preview__:__preview__:foo");
        assert_eq!(
            pipeline_id_from_hls_preview_registry_key(&key),
            Some(pipeline_id)
        );
    }

    #[test]
    fn is_idle_treats_never_touched_consumer_as_idle_at_zero_timeout() {
        // last_access_ms starts at 0; a fresh consumer with no persistent
        // outputs and a zero grace timeout must be considered idle
        // immediately, with no implicit startup grace period.
        let hc = HlsConsumers::new(CancellationToken::new());
        assert!(hc.is_idle(0));
    }

    #[test]
    fn is_idle_ignores_elapsed_time_while_persistent_consumers_exist() {
        // A persistent (egress) consumer must veto idle shutdown outright,
        // even though the heartbeat was never touched and the timeout is 0.
        let hc = HlsConsumers::new(CancellationToken::new());
        hc.add_persistent();
        assert!(!hc.is_idle(0));
    }

    // `is_idle` compares `now_ms()` against `last_access_ms` with
    // `saturating_sub`, not plain subtraction. If `last_access_ms` is ever
    // ahead of `now_ms()` (state corruption, or clock skew across a
    // hypothetical restart of `reference_instant`), plain subtraction on
    // `u64` would panic in debug builds and wrap to a huge value in release,
    // both of which would misreport a live consumer as idle or crash the
    // engine. The guard must instead treat "reads as behind" as "not idle".
    #[test]
    fn is_idle_does_not_underflow_when_last_access_is_ahead_of_now() {
        let hc = HlsConsumers::new(CancellationToken::new());
        hc.last_access_ms.store(u64::MAX, Ordering::Relaxed);
        assert!(!hc.is_idle(1000));
    }

    // `remove_persistent` has no guard against being called without a
    // matching `add_persistent`: the counter is a bare `fetch_sub`, so it
    // wraps to `u64::MAX` instead of saturating at 0. Because `is_idle`
    // treats any nonzero `persistent` count as "never idle", a single
    // mismatched remove call permanently pins the consumer as non-idle and
    // leaks its segmenter/store. This test pins the current wrap-not-panic
    // behavior so a future caller mismatch is visible as a stuck-non-idle
    // regression rather than a silent resource leak.
    #[test]
    fn remove_persistent_without_add_wraps_and_permanently_blocks_idle_shutdown() {
        let hc = HlsConsumers::new(CancellationToken::new());
        hc.remove_persistent();
        assert_eq!(hc.persistent.load(Ordering::Relaxed), u64::MAX);
        assert!(!hc.is_idle(0));
    }
}
