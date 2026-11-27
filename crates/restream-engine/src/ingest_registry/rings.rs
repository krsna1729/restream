//! Pipeline source rings and per-input preview rings: creation, the
//! post-probe resize, and removal.

use std::sync::Arc;

use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_sync::RingBuffer;
use tracing::info;

use super::IngestRegistry;

impl IngestRegistry {
    pub async fn get_or_create_pipeline(&self, pipeline_id: &str) -> Arc<RingBuffer> {
        let mut pipelines = self.pipelines.write().await;
        if let Some(rb) = pipelines.get(pipeline_id) {
            return rb.clone();
        }
        let rb = Arc::new(RingBuffer::new(self.ring_capacity));
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
        let headroom_secs = self.ring_headroom_secs.max(0.1);

        let pkt_rate = video_fps.max(0.0) + audio_track_count as f64 * AUDIO_PKT_RATE;
        let needed = ((pkt_rate * headroom_secs).ceil() as usize)
            .max(self.ring_capacity)
            .min(MAX_RING_CAPACITY);

        let mut pipelines = self.pipelines.write().await;
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

    pub async fn remove_pipeline(&self, pipeline_id: &str) {
        self.pipelines.write().await.remove(pipeline_id);
    }

    #[expect(
        clippy::await_holding_invalid_type,
        reason = "`preview_lock` serializes preview-ring creation; under it `sessions` and `preview_slots` are read one at a time"
    )]
    pub async fn ensure_input_preview_ring(&self, input_id: &str) -> Option<Arc<RingBuffer>> {
        let _preview = self.preview_lock.lock().await;
        let ingest = self.sessions.read().await.get(input_id).cloned()?;
        let slot = self.preview_slots.read().await.get(input_id).cloned()?;
        if let Some(ring) = slot.load_full() {
            return Some(ring);
        }

        let ring = Arc::new(RingBuffer::new(self.ring_capacity));
        let metadata = ingest.metadata();
        if let Some(video) = metadata.video {
            ring.set_codec_hint(&video.codec);
        }
        let audio_tracks = restream_sync::lock::lock(&ingest.audio_tracks).clone();
        if !audio_tracks.is_empty() {
            ring.set_audio_tracks(audio_tracks.as_ref().clone());
        }
        let video_header = restream_sync::lock::lock(&ingest.video_sequence_header).clone();
        let audio_header = restream_sync::lock::lock(&ingest.audio_sequence_header).clone();
        if let Some(payload) = video_header {
            ring.push(MediaPacket {
                media_type: MediaType::Video,
                track_index: 0,
                pts: 0,
                dts: 0,
                is_keyframe: false,
                format: PayloadFormat::Flv,
                payload,
            });
        }
        if let Some(payload) = audio_header {
            ring.push(MediaPacket {
                media_type: MediaType::Audio,
                track_index: 0,
                pts: 0,
                dts: 0,
                is_keyframe: false,
                format: PayloadFormat::Flv,
                payload,
            });
        }
        slot.store(Some(ring.clone()));
        Some(ring)
    }

    pub async fn release_input_preview_ring(&self, input_id: &str) {
        if let Some(slot) = self.preview_slots.read().await.get(input_id) {
            slot.store(None);
        }
    }
}
