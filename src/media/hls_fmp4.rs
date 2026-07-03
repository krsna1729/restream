//! In-memory HLS preview packager for fragmented MP4 renditions.
//!
//! The served preview path intentionally diverges from remote HLS PUT uploads:
//! preview uses native fMP4 so we can expose one muxer per HLS rendition
//! (video plus alternate audio playlists), while upload keeps MPEG-TS because
//! common ingest targets such as YouTube HLS PUT require `.ts` media segments.
//!
//! The rendition split is also deliberate for the muxer surface we use here:
//! `shiguredo_mp4::mux::Fmp4SegmentMuxer` currently handles one audio track and
//! one video track per muxer. HLS alternate audio already models separate
//! audio-only playlists, so one muxer per rendition keeps the code small and
//! matches the HLS presentation model instead of forcing many audio tracks into
//! one fragmented MP4 stream.

use std::collections::{HashMap, VecDeque};
use std::num::NonZeroU32;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use bytes::Bytes;
use shiguredo_mp4::{
    FixedPointNumber, TrackKind, Uint,
    boxes::EsdsBox,
    boxes::{
        AudioSampleEntryFields, Avc1Box, AvccBox, Mp4aBox, SampleEntry, VisualSampleEntryFields,
    },
    descriptors::{DecoderConfigDescriptor, DecoderSpecificInfo, EsDescriptor, SlConfigDescriptor},
    mux::{Fmp4SegmentMuxer, Sample},
};
use tokio_util::sync::CancellationToken;
use tracing::warn;

use crate::media::codec::{
    adts_frame_count, annexb_to_avcc_into, build_aac_sequence_header, build_avcc_sequence_header,
    strip_adts,
};
use crate::media::engine::{AudioMeta, MediaEngine, VideoMeta};
use crate::media::hls::HlsConfig;
use crate::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat, Reader, RingBuffer};

const VIDEO_TIMESCALE: u32 = 90_000;

#[derive(Clone)]
struct RenditionSegment {
    index: u64,
    duration: f64,
    data: Bytes,
}

#[derive(Clone)]
struct RenditionPlaylistState {
    init_segment: Option<Bytes>,
    segments: VecDeque<RenditionSegment>,
    target_duration: f64,
}

impl Default for RenditionPlaylistState {
    fn default() -> Self {
        Self {
            init_segment: None,
            segments: VecDeque::new(),
            target_duration: 6.0,
        }
    }
}

impl RenditionPlaylistState {
    fn clear(&mut self) {
        self.init_segment = None;
        self.segments.clear();
        self.target_duration = 6.0;
    }

    fn put_init_segment(&mut self, data: Bytes) {
        self.init_segment = Some(data);
    }

    fn push_segment(&mut self, config: HlsConfig, index: u64, duration: f64, data: Bytes) {
        if duration > self.target_duration {
            self.target_duration = duration.ceil();
        }
        self.segments.push_back(RenditionSegment {
            index,
            duration,
            data,
        });
        while self.segments.len() > config.max_segments {
            let _ = self.segments.pop_front();
        }
    }

