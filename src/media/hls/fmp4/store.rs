use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use bytes::Bytes;

use super::super::HlsConfig;
use crate::media::metadata::{AudioMeta, VideoMeta};

// Keep a small cushion behind the advertised live window so a browser that
// fetches the oldest listed segment during a playlist refresh does not race
// immediate eviction.
pub(super) const PLAYLIST_RETENTION_GRACE_SEGMENTS: usize = 6;

#[derive(Clone)]
struct RenditionSegment {
    index: u64,
    duration: f64,
    data: Bytes,
}

#[derive(Clone, Default)]
struct RenditionPlaylistState {
    init_segment: Option<Bytes>,
    segments: VecDeque<RenditionSegment>,
    target_duration: Option<f64>,
}

impl RenditionPlaylistState {
    fn clear(&mut self) {
        self.init_segment = None;
        self.segments.clear();
        self.target_duration = None;
    }

    fn put_init_segment(&mut self, data: Bytes) {
        self.init_segment = Some(data);
    }

    fn retained_segment_limit(config: HlsConfig) -> usize {
        config
            .max_segments
            .saturating_add(PLAYLIST_RETENTION_GRACE_SEGMENTS)
    }

    fn push_segment(&mut self, config: HlsConfig, index: u64, duration: f64, data: Bytes) {
        self.target_duration = Some(
            self.target_duration
                .map_or(duration.ceil(), |current| current.max(duration.ceil())),
        );
        self.segments.push_back(RenditionSegment {
            index,
            duration,
            data,
        });
        while self.segments.len() > Self::retained_segment_limit(config) {
            let _ = self.segments.pop_front();
        }
    }

    fn playlist<F, G>(&self, config: HlsConfig, init_uri: F, mut segment_uri: G) -> Option<String>
    where
        F: FnOnce() -> String,
        G: FnMut(u64) -> String,
    {
        if self.segments.is_empty() {
            return None;
        }
        let advertised_start = self.segments.len().saturating_sub(config.max_segments);
        let first_seq = self
            .segments
            .get(advertised_start)
            .map(|segment| segment.index)
            .unwrap_or(0);
        let target_duration = self.target_duration.unwrap_or(1.0).ceil() as u64;
        let mut playlist = format!(
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target_duration}\n#EXT-X-MEDIA-SEQUENCE:{first_seq}\n#EXT-X-MAP:URI=\"{}\"\n",
            init_uri()
        );
        for segment in self.segments.iter().skip(advertised_start) {
            playlist.push_str(&format!(
                "#EXTINF:{:.3},\n{}\n",
                segment.duration,
                segment_uri(segment.index)
            ));
        }
        Some(playlist)
    }

    fn get_segment(&self, index: u64) -> Option<Bytes> {
        self.segments
            .iter()
            .find(|segment| segment.index == index)
            .map(|segment| segment.data.clone())
    }
}

pub struct Fmp4HlsStore {
    inner: Mutex<Fmp4HlsStoreInner>,
    config: HlsConfig,
}

struct Fmp4HlsStoreInner {
    video: RenditionPlaylistState,
    audio: HashMap<u32, RenditionPlaylistState>,
    video_meta: Option<VideoMeta>,
    audio_tracks: Vec<AudioMeta>,
}

impl Default for Fmp4HlsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl Fmp4HlsStore {
    pub fn new() -> Self {
        Self::with_config(HlsConfig::default())
    }

    pub fn with_config(config: HlsConfig) -> Self {
        Self {
            inner: Mutex::new(Fmp4HlsStoreInner {
                video: RenditionPlaylistState::default(),
                audio: HashMap::new(),
                video_meta: None,
                audio_tracks: Vec::new(),
            }),
            config,
        }
    }

    pub fn config(&self) -> HlsConfig {
        self.config
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.video.clear();
        inner.audio.clear();
    }

    pub fn set_stream_metadata(&self, video: Option<VideoMeta>, audio_tracks: Vec<AudioMeta>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.video_meta = video;
        inner.audio_tracks = audio_tracks.clone();
        inner.audio.retain(|track_index, _| {
            audio_tracks
                .iter()
                .any(|track| track.track_index == *track_index)
        });
        for track in audio_tracks {
            inner.audio.entry(track.track_index).or_default();
        }
    }

    pub fn stream_metadata(&self) -> (Option<VideoMeta>, Vec<AudioMeta>) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.video_meta.clone(), inner.audio_tracks.clone())
    }

    pub fn put_video_init_segment(&self, data: Bytes) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.video.put_init_segment(data);
    }

    pub fn put_audio_init_segment(&self, track_index: u32, data: Bytes) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .audio
            .entry(track_index)
            .or_default()
            .put_init_segment(data);
    }

    pub fn push_video_segment(&self, index: u64, duration: f64, data: Bytes) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.video.push_segment(self.config, index, duration, data);
    }

    pub fn publish_video_segment(&self, index: u64, duration: f64, init: Bytes, data: Bytes) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.video.put_init_segment(init);
        inner.video.push_segment(self.config, index, duration, data);
    }

    pub fn push_audio_segment(&self, track_index: u32, index: u64, duration: f64, data: Bytes) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.audio.entry(track_index).or_default().push_segment(
            self.config,
            index,
            duration,
            data,
        );
    }

    pub fn publish_audio_segment(
        &self,
        track_index: u32,
        index: u64,
        duration: f64,
        init: Bytes,
        data: Bytes,
    ) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let rendition = inner.audio.entry(track_index).or_default();
        rendition.put_init_segment(init);
        rendition.push_segment(self.config, index, duration, data);
    }

    pub fn has_video_playlist(&self) -> bool {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        !inner.video.segments.is_empty()
    }

    pub fn get_primary_playlist(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !inner.video.segments.is_empty() {
            return inner.video.playlist(
                self.config,
                || "video/init.mp4".to_string(),
                |index| format!("seg{index}.m4s"),
            );
        }
        let first_track = inner.audio_tracks.first()?;
        inner.audio.get(&first_track.track_index)?.playlist(
            self.config,
            || format!("audio/{}/init.mp4", first_track.track_index),
            |index| format!("audio/{}/seg{index}.m4s", first_track.track_index),
        )
    }

    pub fn get_video_playlist(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.video.playlist(
            self.config,
            || "init.mp4".to_string(),
            |index| format!("seg{index}.m4s"),
        )
    }

    pub fn get_audio_playlist(&self, track_index: u32) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.audio.get(&track_index)?.playlist(
            self.config,
            || "init.mp4".to_string(),
            |index| format!("seg{index}.m4s"),
        )
    }

    pub fn get_video_init_segment(&self) -> Option<Bytes> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.video.init_segment.clone()
    }

    pub fn get_audio_init_segment(&self, track_index: u32) -> Option<Bytes> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.audio.get(&track_index)?.init_segment.clone()
    }

    pub fn get_video_segment(&self, index: u64) -> Option<Bytes> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.video.get_segment(index)
    }

    pub fn get_audio_segment(&self, track_index: u32, index: u64) -> Option<Bytes> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.audio.get(&track_index)?.get_segment(index)
    }

    pub fn segment_count(&self) -> usize {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if !inner.video.segments.is_empty() {
            inner.video.segments.len()
        } else {
            inner
                .audio_tracks
                .first()
                .and_then(|track| inner.audio.get(&track.track_index))
                .map(|rendition| rendition.segments.len())
                .unwrap_or(0)
        }
    }

    pub fn primary_playlist_len(&self) -> usize {
        self.get_primary_playlist()
            .map(|playlist| playlist.len())
            .unwrap_or(0)
    }
}
