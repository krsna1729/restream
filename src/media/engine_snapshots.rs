use std::sync::atomic::Ordering;

use crate::media::snapshots::{
    EgressDiagSnapshot, FileIngestDependencySnapshot, HlsDependencySnapshot, IngestDiagSnapshot,
    RingBufferDiagSnapshot, SrtListenerDiagSnapshot,
};

use super::engine::{MediaEngine, hls_preview_registry_key};

impl MediaEngine {
    pub async fn hls_dependency_snapshot(&self, pipeline_id: &str) -> HlsDependencySnapshot {
        let consumers = self.hls.consumers.read().await;
        let stores = self.hls.preview_stores.read().await;
        let preview_key = hls_preview_registry_key(pipeline_id);

        let consumer = consumers.get(&preview_key);
        let store = stores.get(&preview_key);

        HlsDependencySnapshot {
            store_exists: store.is_some(),
            active: consumer.is_some_and(|consumer| !consumer.cancel_token.is_cancelled()),
            persistent_consumers: consumer
                .map(|consumer| consumer.persistent.load(Ordering::Relaxed))
                .unwrap_or(0),
            last_access_age_ms: consumer.map(|consumer| {
                let now = consumer.reference_instant.elapsed().as_millis() as u64;
                let last = consumer.last_access_ms.load(Ordering::Relaxed);
                now.saturating_sub(last)
            }),
            segments: store.map(|store| store.segment_count()).unwrap_or(0),
            playlist_bytes: store.map(|store| store.primary_playlist_len()).unwrap_or(0),
        }
    }

    pub async fn file_ingest_dependency_snapshot(
        &self,
        ingest_id: &str,
    ) -> FileIngestDependencySnapshot {
        let active = self.file_ingests.active.read().await;
        let children = self.file_ingests.children.read().await;
        FileIngestDependencySnapshot {
            marked_active: active.contains(ingest_id),
            child_registered: children.contains_key(ingest_id),
        }
    }

    pub async fn active_ingest_count(&self) -> usize {
        self.ingests.active.read().await.len()
    }

    pub async fn active_ingest_protocol_for_probe(&self, pipeline_id: &str) -> Option<String> {
        self.ingests
            .active
            .read()
            .await
            .get(pipeline_id)
            .map(|ingest| ingest.protocol.clone())
    }

    pub async fn ingest_video_codec(&self, pipeline_id: &str) -> Option<String> {
        self.ingests
            .active
            .read()
            .await
            .get(pipeline_id)
            .and_then(|ingest| ingest.video.as_ref())
            .map(|video| video.codec.clone())
    }

    pub async fn active_ingest_diag_snapshot(
        &self,
        pipeline_id: &str,
    ) -> Option<IngestDiagSnapshot> {
        let ingests = self.ingests.active.read().await;
        let ingest = ingests.get(pipeline_id)?;
        let keyframe_times = ingest
            .keyframe_times
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        Some(IngestDiagSnapshot {
            protocol: ingest.protocol.clone(),
            uptime_secs: ingest.start_time.elapsed().as_secs_f64(),
            bytes_received: ingest.bytes_received.load(Ordering::Relaxed),
            remote_addr: ingest.remote_addr.clone(),
            video: ingest.video.clone(),
            audio: ingest.audio.clone(),
            quality: ingest.quality.clone(),
            keyframe_times,
        })
    }

    pub async fn active_egress_count(&self) -> usize {
        self.egresses.active.read().await.len()
    }

    pub async fn active_egress_diag_snapshots(&self, pipeline_id: &str) -> Vec<EgressDiagSnapshot> {
        let egresses = self.egresses.active.read().await;
        egresses
            .iter()
            .filter(|(_, egress)| egress.pipeline_id == pipeline_id)
            .map(|(output_id, egress)| EgressDiagSnapshot {
                output_id: output_id.clone(),
                pipeline_id: egress.pipeline_id.clone(),
                protocol: egress.protocol.clone(),
                status: egress.status.to_string(),
                phase: egress
                    .phase
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .to_string(),
                target_addr: egress
                    .target_addr
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
                bytes_sent: egress.bytes_sent.load(Ordering::Relaxed),
                last_progress_ms: egress.last_progress_ms.load(Ordering::Relaxed),
                last_error: egress
                    .last_error
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone(),
            })
            .collect()
    }

    pub async fn pipeline_ring_diag_snapshot(
        &self,
        pipeline_id: &str,
    ) -> Option<RingBufferDiagSnapshot> {
        let pipelines = self.ingests.pipelines.read().await;
        let ring = pipelines.get(pipeline_id)?;
        let (fill_slots, capacity_slots) = ring.fill_and_capacity();
        Some(RingBufferDiagSnapshot {
            fill_slots,
            capacity_slots,
            readers: ring.reader_snapshots(),
        })
    }

    pub async fn srt_listener_diag_snapshot(&self) -> SrtListenerDiagSnapshot {
        SrtListenerDiagSnapshot {
            bonding_available: self.bonding_available(),
            rx_queue_bytes: self
                .runtime
                .listener_stats
                .rx_queue_bytes
                .load(Ordering::Relaxed),
            rx_queue_peak_bytes: self
                .runtime
                .listener_stats
                .rx_queue_max_bytes
                .load(Ordering::Relaxed),
            drops: self.runtime.listener_stats.drops.load(Ordering::Relaxed),
            active_ingest_count: self.active_ingest_count().await,
        }
    }
}