    fn playlist<F, G>(&self, init_uri: F, mut segment_uri: G) -> Option<String>
    where
        F: FnOnce() -> String,
        G: FnMut(u64) -> String,
    {
        if self.segments.is_empty() {
            return None;
        }
        let first_seq = self
            .segments
            .front()
            .map(|segment| segment.index)
            .unwrap_or(0);
        let target_duration = self.target_duration.ceil() as u64;
        let mut playlist = format!(
            "#EXTM3U\n#EXT-X-VERSION:7\n#EXT-X-TARGETDURATION:{target_duration}\n#EXT-X-MEDIA-SEQUENCE:{first_seq}\n#EXT-X-MAP:URI=\"{}\"\n",
            init_uri()
        );
        for segment in &self.segments {
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
        let config = HlsConfig::from_env();
        tracing::info!(?config, "loaded fmp4 hls config");
        Self::with_config(config)
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
                || "video/init.mp4".to_string(),
                |index| format!("seg{index}.m4s"),
            );
        }
        let first_track = inner.audio_tracks.first()?;
        inner.audio.get(&first_track.track_index)?.playlist(
            || format!("audio/{}/init.mp4", first_track.track_index),
            |index| format!("audio/{}/seg{index}.m4s", first_track.track_index),
        )
    }

    pub fn get_video_playlist(&self) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .video
            .playlist(|| "init.mp4".to_string(), |index| format!("seg{index}.m4s"))
    }

    pub fn get_audio_playlist(&self, track_index: u32) -> Option<String> {
        let inner = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        inner
            .audio
            .get(&track_index)?
            .playlist(|| "init.mp4".to_string(), |index| format!("seg{index}.m4s"))
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

pub async fn start_hls_fmp4_segmenter(
    pipeline_id: String,
    store: Arc<Fmp4HlsStore>,
    ring_buffer: Arc<RingBuffer>,
    audio_ring_buffer: Option<Arc<RingBuffer>>,
    engine: Arc<MediaEngine>,
    cancel_token: CancellationToken,
    video_meta_override: Option<VideoMeta>,
) {
    let hls_stage_key = crate::domain::stage::StageKey::new(
        pipeline_id.as_str(),
        crate::domain::stage::StageKind::hls(),
    );
    let metrics = engine
        .get_or_create_stage_metrics(hls_stage_key.clone())
        .await;
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStarted {
            pipeline_id: pipeline_id.clone(),
            encoding: "hls".to_string(),
        });

    let mut reader = Reader::new(format!("hls-fmp4:{pipeline_id}"), ring_buffer.clone());
    let mut audio_reader = audio_ring_buffer
        .clone()
        .map(|ring| Reader::new(format!("hls-fmp4-audio:{pipeline_id}"), ring));
    let mut packets = Vec::with_capacity(32);
    let mut audio_packets = Vec::with_capacity(32);
    let (video_sequence_header, audio_sequence_header) =
        engine.get_sequence_headers(&pipeline_id).await;
    let config = store.config();
    let min_segment_ms = (config.min_segment_secs * 1000.0).round() as i64;
    let preview_video_meta = video_meta_override.clone();

    let mut video_state: Option<VideoRenditionState> = None;
    let mut audio_states: HashMap<u32, AudioRenditionState> = HashMap::new();
    let mut next_segment_index = 0u64;
    let mut got_first_keyframe = false;
    let mut global_zero_ms = 0i64;
    let mut segment_start_pts_ms = 0i64;

    loop {
        tokio::select! {
            _ = cancel_token.cancelled() => break,
            _ = reader.wait_for_data() => {
                loop {
                    packets.clear();
                    match reader.pull_burst(&mut packets, 32) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {}
                    }

                    if let Some(audio_reader) = audio_reader.as_mut() {
                        audio_packets.clear();
                        let _ = audio_reader.pull_burst(&mut audio_packets, 32);
                    }

                    for packet in packets.iter().chain(
                        audio_packets
                            .iter()
                            .filter(|packet| packet.media_type == MediaType::Audio),
                    ) {
                        metrics.record_in(packet.payload.len() as u64);

                        if video_state.is_none() {
                            let Some((video, audio_tracks)) = resolve_hls_preview_metadata(
                                &engine,
                                &ring_buffer,
                                audio_ring_buffer.as_ref(),
                                &cancel_token,
                                &pipeline_id,
                                preview_video_meta.clone(),
                            )
                            .await else {
                                engine.remove_stage_metrics(&hls_stage_key).await;
                                engine.runtime.event_log.emit(crate::events::EventKind::StageStopped {
                                    pipeline_id: pipeline_id.clone(),
                                    encoding: "hls".to_string(),
                                });
                                return;
                            };

                            let supported_audio_tracks: Vec<AudioMeta> = audio_tracks
                                .into_iter()
                                .filter(|track| track.codec.eq_ignore_ascii_case("aac"))
                                .collect();
                            store.set_stream_metadata(Some(video.clone()), supported_audio_tracks.clone());
                            video_state = Some(VideoRenditionState::new(
                                &video,
                                video_sequence_header.as_deref(),
                            ));
                            for track in supported_audio_tracks {
                                audio_states.insert(
                                    track.track_index,
                                    AudioRenditionState::new(
                                        &track,
                                        audio_sequence_header.as_deref(),
                                    ),
                                );
                            }
                        }

                        let t0 = Instant::now();
                        match packet.media_type {
                            MediaType::Video => {
                                let Some(state) = video_state.as_mut() else {
                                    continue;
                                };
                                if packet.is_keyframe {
                                    if !got_first_keyframe {
                                        got_first_keyframe = true;
                                        global_zero_ms = packet.dts;
                                        segment_start_pts_ms = packet.pts;
                                    } else if packet.pts - segment_start_pts_ms >= min_segment_ms {
                                        if let Err(err) = state.flush_segment(
                                            &store,
                                            next_segment_index,
                                            segment_duration_secs(segment_start_pts_ms, packet.pts),
                                            Some(packet.dts),
                                        ) {
                                            warn!(pipeline_id = %pipeline_id, err = %err, "failed to flush video fmp4 segment");
                                        }
                                        for audio_state in audio_states.values_mut() {
                                            if let Err(err) = audio_state.flush_segment(
                                                &store,
                                                next_segment_index,
                                                segment_duration_secs(segment_start_pts_ms, packet.pts),
                                            ) {
                                                warn!(pipeline_id = %pipeline_id, err = %err, "failed to flush audio fmp4 segment");
                                            }
                                        }
                                        next_segment_index += 1;
                                        segment_start_pts_ms = packet.pts;
                                    }
                                }

                                if !got_first_keyframe || packet.dts < global_zero_ms {
                                    continue;
                                }
                                if let Err(err) = state.push_packet(packet, global_zero_ms) {
                                    warn!(pipeline_id = %pipeline_id, err = %err, "dropping video packet from fmp4 preview");
                                }
                            }
                            MediaType::Audio => {
                                if !got_first_keyframe || packet.dts < global_zero_ms {
                                    continue;
                                }
                                let Some(state) = audio_states.get_mut(&packet.track_index) else {
                                    continue;
                                };
                                if let Err(err) = state.push_packet(packet, global_zero_ms) {
                                    warn!(
                                        pipeline_id = %pipeline_id,
                                        track_index = packet.track_index,
                                        err = %err,
                                        "dropping audio packet from fmp4 preview"
                                    );
                                }
                            }
                        }
                        metrics.record_processing(t0.elapsed().as_micros() as u64);
                    }
                }
            }
        }
    }

    if let Some(state) = video_state.as_mut()
        && let Err(err) = state.flush_segment(
            &store,
            next_segment_index,
            state.current_segment_duration_secs(),
            None,
        )
    {
        warn!(pipeline_id = %pipeline_id, err = %err, "failed to flush final video fmp4 segment");
    }
    for audio_state in audio_states.values_mut() {
        if let Err(err) = audio_state.flush_segment(
            &store,
            next_segment_index,
            audio_state.current_segment_duration_secs(),
        ) {
            warn!(pipeline_id = %pipeline_id, err = %err, "failed to flush final audio fmp4 segment");
        }
    }

    engine.remove_stage_metrics(&hls_stage_key).await;
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id,
            encoding: "hls".to_string(),
        });
}

