//! Engine-facing operations owned by `AppState`.
//!
//! Handlers ask for telemetry, graphs, input lifecycle actions, and HLS
//! previews here; the shared `MediaEngine` handle stays private.

use std::sync::Arc;

use bytes::Bytes;
use restream_engine::engine::{ActiveIngest, MediaEngine};

use super::AppState;
use crate::application::hls_preview::{self, HlsPreviewReadError};

/// HLS preview operations over the application engine handle.
pub(crate) struct HlsPreview<'a> {
    engine: &'a Arc<MediaEngine>,
}

/// HLS preview limits the agent context reports.
pub(crate) struct HlsLimits {
    pub min_segment_secs: f64,
    pub segment_capacity_bytes: usize,
    pub max_segments: usize,
}

impl AppState {
    /// HLS preview access for the duration of one request.
    pub(crate) fn hls_preview(&self) -> HlsPreview<'_> {
        HlsPreview {
            engine: &self.engine,
        }
    }

    /// Recent engine events, newest first.
    pub(crate) fn recent_events(
        &self,
        limit: usize,
        pipeline_id: Option<&str>,
    ) -> Vec<restream_engine::events::Event> {
        self.engine.recent_events(limit, pipeline_id)
    }

    /// The engine telemetry view.
    pub(crate) async fn engine_telemetry(&self) -> serde_json::Value {
        crate::api::runtime_views::engine_telemetry(&self.engine).await
    }

    /// One pipeline's telemetry view.
    pub(crate) async fn pipeline_telemetry(&self, pipeline_id: &str) -> serde_json::Value {
        crate::api::runtime_views::pipeline_telemetry(&self.engine, pipeline_id).await
    }

    /// The measure snapshot with the persisted pipeline count.
    pub(crate) async fn measure_snapshot(
        &self,
        pipelines: usize,
    ) -> crate::api::runtime_views::MeasureSnapshot {
        crate::api::runtime_views::measure_snapshot(&self.engine, pipelines).await
    }

    /// One stage's telemetry row by display key.
    pub(crate) async fn stage_telemetry_by_display(
        &self,
        display: &str,
    ) -> Option<serde_json::Value> {
        crate::api::runtime_views::stage_telemetry_by_display(&self.engine, display).await
    }

    /// The runtime processing graph for one pipeline.
    pub(crate) async fn processing_graph(
        &self,
        pipeline_id: &str,
        outputs: &[restream_domain::models::Output],
    ) -> serde_json::Value {
        crate::api::runtime_views::processing_graph(&self.engine, pipeline_id, outputs).await
    }

    /// One output's runtime status view.
    pub(crate) async fn output_status(&self, output_id: &str) -> Option<serde_json::Value> {
        crate::api::runtime_views::output_status(&self.engine, output_id).await
    }

    /// The codec of the pipeline's live ingest video.
    pub(crate) async fn ingest_video_codec(&self, pipeline_id: &str) -> Option<String> {
        self.engine.ingest_video_codec(pipeline_id).await
    }

    /// The pipeline's HLS dependency state.
    pub(crate) async fn hls_dependency_snapshot(
        &self,
        pipeline_id: &str,
    ) -> restream_engine::snapshots::HlsDependencySnapshot {
        self.engine.hls_dependency_snapshot(pipeline_id).await
    }

    /// The file ingest's dependency state.
    pub(crate) async fn file_ingest_dependency_snapshot(
        &self,
        ingest_id: &str,
    ) -> restream_engine::snapshots::FileIngestDependencySnapshot {
        self.engine.file_ingest_dependency_snapshot(ingest_id).await
    }

    /// Whether the pipeline is recording.
    pub(crate) async fn is_recording_active(&self, pipeline_id: &str) -> bool {
        self.engine.is_recording_active(pipeline_id).await
    }

    /// Whether SRT bonding is available.
    pub(crate) fn bonding_available(&self) -> bool {
        self.engine.bonding_available()
    }

    /// Cancels one pipeline input.
    pub(crate) async fn cancel_pipeline_input(&self, input_id: &str) {
        self.engine.cancel_pipeline_input(input_id).await;
    }

    /// Whether the file ingest is running.
    pub(crate) async fn is_file_ingest_running(&self, ingest_id: &str) -> bool {
        self.engine.is_file_ingest_running(ingest_id).await
    }

    /// Whether stream checks are active for the pipeline.
    pub(crate) fn is_stream_qa_active(&self, pipeline_id: &str) -> bool {
        self.engine.is_stream_qa_active(pipeline_id)
    }

    /// The pipeline's stream-check findings.
    pub(crate) fn stream_qa_findings(
        &self,
        pipeline_id: &str,
    ) -> Option<restream_engine::stream_qa::StreamQa> {
        self.engine.stream_qa(pipeline_id)
    }

    /// Starts stream checks when the source is live.
    pub(crate) async fn start_stream_qa(&self, pipeline_id: &str) {
        if self.engine.has_active_ingest(pipeline_id).await {
            crate::application::stream_qa::spawn_stream_qa(
                self.engine.clone(),
                pipeline_id.to_string(),
            )
            .await;
        }
    }

    /// Stops stream checks for the pipeline.
    pub(crate) fn unregister_stream_qa(&self, pipeline_id: &str) {
        self.engine.unregister_stream_qa(pipeline_id);
    }

    /// One active ingest.
    pub(crate) async fn active_ingest(&self, pipeline_id: &str) -> Option<Arc<ActiveIngest>> {
        self.engine.active_ingest(pipeline_id).await
    }

    /// The live session registered for one input.
    pub(crate) async fn ingest_session(&self, input_id: &str) -> Option<Arc<ActiveIngest>> {
        self.engine.ingest_session(input_id).await
    }

    /// Whether the input has a live ingest session.
    pub(crate) async fn input_has_session(&self, pipeline_id: &str, input_id: &str) -> bool {
        self.engine.input_has_session(pipeline_id, input_id).await
    }

    /// The protocol reported by the pipeline's active ingest probe.
    pub(crate) async fn active_ingest_protocol_for_probe(
        &self,
        pipeline_id: &str,
    ) -> Option<String> {
        self.engine
            .active_ingest_protocol_for_probe(pipeline_id)
            .await
    }

    /// Egress shard statuses serialized for diagnostics.
    pub(crate) async fn egress_shard_statuses(
        &self,
        timeout: std::time::Duration,
    ) -> Vec<serde_json::Value> {
        self.engine
            .egress_fabric_shard_statuses(timeout)
            .await
            .into_iter()
            .map(|status| status.to_json())
            .collect()
    }

    /// Cloned engine handle for background tasks that need owned state.
    pub(crate) fn engine_handle(&self) -> Arc<MediaEngine> {
        self.engine.clone()
    }

    /// The diagnostics semaphore for one pipeline.
    pub(crate) async fn diag_semaphore(&self, pipeline_id: &str) -> Arc<tokio::sync::Semaphore> {
        self.engine.get_or_create_diag_semaphore(pipeline_id).await
    }

    /// HLS limits the agent context reports.
    pub(crate) fn hls_limits(&self) -> HlsLimits {
        HlsLimits {
            min_segment_secs: self.engine.config.hls_min_segment_ms,
            segment_capacity_bytes: self.engine.config.hls_segment_capacity_bytes,
            max_segments: self.engine.config.hls_max_segments,
        }
    }

    /// Current RTMP and SRT ingress owner counts.
    pub(crate) fn ingress_owner_counts(&self) -> (usize, usize) {
        self.engine.ingress_owner_counts()
    }
}

