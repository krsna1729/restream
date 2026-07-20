//! Ingest progress, metadata, track, quality, and sequence-header state owned by `MediaEngine`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::snapshots::PublisherQuality;

impl MediaEngine {
    pub async fn update_ingest_bytes(&self, pipeline_id: &str, bytes: u64) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            ingest.bytes_received.fetch_add(bytes, Ordering::Relaxed);
            ingest
                .last_progress_ms
                .store(Self::now_epoch_ms(), Ordering::Relaxed);
        }
    }

    pub async fn record_keyframe(&self, pipeline_id: &str, pts: i64) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            let mut times = ingest
                .keyframe_times
                .lock()
                .unwrap_or_else(|error| error.into_inner());
            times.push(pts);
            if times.len() > 30 {
                times.remove(0);
            }
        }
    }

    pub async fn update_ingest_meta(
        &self,
        pipeline_id: &str,
        video: Option<VideoMeta>,
        audio: Option<AudioMeta>,
        remote_addr: Option<String>,
    ) {
        if let Some(video_meta) = video.as_ref() {
            let pipelines = self.ingests.pipelines.read().await;
            if let Some(ring) = pipelines.get(pipeline_id) {
                ring.set_codec_hint(&video_meta.codec);
            }
        }
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            let mut metadata = ingest
                .metadata
                .write()
                .unwrap_or_else(|error| error.into_inner());
            if video.is_some() {
                metadata.video = video;
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
    }

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
            let mut metadata = ingest
                .metadata
                .write()
                .unwrap_or_else(|error| error.into_inner());
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
            let pipelines = self.ingests.pipelines.read().await;
            if let Some(ring) = pipelines.get(pipeline_id) {
                ring.set_codec_hint(&video.codec);
            }
        }
    }

    pub async fn update_ingest_video_track_selection(
        &self,
        pipeline_id: &str,
        video_track_count: usize,
        selected_video_track_index: Option<u32>,
    ) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            let mut metadata = ingest
                .metadata
                .write()
                .unwrap_or_else(|error| error.into_inner());
            metadata.video_track_count = video_track_count;
            metadata.selected_video_track_index = selected_video_track_index;
        }
    }

    pub async fn update_ingest_session_video_track_selection(
        &self,
        registration: &IngestRegistration,
        video_track_count: usize,
        selected_video_track_index: Option<u32>,
    ) {
        if let Some(ingest) = self.current_ingest_session(registration).await {
            let mut metadata = ingest
                .metadata
                .write()
                .unwrap_or_else(|error| error.into_inner());
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
        *ingest
            .audio_tracks
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = Arc::new(tracks.clone());
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
            let pipelines = self.ingests.pipelines.read().await;
            if let Some(ring) = pipelines.get(pipeline_id) {
                ring.set_audio_tracks(tracks);
            }
        }
    }

    pub async fn cache_sequence_header(
        &self,
        pipeline_id: &str,
        is_video: bool,
        data: bytes::Bytes,
    ) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            if is_video {
                *ingest
                    .video_sequence_header
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(data);
            } else {
                *ingest
                    .audio_sequence_header
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Some(data);
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
            *ingest
                .video_sequence_header
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(data);
        } else {
            *ingest
                .audio_sequence_header
                .lock()
                .unwrap_or_else(|error| error.into_inner()) = Some(data);
        }
    }

    pub async fn get_ingest_session_sequence_headers(
        &self,
        registration: &IngestRegistration,
    ) -> (Option<bytes::Bytes>, Option<bytes::Bytes>) {
        let Some(ingest) = self.current_ingest_session(registration).await else {
            return (None, None);
        };
        let video = ingest
            .video_sequence_header
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        let audio = ingest
            .audio_sequence_header
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .clone();
        (video, audio)
    }

    pub async fn get_sequence_headers(
        &self,
        pipeline_id: &str,
    ) -> (Option<bytes::Bytes>, Option<bytes::Bytes>) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            let video = ingest
                .video_sequence_header
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            let audio = ingest
                .audio_sequence_header
                .lock()
                .unwrap_or_else(|error| error.into_inner())
                .clone();
            (video, audio)
        } else {
            (None, None)
        }
    }

    pub async fn update_ingest_audio_tracks(&self, pipeline_id: &str, tracks: Vec<AudioMeta>) {
        {
            let ingests = self.ingests.active.read().await;
            if let Some(ingest) = ingests.get(pipeline_id) {
                *ingest
                    .audio_tracks
                    .lock()
                    .unwrap_or_else(|error| error.into_inner()) = Arc::new(tracks.clone());
            }
        }
        if !tracks.is_empty() {
            let pipelines = self.ingests.pipelines.read().await;
            if let Some(ring) = pipelines.get(pipeline_id) {
                ring.set_audio_tracks(tracks);
            }
        }
    }

    pub async fn update_publisher_quality(&self, pipeline_id: &str, quality: PublisherQuality) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            ingest
                .metadata
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .quality = quality;
        }
    }

    pub async fn update_ingest_session_quality(
        &self,
        registration: &IngestRegistration,
        quality: PublisherQuality,
    ) {
        if let Some(ingest) = self.current_ingest_session(registration).await {
            ingest
                .metadata
                .write()
                .unwrap_or_else(|error| error.into_inner())
                .quality = quality;
        }
    }
}
