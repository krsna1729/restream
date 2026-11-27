//! Pipeline ring and shared-stage lifecycle operations owned by `MediaEngine`.

use std::collections::HashSet;
use std::sync::Arc;

use crate::engine::MediaEngine;
use crate::stage_registry::StageLease;
use crate::stage_runtime::StageRuntimeManager;
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_sync::RingBuffer;

impl MediaEngine {
    pub async fn get_or_create_pipeline(&self, pipeline_id: &str) -> Arc<RingBuffer> {
        self.ingests.get_or_create_pipeline(pipeline_id).await
    }

    /// Called after stream probe: sizes the source ring for jitter headroom
    /// (`IngestRegistry::adapt_pipeline_ring`). Returns `Some(new_ring)` when
    /// resized so the ingest loop can update its local `ring_buffer` Arc.
    pub async fn adapt_pipeline_ring(
        &self,
        pipeline_id: &str,
        video_fps: f64,
        audio_track_count: usize,
    ) -> Option<Arc<RingBuffer>> {
        self.ingests
            .adapt_pipeline_ring(pipeline_id, video_fps, audio_track_count)
            .await
    }

    /// The stage runtime scheduler over this engine's ingest and stage
    /// registries, event log and external FFmpeg capacity.
    pub fn stage_runtime(&self) -> StageRuntimeManager {
        StageRuntimeManager::new(
            self.ingests.clone(),
            self.stages.clone(),
            self.runtime.event_log.clone(),
            self.runtime.external_ffmpeg_semaphore.clone(),
            self.config.external_ffmpeg_permits,
            self.config.transcoder_ring_capacity,
        )
    }

    /// Get or create a shared transcoder stage for a typed pipeline stage, and
    /// the caller's lease on it: the stage stops when its last lease drops.
    pub async fn get_or_create_transcoder(
        &self,
        pipeline_id: &str,
        stage_kind: StageKind,
        source_buffer: Arc<RingBuffer>,
        input_codec_override: Option<&str>,
    ) -> (Arc<RingBuffer>, StageLease) {
        let key = StageKey::new(pipeline_id, stage_kind.clone());
        let manager = self.stage_runtime();
        let (handle, lease, created) = manager
            .ensure_stage(key, source_buffer.clone(), input_codec_override)
            .await;

        if created {
            manager.spawn_stage(handle.clone(), source_buffer, input_codec_override);
        }

        (handle.ring, lease)
    }

    /// Get or create a shared H.265→H.264 transcoder stage for a pipeline, and
    /// the caller's lease on it.
    pub async fn get_or_create_h264_transcoder(
        &self,
        pipeline_id: &str,
        upstream: StageKind,
        source_buffer: Arc<RingBuffer>,
    ) -> (Arc<RingBuffer>, StageLease) {
        let key = StageKey::new(pipeline_id, StageKind::codec_edge("hevc_to_h264", upstream));
        let manager = self.stage_runtime();
        let (handle, lease, created) = manager.ensure_stage(key, source_buffer.clone(), None).await;

        if created {
            manager.spawn_codec_edge_stage(handle.clone(), source_buffer);
        }

        (handle.ring, lease)
    }

    /// Return the active processing stages for a pipeline as `(kind, is_alive)` pairs.
    pub async fn active_transcoder_stages(&self, pipeline_id: &str) -> Vec<(StageKind, bool)> {
        let runtimes = self.stages.runtimes.read().await;
        runtimes
            .iter()
            .filter(|(key, runtime)| key.pipeline.as_str() == pipeline_id && runtime.ring.is_some())
            .map(|(key, runtime)| (key.kind.clone(), !runtime.cancel.is_cancelled()))
            .collect()
    }

    /// Pipeline ids that hold runtime state: a source ring or a shared stage.
    /// Input-preview resource ids can key a ring too, but are not pipelines;
    /// a passthrough program preview's stage counts as its pipeline's.
    pub async fn runtime_pipeline_ids(&self) -> HashSet<String> {
        let mut ids = self
            .ingests
            .pipelines
            .read()
            .await
            .keys()
            .cloned()
            .collect::<HashSet<_>>();
        ids.extend(
            self.stages
                .runtimes
                .read()
                .await
                .keys()
                .map(|key| key.pipeline.as_str().to_string()),
        );
        ids.into_iter()
            .filter(|id| crate::preview_ids::input_id_from_hls_preview_resource_id(id).is_none())
            .map(|id| crate::preview_ids::preview_pipeline(&id).0.to_string())
            .collect()
    }

    /// Tears down the runtime of a deleted pipeline: its ingest sessions
    /// (cancelling a session also stops its file-ingest child), source ring,
    /// shared stages and HLS segmenters. Egresses go by output id.
    pub async fn teardown_pipeline_runtime(&self, pipeline_id: &str) {
        self.ingests.remove_pipeline(pipeline_id).await;
        self.unregister_ingest(pipeline_id).await;
        self.cleanup_pipeline_stages(pipeline_id).await;
        self.shutdown_hls_preview_segmenter(pipeline_id).await;
        self.shutdown_hls_preview_segmenter(&crate::preview_ids::hevc_passthrough_preview_id(
            pipeline_id,
        ))
        .await;
        self.shutdown_hls_segmenter(pipeline_id).await;
    }

    /// Eagerly remove every shared stage owned by a deleted pipeline.
    pub async fn cleanup_pipeline_stages(&self, pipeline_id: &str) {
        let removed = {
            let mut runtimes = self.stages.runtimes.write().await;
            let mut removed = Vec::new();
            runtimes.retain(|key, runtime| {
                if key.pipeline.as_str() == pipeline_id {
                    runtime.cancel.cancel();
                    removed.push(key.clone());
                    false
                } else {
                    true
                }
            });
            removed
        };
        self.remove_stage_artifacts(&removed).await;
    }

    async fn remove_stage_artifacts(&self, keys: &[StageKey]) {
        if keys.is_empty() {
            return;
        }
        {
            let mut metrics = self.stages.metrics.write().await;
            for key in keys {
                metrics.remove(key);
            }
        }
        let mut lifecycles = self.stages.lifecycles.write().await;
        for key in keys {
            lifecycles.remove(key);
        }
    }
}
