//! Pipeline ring and shared-stage lifecycle operations owned by `MediaEngine`.

use std::collections::HashSet;
use std::sync::Arc;

use tracing::{debug, info};

use crate::domain::stage::{StageKey, StageKind};
use crate::media::engine::MediaEngine;
use crate::media::ring_buffer::RingBuffer;

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
        if let Some(hint) = old_rb.codec_hint.get() {
            new_rb.set_codec_hint(hint);
        }
        if let Some(parameter_sets) = old_rb.video_parameter_sets() {
            new_rb.set_video_parameter_sets(parameter_sets);
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

    /// Get or create a shared transcoder stage for a typed pipeline stage.
    pub async fn get_or_create_transcoder(
        self: &Arc<Self>,
        pipeline_id: &str,
        stage_kind: StageKind,
        source_buffer: Arc<RingBuffer>,
        input_codec_override: Option<&str>,
    ) -> Arc<RingBuffer> {
        let key = StageKey::new(pipeline_id, stage_kind.clone());
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(self.clone());
        let (handle, created) = manager
            .ensure_stage(key.clone(), source_buffer.clone(), input_codec_override)
            .await;

        if created {
            manager.spawn_stage(handle.clone(), source_buffer.clone(), input_codec_override);
        }

        handle.ring
    }

    /// Get or create a shared H.265→H.264 transcoder stage for a pipeline.
    pub async fn get_or_create_h264_transcoder(
        self: &Arc<Self>,
        pipeline_id: &str,
        upstream: StageKind,
        source_buffer: Arc<RingBuffer>,
    ) -> Arc<RingBuffer> {
        let key = StageKey::new(pipeline_id, StageKind::codec_edge("hevc_to_h264", upstream));
        let manager = crate::media::stage_runtime::StageRuntimeManager::new(self.clone());
        let (handle, created) = manager
            .ensure_stage(key.clone(), source_buffer.clone(), None)
            .await;

        if created {
            manager.spawn_codec_edge_stage(handle.clone(), source_buffer.clone());
        }

        handle.ring
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

    pub async fn sweep_unused_transcoder_stages(&self, active_keys: &HashSet<StageKey>) {
        let mut runtimes = self.stages.runtimes.write().await;
        let mut removed = Vec::new();
        runtimes.retain(|key, runtime| {
            if runtime.ring.is_some() && !active_keys.contains(key) {
                debug!("Sweeping unused transcoder stage: {}", key);
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

    /// New stages are exempt from the unused-stage sweep for this long. A
    /// legacy consumer registers a `Reader` synchronously at creation, but a
    /// fabric consumer's liveness marker (`fabric.srt.active_outputs`) is
    /// only set later, from the async task that creates the fabric runtime
    /// — a reconcile tick can land in that gap and see neither signal.
    const UNUSED_STAGE_GRACE: std::time::Duration = std::time::Duration::from_secs(5);

    pub async fn sweep_unused_stages(&self) {
        // Liveness has two independent signals: legacy consumers register a
        // `Reader` on the ring (tracked in `stage.ring.readers`), while
        // fabric consumers read via `EgressFeed::read_from` and never
        // register one. A stage feeding only fabric outputs would otherwise
        // look permanently unused and get cancelled on the next reconcile
        // tick regardless of how recently it started.
        let active_fabric_feeds: std::collections::HashSet<String> = {
            let registry = self.fabric.srt.lock().await;
            registry
                .active_outputs
                .keys()
                .map(|feed_id| feed_id.as_str().to_string())
                .collect()
        };

        let mut stages = self.stages.ts_muxers.write().await;
        stages.retain(|key, stage| {
            if stage.created_at.elapsed() < Self::UNUSED_STAGE_GRACE {
                return true;
            }

            let has_readers = if let Ok(mut readers) = stage.ring.readers.lock() {
                readers.retain(|reader| reader.upgrade().is_some());
                !readers.is_empty()
            } else {
                false
            };
            let has_fabric_consumer = active_fabric_feeds.contains(&format!("srt:{key}"));

            if !has_readers && !has_fabric_consumer {
                debug!("Sweeping unused TS muxer stage: {}", key);
                stage.cancel.cancel();
                false
            } else {
                true
            }
        });
    }
}
