//! Per-attempt ingest metadata writes: a probe's tracks, selection and
//! sequence headers, and the report that the probe is recorded.

use std::sync::Arc;

use restream_core::metadata::{AudioMeta, VideoMeta};

use super::{ActiveIngest, IngestRegistration, IngestRegistry};
use crate::events::EventKind;

impl IngestRegistry {
    pub async fn update_ingest_session_meta(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
        video: Option<VideoMeta>,
        audio: Option<AudioMeta>,
        remote_addr: Option<String>,
    ) {
        let Some(ingest) = self.current_ingest_session(registration).await else {
            return;
        };
        {
            let mut metadata = restream_sync::lock::write(&ingest.metadata);
            if video.is_some() {
                metadata.video = video.clone();
                if metadata.video_track_count == 0 {
                    metadata.video_track_count = 1;
                }
                if metadata.selected_video_track_index.is_none() {
                    metadata.selected_video_track_index = Some(0);
                }
            }
            if audio.is_some() {
                metadata.audio = audio;
            }
            if remote_addr.is_some() {
                metadata.remote_addr = remote_addr;
            }
        }

        if let Some(video) = video.as_ref()
            && let Some(preview_ring) = registration.preview_ring.load_full()
        {
            preview_ring.set_codec_hint(&video.codec);
        }
        if self
            .is_ingest_session_selected(pipeline_id, registration)
            .await
            && let Some(video) = video
        {
            let pipelines = self.pipelines.read().await;
            if let Some(ring) = pipelines.get(pipeline_id) {
                ring.set_codec_hint(&video.codec);
            }
        }
    }

    /// Report that a session's probe has been recorded in full: its video
    /// and audio metadata, audio tracks, selection and sequence headers.
    ///
    /// The probe an output plans its tracks from is media-plane state that
    /// no other event reports, so this wakes the reconciler and an output
    /// skipped waiting for the probe starts on the next pass, not on its next
    /// retry interval. A probe recorder calls it once it has written the
    /// whole probe, so the pass it wakes never sees half of one.
    pub async fn note_ingest_session_probed(&self, registration: &IngestRegistration) {
        if let Some(ingest) = self.current_ingest_session(registration).await {
            self.emit_ingest_probed(&ingest);
        }
    }

    pub(crate) fn emit_ingest_probed(&self, ingest: &ActiveIngest) {
        self.events.emit(EventKind::IngestProbed {
            pipeline_id: ingest.pipeline_id.clone(),
            protocol: ingest.protocol.clone(),
        });
    }

    pub async fn update_ingest_session_video_track_selection(
        &self,
        registration: &IngestRegistration,
        video_track_count: usize,
        selected_video_track_index: Option<u32>,
    ) {
        if let Some(ingest) = self.current_ingest_session(registration).await {
            let mut metadata = restream_sync::lock::write(&ingest.metadata);
            metadata.video_track_count = video_track_count;
            metadata.selected_video_track_index = selected_video_track_index;
        }
    }

    pub async fn update_ingest_session_audio_tracks(
        &self,
        pipeline_id: &str,
        registration: &IngestRegistration,
        tracks: Vec<AudioMeta>,
    ) {
        let Some(ingest) = self.current_ingest_session(registration).await else {
            return;
        };
        *restream_sync::lock::lock(&ingest.audio_tracks) = Arc::new(tracks.clone());
        if !tracks.is_empty()
            && let Some(preview_ring) = registration.preview_ring.load_full()
        {
            preview_ring.set_audio_tracks(tracks.clone());
        }
        if !tracks.is_empty()
            && self
                .is_ingest_session_selected(pipeline_id, registration)
                .await
        {
            let pipelines = self.pipelines.read().await;
            if let Some(ring) = pipelines.get(pipeline_id) {
                ring.set_audio_tracks(tracks);
            }
        }
    }

    pub async fn cache_ingest_session_sequence_header(
        &self,
        registration: &IngestRegistration,
        is_video: bool,
        data: bytes::Bytes,
    ) {
        let Some(ingest) = self.current_ingest_session(registration).await else {
            return;
        };
        if is_video {
            *restream_sync::lock::lock(&ingest.video_sequence_header) = Some(data);
        } else {
            *restream_sync::lock::lock(&ingest.audio_sequence_header) = Some(data);
        }
    }
}
