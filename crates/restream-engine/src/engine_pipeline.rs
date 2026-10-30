//! Pipeline ring and shared-stage lifecycle operations owned by `MediaEngine`.

use std::collections::HashSet;
use std::sync::Arc;

use tracing::info;

use crate::engine::MediaEngine;
use crate::stage_lease::StageLease;
use restream_domain::domain::stage::{StageKey, StageKind};
use restream_sync::RingBuffer;

impl MediaEngine {
    pub async fn get_or_create_pipeline(&self, pipeline_id: &str) -> Arc<RingBuffer> {
        let mut pipelines = self.ingests.pipelines.write().await;
        if let Some(rb) = pipelines.get(pipeline_id) {
            return rb.clone();
        }
        let rb = Arc::new(RingBuffer::new(self.config.ring_capacity));
        pipelines.insert(pipeline_id.to_string(), rb.clone());
        rb
    }

    /// Called after stream probe: sizes the source ring for 5 s jitter headroom.
    ///
    /// Formula: `needed = ceil(pkt_rate × HEADROOM_SECS)`, clamped to
    /// `[configured ring capacity, MAX_RING_CAPACITY]`. If the ring is already
    /// large enough no action is taken. Otherwise the ring is always swapped in,
    /// even if egress readers are already attached.
    ///
    /// Returns `Some(new_ring)` when resized so the ingest loop can update its
    /// local `ring_buffer` Arc.
    pub async fn adapt_pipeline_ring(
        &self,
        pipeline_id: &str,
        video_fps: f64,
        audio_track_count: usize,
    ) -> Option<Arc<RingBuffer>> {
        const AUDIO_PKT_RATE: f64 = 50.0;
        const MAX_RING_CAPACITY: usize = 16_384;
        let headroom_secs = self.config.ring_headroom_secs.max(0.1);

        let pkt_rate = video_fps.max(0.0) + audio_track_count as f64 * AUDIO_PKT_RATE;
        let needed = ((pkt_rate * headroom_secs).ceil() as usize)
            .max(self.config.ring_capacity)
            .min(MAX_RING_CAPACITY);

        let mut pipelines = self.ingests.pipelines.write().await;
        let old_rb = pipelines.get(pipeline_id).cloned()?;
        old_rb.set_estimated_pkt_rate(pkt_rate);

        if needed <= old_rb.capacity() {
            return None;
        }

        let old_write_idx = old_rb.get_write_idx();
        let new_rb = Arc::new(RingBuffer::new_continuing(needed, old_write_idx));
        let seeded_packets = new_rb.seed_readable_tail_from(&old_rb);
        new_rb.set_estimated_pkt_rate(pkt_rate);
        let hint = old_rb.codec_hint_str();
        if !hint.is_empty() {
            new_rb.set_codec_hint(hint);
        }
        if let Some(audio_tracks) = old_rb.audio_tracks() {
            new_rb.set_audio_tracks(audio_tracks.to_vec());
        }
        let new_rb_clone = new_rb.clone();

        pipelines.insert(pipeline_id.to_string(), new_rb.clone());
        drop(pipelines);
        old_rb.seal_and_forward(new_rb);

        info!(
            pipeline_id,
            pkt_rate = format!("{pkt_rate:.0}"),
            video_fps = format!("{video_fps:.0}"),
            audio_track_count,
            new_capacity = needed,
            seeded_packets,
            headroom_secs = format!("{:.1}", headroom_secs),
            "adaptive ring resize: readers migrate in-place, no egress reconnect"
        );

        Some(new_rb_clone)
    }

    /// Get or create a shared transcoder stage for a typed pipeline stage, and
    /// the caller's lease on it: the stage stops when its last lease drops.
    pub async fn get_or_create_transcoder(
        self: &Arc<Self>,
        pipeline_id: &str,
        stage_kind: StageKind,
        source_buffer: Arc<RingBuffer>,
        input_codec_override: Option<&str>,
    ) -> (Arc<RingBuffer>, StageLease) {
        let key = StageKey::new(pipeline_id, stage_kind.clone());
        let manager = crate::stage_runtime::StageRuntimeManager::new(self.clone());
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
        self: &Arc<Self>,
        pipeline_id: &str,
        upstream: StageKind,
        source_buffer: Arc<RingBuffer>,
    ) -> (Arc<RingBuffer>, StageLease) {
        let key = StageKey::new(pipeline_id, StageKind::codec_edge("hevc_to_h264", upstream));
        let manager = crate::stage_runtime::StageRuntimeManager::new(self.clone());
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

    pub async fn remove_pipeline(&self, pipeline_id: &str) {
        self.ingests.pipelines.write().await.remove(pipeline_id);
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
            .filter(|id| crate::engine_hls::input_id_from_hls_preview_resource_id(id).is_none())
            .map(|id| crate::engine_hls::preview_pipeline(&id).0.to_string())
            .collect()
    }

    /// Tears down the runtime of a deleted pipeline: its ingest sessions
    /// (cancelling a session also stops its file-ingest child), source ring,
    /// shared stages and HLS segmenters. Egresses go by output id.
    pub async fn teardown_pipeline_runtime(&self, pipeline_id: &str) {
        self.remove_pipeline(pipeline_id).await;
        self.unregister_ingest(pipeline_id).await;
        self.cleanup_pipeline_stages(pipeline_id).await;
        self.shutdown_hls_preview_segmenter(pipeline_id).await;
        self.shutdown_hls_preview_segmenter(&crate::engine_hls::hevc_passthrough_preview_id(
            pipeline_id,
        ))
        .await;
        self.shutdown_hls_segmenter(pipeline_id).await;
    }

    /// Eagerly remove every shared stage owned by a deleted pipeline.
    pub async fn cleanup_pipeline_stages(&self, pipeline_id: &str) {
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
        drop(runtimes);
        self.remove_stage_artifacts(&removed).await;
    }

    async fn remove_stage_artifacts(&self, keys: &[StageKey]) {
        if keys.is_empty() {
            return;
        }
        let mut metrics = self.stages.metrics.write().await;
        let mut lifecycles = self.stages.lifecycles.write().await;
        for key in keys {
            metrics.remove(key);
            lifecycles.remove(key);
        }
    }
}
