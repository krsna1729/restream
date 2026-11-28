use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::engine::MediaEngine;
use crate::engine_registries::{HlsConsumers, PersistentLease};
use crate::hls::{HlsSegmenterContext, HlsSegmenterStart, HlsStore};
use crate::hls_fmp4::Fmp4HlsStore;
use crate::preview_ids::{
    hls_preview_registry_key, input_id_from_hls_preview_resource_id,
    pipeline_id_from_hls_preview_registry_key, preview_pipeline, sibling_preview_id,
};
use restream_sync::RingBuffer;

impl MediaEngine {
    pub async fn ensure_hls_segmenter(&self, pipeline_id: &str) -> (Arc<HlsStore>, bool) {
        let already_running = {
            let mut consumers = self.hls.consumers.write().await;
            let already_running = consumers.contains_key(pipeline_id);
            if !already_running {
                let token = CancellationToken::new();
                consumers.insert(pipeline_id.to_string(), HlsConsumers::new(token));
            }
            already_running
        };

        let store = self.get_or_create_hls_store(pipeline_id).await;
        (store, already_running)
    }

    /// The MPEG-TS HLS segmenter task for `pipeline_id`: reads `ring_buffer`
    /// (and alternate audio from `audio_ring_buffer`) into `store`, under a
    /// stage runtime in this engine's stage registry, until `cancel_token`
    /// fires.
    pub fn start_hls_segmenter(
        &self,
        pipeline_id: String,
        store: Arc<HlsStore>,
        ring_buffer: Arc<RingBuffer>,
        audio_ring_buffer: Option<Arc<RingBuffer>>,
        cancel_token: CancellationToken,
        start: HlsSegmenterStart,
    ) -> impl Future<Output = ()> + Send + 'static {
        crate::hls::start_hls_segmenter(
            pipeline_id,
            store,
            ring_buffer,
            audio_ring_buffer,
            self.hls_segmenter_context(),
            cancel_token,
            start,
        )
    }

    /// The fMP4 preview segmenter task for `pipeline_id` (a pipeline or
    /// preview resource id): reads `ring_buffer` into `store` until
    /// `cancel_token` fires.
    pub fn start_hls_fmp4_segmenter(
        &self,
        pipeline_id: String,
        store: Arc<Fmp4HlsStore>,
        ring_buffer: Arc<RingBuffer>,
        cancel_token: CancellationToken,
        start: HlsSegmenterStart,
    ) -> impl Future<Output = ()> + Send + 'static {
        crate::hls_fmp4::start_hls_fmp4_segmenter(
            pipeline_id,
            store,
            ring_buffer,
            self.hls_segmenter_context(),
            cancel_token,
            start,
        )
    }

    fn hls_segmenter_context(&self) -> HlsSegmenterContext {
        HlsSegmenterContext::new(
            self.ingests.clone(),
            self.stages.clone(),
            self.runtime.event_log.clone(),
        )
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
        let (already_running, cancel_token) = {
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
            (already_running, cancel_token)
        };

        let store = self.get_or_create_hls_preview_store(pipeline_id).await;
        (store, already_running, cancel_token)
    }

    pub async fn touch_hls_preview(&self, pipeline_id: &str) {
        let preview_key = hls_preview_registry_key(pipeline_id);
        let consumers = self.hls.consumers.read().await;
        if let Some(c) = consumers.get(&preview_key) {
            c.touch();
        }
    }

    /// Register a persistent HLS consumer (an HLS egress output) of the
    /// pipeline's running segmenter; `None` when no segmenter is registered.
    pub async fn lease_hls_persistent_consumer(
        &self,
        pipeline_id: &str,
    ) -> Option<PersistentLease> {
        let consumers = self.hls.consumers.read().await;
        consumers
            .get(pipeline_id)
            .map(HlsConsumers::lease_persistent)
    }

    /// Shut down an idle HLS segmenter and clean up its store.
    pub async fn shutdown_hls_segmenter(&self, pipeline_id: &str) {
        if let Some(c) = self.hls.consumers.write().await.remove(pipeline_id) {
            c.cancel_token.cancel();
        }
        self.hls.stores.write().await.remove(pipeline_id);
    }

    pub async fn shutdown_hls_preview_segmenter(&self, pipeline_id: &str) {
        let preview_key = hls_preview_registry_key(pipeline_id);
        let sibling_running = {
            let mut consumers = self.hls.consumers.write().await;
            if let Some(c) = consumers.remove(&preview_key) {
                c.cancel_token.cancel();
            }
            // The input's ring stays while its other preview form still plays.
            consumers.contains_key(&hls_preview_registry_key(&sibling_preview_id(pipeline_id)))
        };
        self.hls.preview_stores.write().await.remove(&preview_key);
        if let Some(input_id) = input_id_from_hls_preview_resource_id(pipeline_id)
            && !sibling_running
        {
            self.ingests.release_input_preview_ring(input_id).await;
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
        let hls_config = crate::hls::HlsConfig::from_engine_config(&self.config);
        let mut stores = self.hls.stores.write().await;
        stores
            .entry(pipeline_id.to_string())
            .or_insert_with(|| Arc::new(HlsStore::with_config(hls_config)))
            .clone()
    }

    pub async fn get_or_create_hls_preview_store(&self, pipeline_id: &str) -> Arc<Fmp4HlsStore> {
        let preview_key = hls_preview_registry_key(pipeline_id);
        let hls_config = crate::hls::HlsConfig::from_engine_config(&self.config);
        let mut stores = self.hls.preview_stores.write().await;
        stores
            .entry(preview_key)
            .or_insert_with(|| Arc::new(Fmp4HlsStore::with_config(hls_config)))
            .clone()
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
        let has_ingest = self.ingests.has_active_ingest(pipeline_id).await;
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
            self.ingests
                .with_ingest_sessions(|sessions| sessions.contains_key(input_id))
                .await
        } else {
            // A passthrough preview id names its pipeline with a suffix.
            self.ingests
                .has_active_ingest(preview_pipeline(pipeline_id).0)
                .await
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