async fn resolve_hls_preview_metadata(
    engine: &MediaEngine,
    ring_buffer: &Arc<RingBuffer>,
    audio_ring_buffer: Option<&Arc<RingBuffer>>,
    cancel_token: &CancellationToken,
    pipeline_id: &str,
    preview_video_meta: Option<VideoMeta>,
) -> Option<(VideoMeta, Vec<AudioMeta>)> {
    loop {
        if cancel_token.is_cancelled() {
            return None;
        }
        if let Some(tracks) = ring_buffer
            .audio_tracks()
            .filter(|tracks| !tracks.is_empty())
        {
            let video = if let Some(video) = preview_video_meta.clone() {
                Some(video)
            } else {
                let ingests = engine.ingests.active.read().await;
                ingests
                    .get(pipeline_id)
                    .and_then(|ingest| ingest.video.clone())
            };
            if let Some(video) = video {
                return Some((video, tracks.to_vec()));
            }
        }
        if let Some(audio_ring_buffer) = audio_ring_buffer
            && let Some(tracks) = audio_ring_buffer
                .audio_tracks()
                .filter(|tracks| !tracks.is_empty())
        {
            let video = if let Some(video) = preview_video_meta.clone() {
                Some(video)
            } else {
                let ingests = engine.ingests.active.read().await;
                ingests
                    .get(pipeline_id)
                    .and_then(|ingest| ingest.video.clone())
            };
            if let Some(video) = video {
                return Some((video, tracks.to_vec()));
            }
        }
        let result = {
            let ingests = engine.ingests.active.read().await;
            ingests.get(pipeline_id).and_then(|ingest| {
                let video = preview_video_meta.clone().or(ingest.video.clone())?;
                let lock = ingest
                    .audio_tracks
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                let tracks = if lock.is_empty() {
                    ingest
                        .audio
                        .clone()
                        .map(|audio| vec![audio])
                        .unwrap_or_default()
                } else {
                    lock.as_ref().clone()
                };
                Some((video, tracks))
            })
        };
        if let Some(result) = result {
            return Some(result);
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
}

struct BufferedSample {
    pts: i64,
    dts: i64,
    keyframe: bool,
    data_offset: u64,
    data_size: usize,
    default_duration: u32,
}

#[derive(Default)]
struct MonotonicTimestampState {
    last_dts: Option<i64>,
}

impl MonotonicTimestampState {
    fn enforce(&mut self, pts: i64, dts: i64) -> (i64, i64) {
        let offset = pts.saturating_sub(dts);
        let corrected_dts = match self.last_dts {
            Some(last) if dts <= last => last + 1,
            _ => dts,
        };
        let corrected_pts = corrected_dts.saturating_add(offset);
        self.last_dts = Some(corrected_dts);
        (corrected_pts, corrected_dts)
    }
}

struct VideoRenditionState {
    video_meta: VideoMeta,
    muxer: Fmp4SegmentMuxer,
    sample_entry: Option<SampleEntry>,
    config_bytes: Option<Vec<u8>>,
    payload: Vec<u8>,
    samples: Vec<BufferedSample>,
    timestamps: MonotonicTimestampState,
    default_duration: u32,
    current_segment_start_ms: Option<i64>,
}

impl VideoRenditionState {
    fn new(video: &VideoMeta, video_sequence_header: Option<&[u8]>) -> Self {
        let sample_entry = video_sequence_header
            .and_then(|bytes| build_h264_sample_entry_from_flv_sequence_header(bytes, video));
        let config_bytes = sample_entry.as_ref().and_then(sample_entry_to_avcc_bytes);
        Self {
            video_meta: video.clone(),
            muxer: Fmp4SegmentMuxer::new().expect("fmp4 muxer must construct"),
            sample_entry,
            config_bytes,
            payload: Vec::new(),
            samples: Vec::new(),
            timestamps: MonotonicTimestampState::default(),
            default_duration: default_video_duration(video),
            current_segment_start_ms: None,
        }
    }

    fn push_packet(&mut self, packet: &MediaPacket, zero_ms: i64) -> Result<(), String> {
        if packet.format == PayloadFormat::Flv && packet.payload.len() > 1 && packet.payload[1] == 0
        {
            self.sample_entry =
                build_h264_sample_entry_from_flv_sequence_header(&packet.payload, &self.video_meta);
            self.config_bytes = self
                .sample_entry
                .as_ref()
                .and_then(sample_entry_to_avcc_bytes);
            return Ok(());
        }

        if self.sample_entry.is_none()
            && let Some(sample_entry) =
                build_h264_sample_entry_from_video_packet(packet, &self.video_meta)
        {
            self.config_bytes = sample_entry_to_avcc_bytes(&sample_entry);
            self.sample_entry = Some(sample_entry);
        }
        let Some(sample_entry) = self.sample_entry.clone() else {
            return Err("missing avc1 sample entry".to_string());
        };

        let payload_start = self.payload.len() as u64;
        match packet.format {
            PayloadFormat::Flv => {
                if packet.payload.len() <= 5 || packet.payload[1] == 0 {
                    return Ok(());
                }
                self.payload.extend_from_slice(&packet.payload[5..]);
            }
            PayloadFormat::Raw => {
                annexb_to_avcc_into(&packet.payload, &mut self.payload);
            }
        }
        let payload_size = self.payload.len().saturating_sub(payload_start as usize);
        if payload_size == 0 {
            return Ok(());
        }

        let raw_pts = rescale_ms(packet.pts.saturating_sub(zero_ms), VIDEO_TIMESCALE);
        let raw_dts = rescale_ms(packet.dts.saturating_sub(zero_ms), VIDEO_TIMESCALE);
        let (pts, dts) = self.timestamps.enforce(raw_pts, raw_dts);
        self.current_segment_start_ms
            .get_or_insert(packet.pts.saturating_sub(zero_ms));
        self.samples.push(BufferedSample {
            pts,
            dts,
            keyframe: packet.is_keyframe,
            data_offset: payload_start,
            data_size: payload_size,
            default_duration: self.default_duration,
        });
        let _ = sample_entry;
        Ok(())
    }

    fn flush_segment(
        &mut self,
        store: &Fmp4HlsStore,
        index: u64,
        duration_secs: f64,
        next_segment_first_dts_ms: Option<i64>,
    ) -> Result<(), String> {
        if self.samples.is_empty() {
            self.current_segment_start_ms = None;
            self.payload.clear();
            return Ok(());
        }
        let Some(sample_entry) = self.sample_entry.clone() else {
            return Err("missing avc1 sample entry".to_string());
        };
        let next_dts = next_segment_first_dts_ms.map(|dts_ms| rescale_ms(dts_ms, VIDEO_TIMESCALE));
        let samples = build_mux_samples(
            &self.samples,
            TrackKind::Video,
            VIDEO_TIMESCALE,
            sample_entry,
            next_dts,
        )?;
        let metadata = self
            .muxer
            .create_media_segment_metadata(&samples)
            .map_err(|err| err.to_string())?;
        let mut segment = metadata;
        segment.extend_from_slice(&self.payload);
        let init = self
            .muxer
            .init_segment_bytes()
            .map_err(|err| err.to_string())?;
        store.publish_video_segment(
            index,
            duration_secs.max(0.001),
            Bytes::from(init),
            Bytes::from(segment),
        );
        self.samples.clear();
        self.payload.clear();
        self.current_segment_start_ms = None;
        Ok(())
    }

    fn current_segment_duration_secs(&self) -> f64 {
        self.current_segment_start_ms
            .zip(self.samples.last())
            .map(|(start_ms, last)| ((last.pts / 90) as f64 - start_ms as f64).max(1.0) / 1000.0)
            .unwrap_or(1.0)
    }
}

struct AudioRenditionState {
    track_index: u32,
    sample_rate: u32,
    muxer: Fmp4SegmentMuxer,
    sample_entry: SampleEntry,
    payload: Vec<u8>,
    samples: Vec<BufferedSample>,
    timestamps: MonotonicTimestampState,
    current_segment_start_ms: Option<i64>,
}

impl AudioRenditionState {
    fn new(track: &AudioMeta, audio_sequence_header: Option<&[u8]>) -> Self {
        Self {
            track_index: track.track_index,
            sample_rate: track.sample_rate.max(1),
            muxer: Fmp4SegmentMuxer::new().expect("fmp4 muxer must construct"),
            sample_entry: build_aac_sample_entry(track, audio_sequence_header),
            payload: Vec::new(),
            samples: Vec::new(),
            timestamps: MonotonicTimestampState::default(),
            current_segment_start_ms: None,
        }
    }

    fn push_packet(&mut self, packet: &MediaPacket, zero_ms: i64) -> Result<(), String> {
        let raw_payload = match packet.format {
            PayloadFormat::Flv => {
                if packet.payload.len() <= 2 || packet.payload[1] == 0 {
                    return Ok(());
                }
                &packet.payload[2..]
            }
            PayloadFormat::Raw => strip_adts(&packet.payload),
        };
        if raw_payload.is_empty() {
            return Ok(());
        }

        let payload_start = self.payload.len() as u64;
        self.payload.extend_from_slice(raw_payload);
        let payload_size = raw_payload.len();
        let raw_pts = rescale_ms(packet.pts.saturating_sub(zero_ms), self.sample_rate);
        let raw_dts = rescale_ms(packet.dts.saturating_sub(zero_ms), self.sample_rate);
        let (pts, dts) = self.timestamps.enforce(raw_pts, raw_dts);
        self.current_segment_start_ms
            .get_or_insert(packet.pts.saturating_sub(zero_ms));
        self.samples.push(BufferedSample {
            pts,
            dts,
            keyframe: true,
            data_offset: payload_start,
            data_size: payload_size,
            default_duration: audio_default_duration(packet, self.sample_rate),
        });
        Ok(())
    }

    fn flush_segment(
        &mut self,
        store: &Fmp4HlsStore,
        index: u64,
        duration_secs: f64,
    ) -> Result<(), String> {
        if self.samples.is_empty() {
            self.current_segment_start_ms = None;
            self.payload.clear();
            return Ok(());
        }
        let timescale = self.sample_rate.max(1);
        let samples = build_mux_samples(
            &self.samples,
            TrackKind::Audio,
            timescale,
            self.sample_entry.clone(),
            None,
        )?;
        let metadata = self
            .muxer
            .create_media_segment_metadata(&samples)
            .map_err(|err| err.to_string())?;
        let mut segment = metadata;
        segment.extend_from_slice(&self.payload);
        let init = self
            .muxer
            .init_segment_bytes()
            .map_err(|err| err.to_string())?;
        store.publish_audio_segment(
            self.track_index,
            index,
            duration_secs.max(0.001),
            Bytes::from(init),
            Bytes::from(segment),
        );
        self.samples.clear();
        self.payload.clear();
        self.current_segment_start_ms = None;
        Ok(())
    }

    fn current_segment_duration_secs(&self) -> f64 {
        self.current_segment_start_ms
            .zip(self.samples.last())
            .map(|(start_ms, last)| {
                let last_ms = ((last.pts as f64) * 1000.0 / self.sample_rate as f64).round() as i64;
                (last_ms.saturating_sub(start_ms)).max(1) as f64 / 1000.0
            })
            .unwrap_or(1.0)
    }
}

fn build_mux_samples(
    buffered: &[BufferedSample],
    track_kind: TrackKind,
    timescale: u32,
    sample_entry: SampleEntry,
    next_segment_first_dts: Option<i64>,
) -> Result<Vec<Sample>, String> {
    let timescale = NonZeroU32::new(timescale).ok_or_else(|| "zero timescale".to_string())?;
    let mut samples = Vec::with_capacity(buffered.len());
    for (index, sample) in buffered.iter().enumerate() {
        let next_dts = buffered
            .get(index + 1)
            .map(|next| next.dts)
            .or(next_segment_first_dts)
            .unwrap_or_else(|| sample.dts + sample.default_duration as i64);
        let duration = next_dts.saturating_sub(sample.dts);
        if duration <= 0 || duration > u32::MAX as i64 {
            return Err(format!("invalid sample duration: {duration}"));
        }
        let composition_time_offset = if track_kind == TrackKind::Video {
            let cto = sample.pts.saturating_sub(sample.dts);
            if cto == 0 {
                None
            } else if !(i32::MIN as i64..=i32::MAX as i64).contains(&cto) {
                return Err(format!("composition offset out of i32 range: {cto}"));
            } else {
                Some(cto)
            }
        } else {
            None
        };
        samples.push(Sample {
            track_kind,
            sample_entry: Some(sample_entry.clone()),
            keyframe: sample.keyframe,
            timescale,
            duration: duration as u32,
            composition_time_offset,
            data_offset: sample.data_offset,
            data_size: sample.data_size,
        });
    }
    Ok(samples)
}

fn build_h264_sample_entry_from_video_packet(
    packet: &MediaPacket,
    video: &VideoMeta,
) -> Option<SampleEntry> {
    match packet.format {
        PayloadFormat::Flv => {
            build_h264_sample_entry_from_flv_sequence_header(&packet.payload, video)
        }
        PayloadFormat::Raw => {
            let seq = build_avcc_sequence_header(&packet.payload)?;
            build_h264_sample_entry_from_flv_sequence_header(seq.as_ref(), video)
        }
    }
}

fn build_h264_sample_entry_from_flv_sequence_header(
    sequence_header: &[u8],
    video: &VideoMeta,
) -> Option<SampleEntry> {
    let avcc = sequence_header.get(5..)?;
    let avcc_box = parse_avcc_box(avcc)?;
    Some(SampleEntry::Avc1(Avc1Box {
        visual: VisualSampleEntryFields {
            data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
            width: video.width.min(u16::MAX as u32) as u16,
            height: video.height.min(u16::MAX as u32) as u16,
            horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
            vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
            frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
            compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
            depth: VisualSampleEntryFields::DEFAULT_DEPTH,
        },
        avcc_box,
        unknown_boxes: Vec::new(),
    }))
}

fn parse_avcc_box(data: &[u8]) -> Option<AvccBox> {
    if data.len() < 7 {
        return None;
    }
    let mut pos = 6usize;
    let num_sps = (data[5] & 0x1F) as usize;
    let mut sps_list = Vec::with_capacity(num_sps);
    for _ in 0..num_sps {
        let len = u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2;
        sps_list.push(data.get(pos..pos + len)?.to_vec());
        pos += len;
    }
    let num_pps = *data.get(pos)? as usize;
    pos += 1;
    let mut pps_list = Vec::with_capacity(num_pps);
    for _ in 0..num_pps {
        let len = u16::from_be_bytes([*data.get(pos)?, *data.get(pos + 1)?]) as usize;
        pos += 2;
        pps_list.push(data.get(pos..pos + len)?.to_vec());
        pos += len;
    }
    let mut avcc = AvccBox {
        avc_profile_indication: data[1],
        profile_compatibility: data[2],
        avc_level_indication: data[3],
        length_size_minus_one: Uint::new(data[4] & 0x03),
        sps_list,
        pps_list,
        chroma_format: None,
        bit_depth_luma_minus8: None,
        bit_depth_chroma_minus8: None,
        sps_ext_list: Vec::new(),
    };
    if let Some(sps) = avcc.sps_list.first()
        && let Some(fields) = parse_h264_sps_avcc_fields(sps)
    {
        avcc.chroma_format = Some(fields.chroma_format);
        avcc.bit_depth_luma_minus8 = Some(fields.bit_depth_luma_minus8);
        avcc.bit_depth_chroma_minus8 = Some(fields.bit_depth_chroma_minus8);
    }
    Some(avcc)
}

struct AvccProfileFields {
    chroma_format: Uint<u8, 2>,
    bit_depth_luma_minus8: Uint<u8, 3>,
    bit_depth_chroma_minus8: Uint<u8, 3>,
}

fn parse_h264_sps_avcc_fields(sps_nalu: &[u8]) -> Option<AvccProfileFields> {
    if sps_nalu.is_empty() {
        return None;
    }
    let rbsp = remove_emulation_prevention_bytes(sps_nalu);
    let mut reader = H264BitReader::new(&rbsp);
    reader.skip(8)?; // nal_unit_header
    let profile_idc = reader.read_bits(8)? as u8;
    reader.skip(8)?; // constraint flags + reserved_zero_2bits
    reader.skip(8)?; // level_idc
    let _seq_parameter_set_id = reader.read_exp_golomb()?;

    if matches!(profile_idc, 66 | 77 | 88) {
        return None;
    }

    let chroma_format = reader.read_exp_golomb()?;
    if chroma_format > 3 {
        return None;
    }
    if chroma_format == 3 {
        reader.skip(1)?; // separate_colour_plane_flag
    }

    let bit_depth_luma_minus8 = reader.read_exp_golomb()?;
    let bit_depth_chroma_minus8 = reader.read_exp_golomb()?;
    if bit_depth_luma_minus8 > 7 || bit_depth_chroma_minus8 > 7 {
        return None;
    }

    Some(AvccProfileFields {
        chroma_format: Uint::new(chroma_format as u8),
        bit_depth_luma_minus8: Uint::new(bit_depth_luma_minus8 as u8),
        bit_depth_chroma_minus8: Uint::new(bit_depth_chroma_minus8 as u8),
    })
}

fn remove_emulation_prevention_bytes(data: &[u8]) -> Vec<u8> {
    let mut rbsp = Vec::with_capacity(data.len());
    let mut index = 0;
    while index < data.len() {
        if index + 2 < data.len()
            && data[index] == 0
            && data[index + 1] == 0
            && data[index + 2] == 3
        {
            rbsp.push(0);
            rbsp.push(0);
            index += 3;
        } else {
            rbsp.push(data[index]);
            index += 1;
        }
    }
    rbsp
}

struct H264BitReader<'a> {
    data: &'a [u8],
    bit_pos: usize,
}

impl<'a> H264BitReader<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, bit_pos: 0 }
    }

    fn read_bits(&mut self, count: usize) -> Option<u32> {
        if count == 0 {
            return Some(0);
        }
        let mut value = 0u32;
        for _ in 0..count {
            let byte = *self.data.get(self.bit_pos / 8)?;
            let shift = 7 - (self.bit_pos % 8);
            value = (value << 1) | ((byte >> shift) & 1) as u32;
            self.bit_pos += 1;
        }
        Some(value)
    }

    fn skip(&mut self, count: usize) -> Option<()> {
        self.read_bits(count).map(|_| ())
    }

    fn read_exp_golomb(&mut self) -> Option<u32> {
        let mut leading_zero_bits = 0usize;
        while self.read_bits(1)? == 0 {
            leading_zero_bits += 1;
        }
        let suffix = if leading_zero_bits == 0 {
            0
        } else {
            self.read_bits(leading_zero_bits)?
        };
        Some(((1u32 << leading_zero_bits) - 1) + suffix)
    }
}

