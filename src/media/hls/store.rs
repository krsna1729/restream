use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;

use bytes::Bytes;

use super::HlsConfig;
use crate::media::metadata::{AudioMeta, VideoMeta};

const TARGET_DURATION_SECS: f64 = 6.0;

struct HlsSegment {
    index: u64,
    duration: f64,
    data: Bytes,
}

#[derive(Clone)]
pub struct HlsSegmentSnapshot {
    pub index: u64,
    pub data: Bytes,
}

pub struct HlsStoreSnapshot {
    pub playlist: String,
    pub segments: Vec<HlsSegmentSnapshot>,
}

pub struct HlsStore {
    inner: Mutex<HlsStoreInner>,
    config: HlsConfig,
}

struct HlsStoreInner {
    segments: VecDeque<HlsSegment>,
    next_index: u64,
    target_duration: f64,
    video: Option<VideoMeta>,
    audio_tracks: Vec<AudioMeta>,
    variant_segments: HashMap<(u64, HlsSegmentVariant), Bytes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HlsSegmentVariant {
    Video,
    Audio(u32),
}

impl Default for HlsStore {
    fn default() -> Self {
        Self::new()
    }
}

impl HlsStore {
    pub fn new() -> Self {
        Self::with_config(HlsConfig::default())
    }

    pub fn with_config(config: HlsConfig) -> Self {
        Self {
            inner: Mutex::new(HlsStoreInner {
                segments: VecDeque::new(),
                next_index: 0,
                target_duration: TARGET_DURATION_SECS,
                video: None,
                audio_tracks: Vec::new(),
                variant_segments: HashMap::new(),
            }),
            config,
        }
    }

    pub fn config(&self) -> HlsConfig {
        self.config
    }

    pub fn clear(&self) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.segments.clear();
        inner.next_index = 0;
        inner.target_duration = TARGET_DURATION_SECS;
        inner.variant_segments.clear();
    }

    pub fn push_segment(&self, duration: f64, data: Bytes) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let index = inner.next_index;
        inner.next_index += 1;
        if duration > inner.target_duration {
            inner.target_duration = duration.ceil();
        }
        inner.segments.push_back(HlsSegment {
            index,
            duration,
            data,
        });
        while inner.segments.len() > self.config.max_segments {
            if let Some(segment) = inner.segments.pop_front() {
                inner
                    .variant_segments
                    .retain(|(segment_index, _), _| *segment_index != segment.index);
            }
        }
    }

    pub fn get_playlist(&self) -> Option<String> {
        self.get_playlist_with_segment_uri(|index| format!("seg{index}.ts"))
    }

    pub fn get_playlist_with_segment_uri<F>(&self, mut segment_uri: F) -> Option<String>
    where
        F: FnMut(u64) -> String,
    {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.segments.is_empty() {
            return None;
        }
        let first_seq = inner.segments.front().map(|s| s.index).unwrap_or(0);
        let target_dur = inner.target_duration.ceil() as u64;

        let mut m3u8 = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
            target_dur, first_seq
        );
        for seg in &inner.segments {
            let uri = segment_uri(seg.index);
            m3u8.push_str(&format!("#EXTINF:{:.3},\n{}\n", seg.duration, uri));
        }
        Some(m3u8)
    }

    pub fn get_segment(&self, index: u64) -> Option<Bytes> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .segments
            .iter()
            .find(|s| s.index == index)
            .map(|s| s.data.clone())
    }

    pub fn set_stream_metadata(&self, video: Option<VideoMeta>, audio_tracks: Vec<AudioMeta>) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.video = video;
        inner.audio_tracks = audio_tracks;
    }

    pub fn stream_metadata(&self) -> (Option<VideoMeta>, Vec<AudioMeta>) {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        (inner.video.clone(), inner.audio_tracks.clone())
    }

    pub fn get_variant_segment(&self, index: u64, variant: HlsSegmentVariant) -> Option<Bytes> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner.variant_segments.get(&(index, variant)).cloned()
    }

    pub fn put_variant_segment(&self, index: u64, variant: HlsSegmentVariant, data: Bytes) {
        let mut inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.segments.iter().any(|segment| segment.index == index) {
            inner.variant_segments.insert((index, variant), data);
        }
    }

    pub fn snapshot(&self) -> Option<HlsStoreSnapshot> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        if inner.segments.is_empty() {
            return None;
        }
        let first_seq = inner.segments.front().map(|s| s.index).unwrap_or(0);
        let target_dur = inner.target_duration.ceil() as u64;
        let mut playlist = format!(
            "#EXTM3U\n#EXT-X-VERSION:3\n#EXT-X-TARGETDURATION:{}\n#EXT-X-MEDIA-SEQUENCE:{}\n",
            target_dur, first_seq
        );
        let mut segments = Vec::with_capacity(inner.segments.len());
        for seg in &inner.segments {
            playlist.push_str(&format!(
                "#EXTINF:{:.3},\nseg{}.ts\n",
                seg.duration, seg.index
            ));
            segments.push(HlsSegmentSnapshot {
                index: seg.index,
                data: seg.data.clone(),
            });
        }
        Some(HlsStoreSnapshot { playlist, segments })
    }
}
