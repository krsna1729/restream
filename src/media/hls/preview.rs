use std::sync::Arc;

use crate::media::engine::MediaEngine;
use crate::media::hls::fmp4::Fmp4HlsStore;

impl MediaEngine {
    /// Ensure a browser-preview HLS runtime exists and spawn its segmenter if
    /// this call created the preview consumer. The runtime layer owns the store,
    /// cancellation token, graph planning, and segmenter task lifecycle.
    pub async fn ensure_hls_preview_runtime(
        self: &Arc<Self>,
        pipeline_id: &str,
    ) -> Arc<Fmp4HlsStore> {
        let (store, already_running, cancel_token) =
            self.ensure_hls_preview_segmenter(pipeline_id).await;
        if !already_running {
            let engine = self.clone();
            let pid = pipeline_id.to_string();
            let graph = match crate::media::hls::preview_graph::resolve_hls_preview_graph(
                self.clone(),
                pipeline_id,
                cancel_token.clone(),
            )
            .await
            {
                Some(graph) => graph,
                None => crate::media::hls::preview_graph::HlsPreviewGraph {
                    video_ring: self.get_or_create_pipeline(pipeline_id).await,
                    audio_ring: None,
                    video_meta: None,
                },
            };
            let store_for_task = store.clone();
            tokio::spawn(async move {
                crate::media::hls::fmp4::start_hls_fmp4_segmenter(
                    pid.clone(),
                    store_for_task,
                    graph.video_ring,
                    graph.audio_ring,
                    engine.clone(),
                    cancel_token,
                    crate::media::hls::HlsSegmenterStart {
                        video_meta_override: graph.video_meta,
                        planned_stage_key: None,
                    },
                )
                .await;
                engine.shutdown_hls_preview_segmenter(&pid).await;
            });
        }

        self.touch_hls_preview(pipeline_id).await;
        store
    }

    pub async fn ensure_input_hls_preview_runtime(
        self: &Arc<Self>,
        input_id: &str,
    ) -> Option<(String, Arc<Fmp4HlsStore>)> {
        let source_ring = self.ensure_input_preview_ring(input_id).await?;
        let resource_id = crate::media::engine_hls::input_hls_preview_resource_id(input_id);
        let (store, already_running, cancel_token) =
            self.ensure_hls_preview_segmenter(&resource_id).await;
        if !already_running {
            let graph = crate::media::hls::preview_graph::resolve_input_hls_preview_graph(
                self.clone(),
                &resource_id,
                input_id,
                source_ring,
                cancel_token.clone(),
            )
            .await;
            let engine = self.clone();
            let segmenter_id = resource_id.clone();
            let store_for_task = store.clone();
            tokio::spawn(async move {
                crate::media::hls::fmp4::start_hls_fmp4_segmenter(
                    segmenter_id.clone(),
                    store_for_task,
                    graph.video_ring,
                    graph.audio_ring,
                    engine.clone(),
                    cancel_token,
                    crate::media::hls::HlsSegmenterStart {
                        video_meta_override: graph.video_meta,
                        planned_stage_key: None,
                    },
                )
                .await;
                engine.shutdown_hls_preview_segmenter(&segmenter_id).await;
            });
        }
        self.touch_hls_preview(&resource_id).await;
        Some((resource_id, store))
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

    /// Regression test for a TOCTOU race: `ensure_hls_preview_runtime` used to
    /// insert a consumer entry, drop the lock, then re-acquire it via
    /// `get_hls_preview_cancel_token(...).await.unwrap()`. A concurrent
    /// `shutdown_hls_preview_segmenter` call could remove the entry in that
    /// window, turning the `unwrap()` into a panic. `ensure_hls_preview_segmenter`
    /// now hands back the token it just inserted directly, so there is no
    /// window for a concurrent shutdown to invalidate. Hammer ensure/shutdown
    /// concurrently and confirm every task completes without panicking.
    #[tokio::test]
    async fn ensure_hls_preview_runtime_survives_concurrent_shutdown_race() {
        let engine = Arc::new(MediaEngine::new());
        let pipeline_id = "pipe-hls-preview-race";
        engine
            .try_register_ingest(pipeline_id, "stream-key", "rtmp")
            .await
            .unwrap();
        let _ = engine.get_or_create_pipeline(pipeline_id).await;

        let mut tasks = Vec::new();
        for _ in 0..64 {
            let ensure_engine = engine.clone();
            let pid = pipeline_id.to_string();
            tasks.push(tokio::spawn(async move {
                let _ = ensure_engine.ensure_hls_preview_runtime(&pid).await;
            }));

            let shutdown_engine = engine.clone();
            let pid = pipeline_id.to_string();
            tasks.push(tokio::spawn(async move {
                shutdown_engine.shutdown_hls_preview_segmenter(&pid).await;
            }));
        }

        for task in tasks {
            task.await
                .expect("no task should panic under the ensure/shutdown race");
        }

        engine.shutdown_hls_preview_segmenter(pipeline_id).await;
    }
}
