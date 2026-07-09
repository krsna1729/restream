use std::sync::Arc;

use crate::media::engine::MediaEngine;
use crate::media::hls_fmp4::Fmp4HlsStore;

impl MediaEngine {
    /// Ensure a browser-preview HLS runtime exists and spawn its segmenter if
    /// this call created the preview consumer. The runtime layer owns the store,
    /// cancellation token, graph planning, and segmenter task lifecycle.
    pub async fn ensure_hls_preview_runtime(
        self: &Arc<Self>,
        pipeline_id: &str,
    ) -> Arc<Fmp4HlsStore> {
        let (store, already_running) = self.ensure_hls_preview_segmenter(pipeline_id).await;
        if !already_running {
            let engine = self.clone();
            let pid = pipeline_id.to_string();
            let cancel_token = self
                .get_hls_preview_cancel_token(pipeline_id)
                .await
                .unwrap();
            let graph = match crate::planner::hls_preview::plan_hls_preview(
                self.clone(),
                pipeline_id,
                cancel_token.clone(),
            )
            .await
            {
                Some(graph) => graph,
                None => crate::planner::hls_preview::HlsPreviewGraph {
                    video_ring: self.get_or_create_pipeline(pipeline_id).await,
                    audio_ring: None,
                    video_meta: None,
                },
            };
            let store_for_task = store.clone();
            tokio::spawn(async move {
                crate::media::hls_fmp4::start_hls_fmp4_segmenter(
                    pid.clone(),
                    store_for_task,
                    graph.video_ring,
                    graph.audio_ring,
                    engine.clone(),
                    cancel_token,
                    graph.video_meta,
                )
                .await;
                engine.shutdown_hls_preview_segmenter(&pid).await;
            });
        }

        self.touch_hls_preview(pipeline_id).await;
        store
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::media::engine::MediaEngine;

    #[tokio::test]
    async fn ensure_hls_preview_runtime_creates_and_reuses_runtime_store() {
        let engine = Arc::new(MediaEngine::new());
        let pipeline_id = "pipe-hls-preview-runtime";
        engine
            .try_register_ingest(pipeline_id, "stream-key", "rtmp")
            .await
            .unwrap();
        let _ = engine.get_or_create_pipeline(pipeline_id).await;

        let store = engine.ensure_hls_preview_runtime(pipeline_id).await;
        let reused = engine.ensure_hls_preview_runtime(pipeline_id).await;

        assert!(
            Arc::ptr_eq(&store, &reused),
            "preview runtime should reuse the existing fMP4 store"
        );
        assert!(engine.get_hls_preview_store(pipeline_id).await.is_some());
        assert!(
            engine
                .get_hls_preview_cancel_token(pipeline_id)
                .await
                .is_some()
        );

        engine.shutdown_hls_preview_segmenter(pipeline_id).await;
    }
}