fn sample_entry_to_avcc_bytes(sample_entry: &SampleEntry) -> Option<Vec<u8>> {
    match sample_entry {
        SampleEntry::Avc1(avc1) => {
            let avcc = &avc1.avcc_box;
            let mut bytes = Vec::with_capacity(64);
            bytes.push(1);
            bytes.push(avcc.avc_profile_indication);
            bytes.push(avcc.profile_compatibility);
            bytes.push(avcc.avc_level_indication);
            bytes.push(0xFC | avcc.length_size_minus_one.get());
            bytes.push(0xE0 | avcc.sps_list.len() as u8);
            for sps in &avcc.sps_list {
                bytes.extend_from_slice(&(sps.len() as u16).to_be_bytes());
                bytes.extend_from_slice(sps);
            }
            bytes.push(avcc.pps_list.len() as u8);
            for pps in &avcc.pps_list {
                bytes.extend_from_slice(&(pps.len() as u16).to_be_bytes());
                bytes.extend_from_slice(pps);
            }
            Some(bytes)
        }
        _ => None,
    }
}

fn build_aac_sample_entry(track: &AudioMeta, audio_sequence_header: Option<&[u8]>) -> SampleEntry {
    let asc = audio_sequence_header
        .filter(|bytes| bytes.len() >= 4)
        .map(|bytes| bytes[2..4].to_vec())
        .unwrap_or_else(|| {
            build_aac_sequence_header(track.sample_rate, track.channels)
                .slice(2..4)
                .to_vec()
        });
    SampleEntry::Mp4a(Mp4aBox {
        audio: AudioSampleEntryFields {
            data_reference_index: AudioSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
            channelcount: track.channels.min(u16::MAX as u32) as u16,
            samplesize: AudioSampleEntryFields::DEFAULT_SAMPLESIZE,
            samplerate: FixedPointNumber::new(track.sample_rate.min(u16::MAX as u32) as u16, 0),
        },
        esds_box: EsdsBox {
            es: EsDescriptor {
                es_id: EsDescriptor::MIN_ES_ID,
                stream_priority: EsDescriptor::LOWEST_STREAM_PRIORITY,
                depends_on_es_id: None,
                url_string: None,
                ocr_es_id: None,
                dec_config_descr: DecoderConfigDescriptor {
                    object_type_indication:
                        DecoderConfigDescriptor::OBJECT_TYPE_INDICATION_AUDIO_ISO_IEC_14496_3,
                    stream_type: DecoderConfigDescriptor::STREAM_TYPE_AUDIO,
                    up_stream: DecoderConfigDescriptor::UP_STREAM_FALSE,
                    buffer_size_db: Uint::new(0),
                    max_bitrate: 0,
                    avg_bitrate: 0,
                    dec_specific_info: Some(DecoderSpecificInfo { payload: asc }),
                },
                sl_config_descr: SlConfigDescriptor,
            },
        },
        unknown_boxes: Vec::new(),
    })
}