impl HlsPreview<'_> {
    pub(crate) async fn primary_playlist(
        &self,
        pipeline_id: &str,
    ) -> Result<String, HlsPreviewReadError> {
        hls_preview::primary_playlist(self.engine.clone(), pipeline_id).await
    }

    pub(crate) async fn master_playlist(
        &self,
        pipeline_id: &str,
    ) -> Result<String, HlsPreviewReadError> {
        hls_preview::master_playlist(self.engine.clone(), pipeline_id).await
    }

    pub(crate) async fn video_playlist(
        &self,
        pipeline_id: &str,
    ) -> Result<String, HlsPreviewReadError> {
        hls_preview::video_playlist(self.engine.clone(), pipeline_id).await
    }

    pub(crate) async fn audio_playlist(
        &self,
        pipeline_id: &str,
        track_index: u32,
    ) -> Result<String, HlsPreviewReadError> {
        hls_preview::audio_playlist(self.engine.clone(), pipeline_id, track_index).await
    }

    pub(crate) async fn video_segment(
        &self,
        pipeline_id: &str,
        segment: &str,
    ) -> Result<Bytes, HlsPreviewReadError> {
        hls_preview::video_segment(self.engine.clone(), pipeline_id, segment).await
    }

    pub(crate) async fn video_init_segment(
        &self,
        pipeline_id: &str,
    ) -> Result<Bytes, HlsPreviewReadError> {
        hls_preview::video_init_segment(self.engine.clone(), pipeline_id).await
    }

    pub(crate) async fn audio_segment(
        &self,
        pipeline_id: &str,
        track_index: u32,
        segment: &str,
    ) -> Result<Bytes, HlsPreviewReadError> {
        hls_preview::audio_segment(self.engine.clone(), pipeline_id, track_index, segment).await
    }

    pub(crate) async fn audio_init_segment(
        &self,
        pipeline_id: &str,
        track_index: u32,
    ) -> Result<Bytes, HlsPreviewReadError> {
        hls_preview::audio_init_segment(self.engine.clone(), pipeline_id, track_index).await
    }

    pub(crate) async fn input_master_playlist(
        &self,
        input_id: &str,
    ) -> Result<String, HlsPreviewReadError> {
        hls_preview::input_master_playlist(self.engine.clone(), input_id).await
    }

    pub(crate) async fn input_video_playlist(
        &self,
        input_id: &str,
    ) -> Result<String, HlsPreviewReadError> {
        hls_preview::input_video_playlist(self.engine.clone(), input_id).await
    }

    pub(crate) async fn input_video_init_segment(
        &self,
        input_id: &str,
    ) -> Result<Bytes, HlsPreviewReadError> {
        hls_preview::input_video_init_segment(self.engine.clone(), input_id).await
    }

    pub(crate) async fn input_video_segment(
        &self,
        input_id: &str,
        segment: &str,
    ) -> Result<Bytes, HlsPreviewReadError> {
        hls_preview::input_video_segment(self.engine.clone(), input_id, segment).await
    }

    pub(crate) async fn input_audio_playlist(
        &self,
        input_id: &str,
        track_index: u32,
    ) -> Result<String, HlsPreviewReadError> {
        hls_preview::input_audio_playlist(self.engine.clone(), input_id, track_index).await
    }

    pub(crate) async fn input_audio_init_segment(
        &self,
        input_id: &str,
        track_index: u32,
    ) -> Result<Bytes, HlsPreviewReadError> {
        hls_preview::input_audio_init_segment(self.engine.clone(), input_id, track_index).await
    }

    pub(crate) async fn input_audio_segment(
        &self,
        input_id: &str,
        track_index: u32,
        segment: &str,
    ) -> Result<Bytes, HlsPreviewReadError> {
        hls_preview::input_audio_segment(self.engine.clone(), input_id, track_index, segment).await
    }
}
