use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::ingest_auth::AuthenticatedPipeline;
use crate::media::input_gate::InputTimestampMapper;
use crate::media::mpegts::TsDemuxer;
use crate::media::srt::ingest_packets::forward_ingest_packets;
use crate::media::standby_gop::StandbyGopCache;

use super::super::SrtServer;

pub(super) struct RustIngestSession {
    pipeline_id: String,
    registration: IngestRegistration,
    ring_buffer: Arc<crate::media::ring_buffer::RingBuffer>,
    demuxer: TsDemuxer,
    timestamp_mapper: InputTimestampMapper,
    standby_gop: StandbyGopCache,
    packets: Vec<crate::media::packet::MediaPacket>,
    cached_keyframe_times: Option<Arc<Mutex<Vec<i64>>>>,
    bytes_received: Arc<AtomicU64>,
    ingest_metrics: Arc<crate::media::stage_metrics::StageMetrics>,
    last_progress_ms: Arc<AtomicU64>,
    probe_sent: bool,
}

impl RustIngestSession {
    pub(super) async fn create(
        server: &SrtServer,
        pipeline: AuthenticatedPipeline,
        stream_key: &str,
        peer: &str,
    ) -> Option<Self> {
        let ring_buffer = server.engine.get_or_create_pipeline(&pipeline.id).await;
        let registration = server
            .engine
            .try_register_pipeline_input_attempt(
                &pipeline.id,
                &pipeline.input_id,
                stream_key,
                "srt",
                pipeline.selected,
            )
            .await?;
        server
            .engine
            .update_ingest_session_meta(
                &pipeline.id,
                &registration,
                None,
                None,
                Some(peer.to_string()),
            )
            .await;
        let Some((bytes_received, ingest_metrics, last_progress_ms, cached_keyframe_times)) =
            server
                .engine
                .with_ingest_session(&registration, |ingest| {
                    (
                        ingest.bytes_received.clone(),
                        ingest.metrics.clone(),
                        ingest.last_progress_ms.clone(),
                        ingest.keyframe_times.clone(),
                    )
                })
                .await
        else {
            server
                .engine
                .unregister_ingest_if_current(&pipeline.id, &registration)
                .await;
            return None;
        };
        Some(Self {
            pipeline_id: pipeline.id,
            registration,
            ring_buffer,
            demuxer: TsDemuxer::new(),
            timestamp_mapper: InputTimestampMapper::default(),
            standby_gop: StandbyGopCache::default(),
            packets: Vec::with_capacity(16),
            cached_keyframe_times: Some(cached_keyframe_times),
            bytes_received,
            ingest_metrics,
            last_progress_ms,
            probe_sent: false,
        })
    }

    pub(super) async fn push(&mut self, engine: &MediaEngine, payload: &[u8]) {
        self.demuxer.feed(payload);
        if self.demuxer.drain_into(&mut self.packets) > 0 {
            forward_ingest_packets(
                &mut self.packets,
                &self.ring_buffer,
                &self.registration,
                &mut self.timestamp_mapper,
                &mut self.standby_gop,
                self.cached_keyframe_times.as_ref(),
            );
        }
        if !self.probe_sent
            && let Some(probe) = self.demuxer.take_probe()
        {
            self.probe_sent = true;
            let video_fps = probe.video.as_ref().map(|video| video.fps).unwrap_or(30.0);
            let audio_track_count = probe.audio_tracks.len();
            let first_audio = probe.audio_tracks.first().cloned();
            let selected_video_track_index = probe.video.as_ref().map(|_| 0);
            engine
                .update_ingest_session_meta(
                    &self.pipeline_id,
                    &self.registration,
                    probe.video,
                    first_audio,
                    None,
                )
                .await;
            engine
                .update_ingest_session_video_track_selection(
                    &self.registration,
                    probe.video_track_count,
                    selected_video_track_index,
                )
                .await;
            if !probe.audio_tracks.is_empty() {
                engine
                    .update_ingest_session_audio_tracks(
                        &self.pipeline_id,
                        &self.registration,
                        probe.audio_tracks,
                    )
                    .await;
            }
            if engine
                .is_ingest_session_selected(&self.pipeline_id, &self.registration)
                .await
                && let Some(ring) = engine
                    .adapt_pipeline_ring(&self.pipeline_id, video_fps, audio_track_count)
                    .await
            {
                self.ring_buffer = ring;
            }
        }
        self.bytes_received
            .fetch_add(payload.len() as u64, Ordering::Relaxed);
        self.ingest_metrics.record_in(payload.len() as u64);
        self.last_progress_ms
            .store(MediaEngine::now_epoch_ms(), Ordering::Relaxed);
    }

    pub(super) async fn finish(
        mut self,
        engine: &MediaEngine,
        phase: Option<&str>,
        reason: Option<String>,
        had_error: bool,
    ) {
        self.demuxer.flush();
        if self.demuxer.drain_into(&mut self.packets) > 0 {
            forward_ingest_packets(
                &mut self.packets,
                &self.ring_buffer,
                &self.registration,
                &mut self.timestamp_mapper,
                &mut self.standby_gop,
                None,
            );
        }
        engine
            .record_ingest_disconnect_if_current(
                &self.pipeline_id,
                &self.registration,
                phase,
                reason,
                had_error,
            )
            .await;
        engine
            .unregister_ingest_if_current(&self.pipeline_id, &self.registration)
            .await;
    }
}