fn default_video_duration(video: &VideoMeta) -> u32 {
    if video.fps.is_finite() && video.fps > 0.0 {
        ((VIDEO_TIMESCALE as f64 / video.fps).round() as u32).max(1)
    } else {
        3_000
    }
}

fn audio_default_duration(packet: &MediaPacket, sample_rate: u32) -> u32 {
    let frames = match packet.format {
        PayloadFormat::Flv => 1,
        PayloadFormat::Raw => {
            let count = adts_frame_count(&packet.payload);
            if count == 0 { 1 } else { count }
        }
    };
    let frame_samples = 1024u32;
    let duration = frame_samples.saturating_mul(frames as u32);
    duration.min(sample_rate.max(duration)).max(1)
}

fn rescale_ms(ms: i64, timescale: u32) -> i64 {
    ms.saturating_mul(timescale as i64) / 1000
}

fn segment_duration_secs(start_pts_ms: i64, end_pts_ms: i64) -> f64 {
    end_pts_ms.saturating_sub(start_pts_ms).max(1) as f64 / 1000.0
}

pub fn parse_fmp4_segment_name(segment: &str) -> Option<u64> {
    segment
        .strip_prefix("seg")
        .and_then(|segment| segment.strip_suffix(".m4s"))
        .and_then(|segment| segment.parse::<u64>().ok())
}

