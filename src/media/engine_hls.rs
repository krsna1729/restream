use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use tokio_util::sync::CancellationToken;

use crate::media::engine::MediaEngine;
use crate::media::hls::HlsStore;
use crate::media::hls_fmp4::Fmp4HlsStore;

const HLS_PREVIEW_KEY_PREFIX: &str = "__preview__:";

pub(crate) fn hls_preview_registry_key(pipeline_id: &str) -> String {
    format!("{HLS_PREVIEW_KEY_PREFIX}{pipeline_id}")
}

fn pipeline_id_from_hls_preview_registry_key(key: &str) -> Option<&str> {
    key.strip_prefix(HLS_PREVIEW_KEY_PREFIX)
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
    pub async fn ensure_hls_preview_segmenter(
        &self,
        pipeline_id: &str,
    ) -> (Arc<Fmp4HlsStore>, bool) {
        let preview_key = hls_preview_registry_key(pipeline_id);
        let mut consumers = self.hls.consumers.write().await;
        let already_running = consumers.contains_key(&preview_key);
        if !already_running {
            let token = CancellationToken::new();
            consumers.insert(preview_key.clone(), HlsConsumers::new(token));
        }
        drop(consumers);

        let store = self.get_or_create_hls_preview_store(pipeline_id).await;
        (store, already_running)
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
        let has_ingest = self.has_active_ingest(pipeline_id).await;
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
