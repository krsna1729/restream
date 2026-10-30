//! Ingest progress, metadata, track, quality, and sequence-header state owned by `MediaEngine`.

use std::sync::Arc;
use std::sync::atomic::Ordering;

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::snapshots::PublisherQuality;
use restream_core::metadata::{AudioMeta, VideoMeta};

/// How long a source may publish media without reporting a probe before an
/// output stops waiting for one and starts video-only.
///
/// Ten seconds: longer than a long GOP, so an SRT publisher that reconnects
/// mid-GOP and completes its TS probe on the next SPS is still covered, and
/// longer than an HLS target duration. A source that never probes is the
/// exception this bound exists for; starting it a few seconds late costs far
/// less than a silent run without audio.
pub(crate) const PROBE_GRACE_MS: u64 = 10_000;

impl MediaEngine {
    pub async fn update_ingest_bytes(&self, pipeline_id: &str, bytes: u64) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            let now_ms = Self::now_epoch_ms();
            crate::media::engine::note_media_bytes(
                &ingest.bytes_received,
                &ingest.first_media_ms,
                bytes,
                now_ms,
            );
            ingest.last_progress_ms.store(now_ms, Ordering::Relaxed);
        }
    }

    pub async fn record_keyframe(&self, pipeline_id: &str, pts: i64) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            let mut times = restream_sync::lock::lock(&ingest.keyframe_times);
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
            let mut metadata = restream_sync::lock::write(&ingest.metadata);
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

    /// Records what the session's encoder declared (`onMetaData`).
    pub async fn update_ingest_session_declared(
        &self,
        registration: &IngestRegistration,
        declared: restream_core::metadata::DeclaredEncoder,
    ) {
        if let Some(ingest) = self.current_ingest_session(registration).await {
            restream_sync::lock::write(&ingest.metadata).declared = Some(declared);
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
            let pipelines = self.ingests.pipelines.read().await;
            if let Some(ring) = pipelines.get(pipeline_id) {
                ring.set_codec_hint(&video.codec);
            }
        }
    }

    /// Replace an authoritative container track snapshot, rather than merge
    /// optional probe fields. Selection, registration replacement and origin
    /// resolution share this lock: an old attempt can neither clear a new
    /// session's metadata nor resize/change its selected ring.
    pub(crate) async fn replace_ingest_session_tracks(
        &self,
        registration: &IngestRegistration,
        video: Option<VideoMeta>,
        audio_tracks: Vec<AudioMeta>,
        video_track_count: usize,
        video_sequence_header: Option<bytes::Bytes>,
    ) -> Option<Arc<restream_sync::RingBuffer>> {
        let _selection = self.ingests.selection_lock.lock().await;
        let ingest = self.current_ingest_session(registration).await?;
        if registration.cancel_token.is_cancelled() {
            return None;
        }
        let selected = self
            .is_ingest_session_selected(&ingest.pipeline_id, registration)
            .await;
        if selected {
            self.adapt_pipeline_ring(
                &ingest.pipeline_id,
                video.as_ref().map_or(0.0, |video| video.fps),
                audio_tracks.len(),
            )
            .await;
        }
        {
            let mut metadata = restream_sync::lock::write(&ingest.metadata);
            metadata.video = video.clone();
            metadata.audio = audio_tracks.first().cloned();
            metadata.video_track_count = video_track_count;
            metadata.selected_video_track_index = video.as_ref().map(|_| 0);
            metadata.tracks_authoritative = true;
        }
        *restream_sync::lock::lock(&ingest.audio_tracks) = Arc::new(audio_tracks.clone());
        *restream_sync::lock::lock(&ingest.video_sequence_header) = video_sequence_header;
        if audio_tracks.is_empty() {
            *restream_sync::lock::lock(&ingest.audio_sequence_header) = None;
        }
        let update_ring = |ring: &restream_sync::RingBuffer| {
            if let Some(video) = &video {
                ring.set_codec_hint(&video.codec);
            } else {
                ring.remove_video_track();
            }
            ring.set_audio_tracks(audio_tracks.clone());
        };
        if let Some(preview) = registration.preview_ring.load_full() {
            update_ring(&preview);
        }
        // A superseding snapshot may not resize again, and the source may
        // have been deselected. Its next accepted ack must still hand off to
        // the current shared ring, not strand its Owner on a sealed ancestor.
        let ring = self
            .ingests
            .pipelines
            .read()
            .await
            .get(&ingest.pipeline_id)
            .cloned();
        if selected && let Some(ring) = &ring {
            update_ring(ring);
        }
        ring
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
        let Some(ingest) = self.current_ingest_session(registration).await else {
            return;
        };
        self.runtime
            .event_log
            .emit(crate::media::events::EventKind::IngestProbed {
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
                *restream_sync::lock::lock(&ingest.video_sequence_header) = Some(data);
            } else {
                *restream_sync::lock::lock(&ingest.audio_sequence_header) = Some(data);
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

    pub async fn get_ingest_session_sequence_headers(
        &self,
        registration: &IngestRegistration,
    ) -> (Option<bytes::Bytes>, Option<bytes::Bytes>) {
        let Some(ingest) = self.current_ingest_session(registration).await else {
            return (None, None);
        };
        let video = restream_sync::lock::lock(&ingest.video_sequence_header).clone();
        let audio = restream_sync::lock::lock(&ingest.audio_sequence_header).clone();
        (video, audio)
    }

    /// The ingest's cached video sequence header, read without waiting: for
    /// codec threads, which must not enter async engine state. `None` when
    /// the pipeline has no ingest, no header yet, or the ingest map is being
    /// written at this instant (the caller retries on a later packet).
    pub fn try_video_sequence_header(&self, pipeline_id: &str) -> Option<bytes::Bytes> {
        let ingests = self.ingests.active.try_read().ok()?;
        restream_sync::lock::lock(&ingests.get(pipeline_id)?.video_sequence_header).clone()
    }

    pub async fn get_sequence_headers(
        &self,
        pipeline_id: &str,
    ) -> (Option<bytes::Bytes>, Option<bytes::Bytes>) {
        let ingests = self.ingests.active.read().await;
        if let Some(ingest) = ingests.get(pipeline_id) {
            let video = restream_sync::lock::lock(&ingest.video_sequence_header).clone();
            let audio = restream_sync::lock::lock(&ingest.audio_sequence_header).clone();
            (video, audio)
        } else {
            (None, None)
        }
    }

    pub async fn update_ingest_audio_tracks(&self, pipeline_id: &str, tracks: Vec<AudioMeta>) {
        {
            let ingests = self.ingests.active.read().await;
            if let Some(ingest) = ingests.get(pipeline_id) {
                *restream_sync::lock::lock(&ingest.audio_tracks) = Arc::new(tracks.clone());
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
            restream_sync::lock::write(&ingest.metadata).quality = quality;
        }
    }

    /// Whether the pipeline's active ingest has the probe an output needs
    /// before it plans its tracks, or `None` when no ingest is active (the
    /// caller decides from the disconnect grace).
    ///
    /// A direct egress resolves its track plan once, when it starts: the
    /// RTMP leaf builds the audio track, the AAC sequence header and the
    /// stream metadata from it, and drops raw AAC packets it cannot
    /// announce. HLS push ingest registers its input on the first uploaded
    /// object — the playlist, which carries no media — so an output started
    /// on that `IngestConnected` plans video-only tracks and then loses the
    /// whole stream's audio. Wait for the probe instead.
    ///
    /// The probe is the video or audio metadata, which every probe recorder
    /// writes after the audio tracks, the track selection and the sequence
    /// headers, so an open gate never shows a half-recorded probe.
    ///
    /// A source that publishes media without ever reporting one (no
    /// sequence header an ingest parses) is treated as track-less once its
    /// media has flowed for [`PROBE_GRACE_MS`], so its output starts
    /// video-only rather than never. That grace counts from the first media
    /// byte, never from registration: an SRT publisher reconnecting mid-GOP
    /// sends bytes at once and completes its TS probe only on the next SPS,
    /// and an HLS session whose first object is a playlist uploads its first
    /// segment one target duration later.
    pub async fn ingest_tracks_ready(&self, pipeline_id: &str) -> Option<bool> {
        let ingests = self.ingests.active.read().await;
        let ingest = ingests.get(pipeline_id)?;
        {
            let metadata = restream_sync::lock::read(&ingest.metadata);
            if metadata.video.is_some() || metadata.audio.is_some() {
                return Some(true);
            }
        }
        let first_media_ms = ingest.first_media_ms.load(Ordering::Relaxed);
        if first_media_ms == 0 {
            return Some(false);
        }
        Some(Self::now_epoch_ms().saturating_sub(first_media_ms) >= PROBE_GRACE_MS)
    }

    pub async fn update_ingest_session_quality(
        &self,
        registration: &IngestRegistration,
        quality: PublisherQuality,
    ) {
        if let Some(ingest) = self.current_ingest_session(registration).await {
            restream_sync::lock::write(&ingest.metadata).quality = quality;
        }
    }
}