#[cfg(test)]
mod tests {
    use super::*;
    use loom::sync::{Arc as LoomArc, Mutex as LoomMutex};
    use loom::thread;
    use proptest::prelude::*;

    fn test_video_meta() -> VideoMeta {
        VideoMeta {
            codec: "h264".to_string(),
            width: 1920,
            height: 1080,
            fps: 30.0,
            bw: None,
            pid: None,
            language: None,
            title: None,
            profile: None,
            level: None,
            pixel_format: None,
        }
    }

    fn dummy_avc_sample_entry() -> SampleEntry {
        SampleEntry::Avc1(Avc1Box {
            visual: VisualSampleEntryFields {
                data_reference_index: VisualSampleEntryFields::DEFAULT_DATA_REFERENCE_INDEX,
                width: 1920,
                height: 1080,
                horizresolution: VisualSampleEntryFields::DEFAULT_HORIZRESOLUTION,
                vertresolution: VisualSampleEntryFields::DEFAULT_VERTRESOLUTION,
                frame_count: VisualSampleEntryFields::DEFAULT_FRAME_COUNT,
                compressorname: VisualSampleEntryFields::NULL_COMPRESSORNAME,
                depth: VisualSampleEntryFields::DEFAULT_DEPTH,
            },
            avcc_box: AvccBox {
                avc_profile_indication: 100,
                profile_compatibility: 0,
                avc_level_indication: 40,
                length_size_minus_one: Uint::new(3),
                sps_list: vec![vec![0x67, 0x64, 0x00, 0x28]],
                pps_list: vec![vec![0x68, 0xee, 0x3c, 0x80]],
                chroma_format: None,
                bit_depth_luma_minus8: None,
                bit_depth_chroma_minus8: None,
                sps_ext_list: Vec::new(),
            },
            unknown_boxes: Vec::new(),
        })
    }

