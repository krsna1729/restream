//! `MediaEngine` forwards to the ingest registry's progress, metadata, track,
//! quality and sequence-header operations (`ingest_registry/metadata.rs`),
//! for the ingest servers and the application.

use std::sync::Arc;

use crate::engine::MediaEngine;
use crate::ingest_registry::{ActiveIngest, IngestRegistration};
use crate::snapshots::PublisherQuality;
use restream_core::metadata::{AudioMeta, DeclaredEncoder, VideoMeta};

impl MediaEngine {
    pub async fn update_ingest_bytes(&self, pipeline_id: &str, bytes: u64) {
        self.ingests.update_ingest_bytes(pipeline_id, bytes).await;
    }

    pub async fn record_keyframe(&self, pipeline_id: &str, pts: i64) {
        self.ingests.record_keyframe(pipeline_id, pts).await;
    }

    pub async fn update_ingest_meta(
        &self,
        pipeline_id: &str,
        video: Option<VideoMeta>,
        audio: Option<AudioMeta>,
        remote_addr: Option<String>,
    ) {
        self.ingests
            .update_ingest_meta(pipeline_id, video, audio, remote_addr)
            .await;
    }

    /// Records what the session's encoder declared (`onMetaData`).
    pub async fn update_ingest_session_declared(
        &self,
        registration: &IngestRegistration,
        declared: DeclaredEncoder,
    ) {
        self.ingests
            .update_ingest_session_declared(registration, declared)
            .await;
    }

    pub async fn update_ingest_session_meta(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
        video: Option<VideoMeta>,
        audio: Option<AudioMeta>,
        remote_addr: Option<String>,
    ) {
        self.ingests
            .update_ingest_session_meta(pipeline_id, registration, video, audio, remote_addr)
            .await;
    }

    /// Replace an authoritative container track snapshot and report the
    /// probe recorded (`IngestRegistry::replace_ingest_session_tracks`).
    pub(crate) async fn replace_ingest_session_tracks(
        &self,
        ingest: &ActiveIngest,
        registration: &IngestRegistration,
        video: Option<VideoMeta>,
        audio_tracks: Vec<AudioMeta>,
        video_track_count: usize,
        video_sequence_header: Option<bytes::Bytes>,
    ) -> Option<Arc<restream_sync::RingBuffer>> {
        self.ingests
            .replace_ingest_session_tracks(
                ingest,
                registration,
                video,
                audio_tracks,
                video_track_count,
                video_sequence_header,
            )
            .await
    }

    /// Report that a session's probe has been recorded in full (see
    /// `IngestRegistry::note_ingest_session_probed`).
    pub async fn note_ingest_session_probed(&self, registration: &IngestRegistration) {
        self.ingests.note_ingest_session_probed(registration).await;
    }

    pub async fn update_ingest_session_video_track_selection(
        &self,
        registration: &IngestRegistration,
        video_track_count: usize,
        selected_video_track_index: Option<u32>,
    ) {
        self.ingests
            .update_ingest_session_video_track_selection(
                registration,
                video_track_count,
                selected_video_track_index,
            )
            .await;
    }

    pub async fn update_ingest_session_audio_tracks(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
        tracks: Vec<AudioMeta>,
    ) {
        self.ingests
            .update_ingest_session_audio_tracks(pipeline_id, registration, tracks)
            .await;
    }

    pub async fn cache_sequence_header(
        &self,
        pipeline_id: &str,
        is_video: bool,
        data: bytes::Bytes,
    ) {
        self.ingests
            .cache_sequence_header(pipeline_id, is_video, data)
            .await;
    }

    pub async fn get_ingest_session_sequence_headers(
        &self,
        registration: &IngestRegistration,
    ) -> (Option<bytes::Bytes>, Option<bytes::Bytes>) {
        self.ingests
            .get_ingest_session_sequence_headers(registration)
            .await
    }

    pub async fn get_sequence_headers(
        &self,
        pipeline_id: &str,
    ) -> (Option<bytes::Bytes>, Option<bytes::Bytes>) {
        self.ingests.get_sequence_headers(pipeline_id).await
    }

    pub async fn update_ingest_audio_tracks(&self, pipeline_id: &str, tracks: Vec<AudioMeta>) {
        self.ingests
            .update_ingest_audio_tracks(pipeline_id, tracks)
            .await;
    }

    pub async fn update_publisher_quality(&self, pipeline_id: &str, quality: PublisherQuality) {
        self.ingests
            .update_publisher_quality(pipeline_id, quality)
            .await;
    }

    /// Whether the pipeline's active ingest has the probe an output needs
    /// before it plans its tracks, or `None` when no ingest is active (see
    /// `IngestRegistry::ingest_tracks_ready`).
    pub async fn ingest_tracks_ready(&self, pipeline_id: &str) -> Option<bool> {
        self.ingests.ingest_tracks_ready(pipeline_id).await
    }

    pub async fn update_ingest_session_quality(
        &self,
        registration: &IngestRegistration,
        quality: PublisherQuality,
    ) {
        self.ingests
            .update_ingest_session_quality(registration, quality)
            .await;
    }
}
