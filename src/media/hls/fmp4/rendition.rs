use bytes::Bytes;
use shiguredo_mp4::{TrackKind, boxes::SampleEntry, mux::Fmp4SegmentMuxer};

use super::codec::{
    VIDEO_TIMESCALE, audio_default_duration, build_aac_sample_entry,
    build_h264_sample_entry_from_flv_sequence_header, build_h264_sample_entry_from_video_packet,
    build_mux_samples, default_video_duration, rescale_ms, sample_entry_to_avcc_bytes,
};
use super::store::Fmp4HlsStore;
use crate::media::codec::{annexb_to_avcc_into, strip_adts};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::{MediaPacket, PayloadFormat};

pub(super) struct BufferedSample {
    pub(super) pts: i64,
    pub(super) dts: i64,
    pub(super) keyframe: bool,
    pub(super) data_offset: u64,
    pub(super) data_size: usize,
    pub(super) default_duration: u32,
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

pub(super) struct VideoRenditionState {
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
    pub(super) fn new(video: &VideoMeta, video_sequence_header: Option<&[u8]>) -> Self {
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

    pub(super) fn push_packet(&mut self, packet: &MediaPacket, zero_ms: i64) -> Result<(), String> {
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

    pub(super) fn flush_segment(
        &mut self,
        store: &Fmp4HlsStore,
        index: u64,
        duration_secs: f64,
        next_segment_first_relative_dts_ms: Option<i64>,
    ) -> Result<(), String> {
        if self.samples.is_empty() {
            self.current_segment_start_ms = None;
            self.payload.clear();
            return Ok(());
        }
        let Some(sample_entry) = self.sample_entry.clone() else {
            return Err("missing avc1 sample entry".to_string());
        };
        let next_dts =
            next_segment_first_relative_dts_ms.map(|dts_ms| rescale_ms(dts_ms, VIDEO_TIMESCALE));
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

    pub(super) fn current_segment_duration_secs(&self) -> f64 {
        self.current_segment_start_ms
            .zip(self.samples.last())
            .map(|(start_ms, last)| ((last.pts / 90) as f64 - start_ms as f64).max(1.0) / 1000.0)
            .unwrap_or(1.0)
    }
}

pub(super) struct AudioRenditionState {
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
    pub(super) fn new(track: &AudioMeta, audio_sequence_header: Option<&[u8]>) -> Self {
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

    pub(super) fn push_packet(&mut self, packet: &MediaPacket, zero_ms: i64) -> Result<(), String> {
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

    pub(super) fn flush_segment(
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

    pub(super) fn current_segment_duration_secs(&self) -> f64 {
        self.current_segment_start_ms
            .zip(self.samples.last())
            .map(|(start_ms, last)| {
                let last_ms = ((last.pts as f64) * 1000.0 / self.sample_rate as f64).round() as i64;
                (last_ms.saturating_sub(start_ms)).max(1) as f64 / 1000.0
            })
            .unwrap_or(1.0)
    }
}