    fn high_profile_sequence_header() -> Vec<u8> {
        vec![
            0x17, 0x00, 0x00, 0x00, 0x00, // FLV video header + AVC sequence header
            0x01, // configurationVersion
            0x64, // profile = High
            0x00, // profile compatibility
            0x1F, // level = 3.1
            0xFF, // lengthSizeMinusOne = 3
            0xE1, // num SPS = 1
            0x00, 0x19, // SPS length = 25
            0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB, 0x01, 0x10, 0x00, 0x00,
            0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x62, 0xE4,
            0x01, // num PPS = 1
            0x00, 0x04, // PPS length = 4
            0x68, 0xEE, 0x3C, 0x80,
        ]
    }

    fn high_profile_annexb_keyframe() -> Vec<u8> {
        vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x64, 0x00, 0x1F, 0xAC, 0xD9, 0x40, 0x50, 0x05, 0xBB,
            0x01, 0x10, 0x00, 0x00, 0x03, 0x00, 0x10, 0x00, 0x00, 0x03, 0x03, 0xC0, 0xF1, 0x62,
            0xE4, 0x00, 0x00, 0x00, 0x01, 0x68, 0xEE, 0x3C, 0x80, 0x00, 0x00, 0x00, 0x01, 0x65,
            0x88, 0x84, 0x00,
        ]
    }

    fn test_store() -> Fmp4HlsStore {
        Fmp4HlsStore::with_config(HlsConfig::default())
    }

    #[test]
    fn primary_playlist_points_at_fmp4_segments() {
        let store = test_store();
        store.put_video_init_segment(Bytes::from_static(b"init"));
        store.push_video_segment(0, 2.25, Bytes::from_static(b"segment-zero"));
        store.push_video_segment(1, 3.5, Bytes::from_static(b"segment-one"));

        let playlist = store.get_primary_playlist().expect("playlist");
        assert!(playlist.contains("#EXT-X-MAP:URI=\"video/init.mp4\""));
        assert!(playlist.contains("#EXTINF:2.250,\nseg0.m4s"));
        assert!(playlist.contains("#EXTINF:3.500,\nseg1.m4s"));
    }

    #[test]
    fn audio_playlist_uses_audio_relative_paths() {
        let store = test_store();
        store.set_stream_metadata(
            Some(test_video_meta()),
            vec![AudioMeta {
                codec: "aac".to_string(),
                sample_rate: 48_000,
                channels: 2,
                channel_layout: None,
                track_index: 15,
                pid: None,
                language: None,
                title: None,
                profile: None,
            }],
        );
        store.put_audio_init_segment(15, Bytes::from_static(b"init-audio"));
        store.push_audio_segment(15, 7, 2.0, Bytes::from_static(b"audio-segment"));

        let playlist = store.get_audio_playlist(15).expect("audio playlist");
        assert!(playlist.contains("#EXT-X-MAP:URI=\"init.mp4\""));
        assert!(playlist.contains("#EXTINF:2.000,\nseg7.m4s"));
    }

    #[test]
    fn parse_fmp4_segment_names() {
        assert_eq!(parse_fmp4_segment_name("seg42.m4s"), Some(42));
        assert_eq!(parse_fmp4_segment_name("seg42.ts"), None);
        assert_eq!(parse_fmp4_segment_name("init.mp4"), None);
    }

    #[test]
    fn publish_video_segment_makes_init_and_media_visible_together() {
        let store = test_store();
        store.publish_video_segment(
            0,
            2.0,
            Bytes::from_static(b"init"),
            Bytes::from_static(b"segment"),
        );

        assert!(store.get_video_init_segment().is_some());
        assert!(store.get_video_segment(0).is_some());
        assert!(
            store
                .get_video_playlist()
                .expect("playlist")
                .contains("#EXT-X-MAP:URI=\"init.mp4\"")
        );
    }

    #[test]
    fn high_profile_sequence_header_supports_init_segment_generation() {
        let sample_entry = build_h264_sample_entry_from_flv_sequence_header(
            &high_profile_sequence_header(),
            &test_video_meta(),
        )
        .expect("sample entry");
        let mut muxer = Fmp4SegmentMuxer::new().expect("muxer");
        let samples = vec![Sample {
            track_kind: TrackKind::Video,
            sample_entry: Some(sample_entry),
            keyframe: true,
            timescale: NonZeroU32::new(VIDEO_TIMESCALE).expect("timescale"),
            duration: 3_000,
            composition_time_offset: None,
            data_offset: 0,
            data_size: 4,
        }];
        let _ = muxer
            .create_media_segment_metadata(&samples)
            .expect("media metadata");
        let init = muxer.init_segment_bytes().expect("init segment");
        assert!(!init.is_empty());
    }

    #[test]
    fn packet_derived_h264_sample_entries_preserve_known_dimensions() {
        let packet = MediaPacket {
            media_type: MediaType::Video,
            payload: Bytes::from(high_profile_sequence_header()),
            is_keyframe: true,
            pts: 0,
            dts: 0,
            format: PayloadFormat::Flv,
            track_index: 0,
        };
        let sample_entry = build_h264_sample_entry_from_video_packet(&packet, &test_video_meta())
            .expect("flv sample entry");
        let SampleEntry::Avc1(avc1) = sample_entry else {
            panic!("expected avc1 sample entry");
        };
        assert_eq!(avc1.visual.width, 1920);
        assert_eq!(avc1.visual.height, 1080);

        let raw_packet = MediaPacket {
            format: PayloadFormat::Raw,
            payload: Bytes::from(high_profile_annexb_keyframe()),
            ..packet
        };
        let raw_sample_entry =
            build_h264_sample_entry_from_video_packet(&raw_packet, &test_video_meta())
                .expect("raw sample entry");
        let SampleEntry::Avc1(raw_avc1) = raw_sample_entry else {
            panic!("expected raw avc1 sample entry");
        };
        assert_eq!(raw_avc1.visual.width, 1920);
        assert_eq!(raw_avc1.visual.height, 1080);

        let mut muxer = Fmp4SegmentMuxer::new().expect("muxer");
        let samples = vec![Sample {
            track_kind: TrackKind::Video,
            sample_entry: Some(SampleEntry::Avc1(raw_avc1)),
            keyframe: true,
            timescale: NonZeroU32::new(VIDEO_TIMESCALE).expect("timescale"),
            duration: 3_000,
            composition_time_offset: None,
            data_offset: 0,
            data_size: 4,
        }];
        let _ = muxer
            .create_media_segment_metadata(&samples)
            .expect("media metadata");
        let init = muxer.init_segment_bytes().expect("init segment");
        assert!(!init.is_empty());
    }

    #[test]
    fn loom_publish_model_never_exposes_segment_without_init() {
        loom::model(|| {
            #[derive(Default)]
            struct ModelState {
                init_visible: bool,
                segment_visible: bool,
            }

            let state = LoomArc::new(LoomMutex::new(ModelState::default()));

            let publisher_state = state.clone();
            let publisher = thread::spawn(move || {
                let mut guard = publisher_state.lock().expect("publisher lock");
                guard.init_visible = true;
                guard.segment_visible = true;
            });

            let reader_state = state.clone();
            let reader = thread::spawn(move || {
                let guard = reader_state.lock().expect("reader lock");
                if guard.segment_visible {
                    assert!(guard.init_visible);
                }
            });

            publisher.join().expect("publisher join");
            reader.join().expect("reader join");
        });
    }

    #[test]
    fn build_mux_samples_rejects_out_of_range_composition_offsets() {
        let result = build_mux_samples(
            &[BufferedSample {
                pts: i32::MAX as i64 + 10,
                dts: 0,
                keyframe: true,
                data_offset: 0,
                data_size: 4,
                default_duration: 3_000,
            }],
            TrackKind::Video,
            VIDEO_TIMESCALE,
            dummy_avc_sample_entry(),
            Some(i32::MAX as i64 + 20),
        );
        assert!(result.is_err());
    }

    proptest! {
        #[test]
        fn proptest_parse_segment_name_round_trips(index in 0u64..1_000_000) {
            let name = format!("seg{index}.m4s");
            prop_assert_eq!(parse_fmp4_segment_name(&name), Some(index));
        }

        #[test]
        fn proptest_store_window_tracks_last_segments(
            max_segments in 1usize..8,
            durations in proptest::collection::vec(1u16..8u16, 1..24),
        ) {
            let store = Fmp4HlsStore::with_config(HlsConfig {
                max_segments,
                ..HlsConfig::default()
            });
            store.put_video_init_segment(Bytes::from_static(b"init"));

            for (index, duration) in durations.iter().copied().enumerate() {
                store.push_video_segment(index as u64, duration as f64, Bytes::from(index.to_string()));
            }

            let expected = durations.len().min(max_segments);
            prop_assert_eq!(store.segment_count(), expected);

            let playlist = store.get_primary_playlist().expect("playlist must exist");
            let expected_first = durations.len().saturating_sub(expected) as u64;
            let expected_seq_line = format!("#EXT-X-MEDIA-SEQUENCE:{expected_first}");
            prop_assert!(playlist.contains(&expected_seq_line));
            let expected_last = durations.len() as u64 - 1;
            let expected_segment_name = format!("seg{expected_last}.m4s");
            prop_assert!(playlist.contains(&expected_segment_name));
        }

        #[test]
        fn proptest_build_mux_samples_preserves_duration_and_cto(
            durations in proptest::collection::vec(1u32..5_000u32, 1..16),
            ctos in proptest::collection::vec(-200i32..200i32, 1..16),
            tail in 1u32..5_000u32,
        ) {
            let len = durations.len().min(ctos.len());
            let mut dts = 0i64;
            let mut buffered = Vec::with_capacity(len);
            for index in 0..len {
                let duration = durations[index];
                let cto = ctos[index] as i64;
                buffered.push(BufferedSample {
                    pts: dts + cto,
                    dts,
                    keyframe: index == 0,
                    data_offset: index as u64 * 10,
                    data_size: 10,
                    default_duration: duration,
                });
                dts += duration as i64;
            }
            let next_segment_first_dts = buffered.last().map(|last| last.dts + tail as i64);
            let samples = build_mux_samples(
                &buffered,
                TrackKind::Video,
                VIDEO_TIMESCALE,
                dummy_avc_sample_entry(),
                next_segment_first_dts,
            ).expect("samples should build");

            prop_assert_eq!(samples.len(), len);
            for (index, sample) in samples.iter().enumerate() {
                let expected_duration = if index + 1 < len {
                    buffered[index + 1].dts - buffered[index].dts
                } else {
                    next_segment_first_dts.expect("tail dts") - buffered[index].dts
                };
                prop_assert_eq!(sample.duration, expected_duration as u32);
                let expected_cto = buffered[index].pts - buffered[index].dts;
                if expected_cto == 0 {
                    prop_assert_eq!(sample.composition_time_offset, None);
                } else {
                    prop_assert_eq!(sample.composition_time_offset, Some(expected_cto));
                }
            }
        }
    }
}
