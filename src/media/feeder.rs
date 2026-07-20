//! Shared packet feeder primitives for TS-producing stages.
//!
//! Recording, HLS, and transcoder stdin stages all perform the same packet
//! work: convert payloads into TS-ready elementary stream bytes, map media
//! packets to muxer stream indexes, enforce monotonic DTS, and append MPEG-TS
//! packets to a sink. Keeping that logic here gives stage code a smaller
//! surface area: read bursts, feed packets, flush bytes.

use std::sync::Arc;

use crate::media::codec::{audio_for_ts_into, video_for_ts_into};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::mpegts::{TsMuxer, TsServiceMetadata};
use crate::media::packet::{MediaPacket, MediaType};
use crate::media::ring_buffer::DtsEnforcer;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeedAction {
    Continue,
    Stop,
}

pub trait FeedSink {
    fn on_ts_bytes(&mut self, bytes: &[u8]) -> FeedAction;
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrackPolicy {
    All,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FeedWriteMode {
    Batch,
}

#[derive(Debug, Clone)]
pub struct PacketFeedConfig {
    pub track_policy: TrackPolicy,
    pub write_mode: FeedWriteMode,
    pub video_sequence_header: Option<Vec<u8>>,
    pub raw_video_parameter_sets: Option<Vec<u8>>,
    pub service_metadata: Option<TsServiceMetadata>,
}

impl Default for PacketFeedConfig {
    fn default() -> Self {
        Self {
            track_policy: TrackPolicy::All,
            write_mode: FeedWriteMode::Batch,
            video_sequence_header: None,
            raw_video_parameter_sets: None,
            service_metadata: None,
        }
    }
}

pub struct TsPacketFeeder {
    muxer: TsMuxer,
    dts_enforcer: DtsEnforcer,
    audio_tracks: Arc<Vec<AudioMeta>>,
    audio_track_indices: Vec<u32>,
    audio_sample_rates: Vec<u32>,
    audio_channels: Vec<u32>,
    has_video: bool,
    waiting_for_video_startup: bool,
    nalu_len_size: usize,
    sps_pps_cache: Vec<u8>,
    video_conv_buf: Vec<u8>,
    audio_conv_buf: Vec<u8>,
}

impl TsPacketFeeder {
    pub fn new(
        video: Option<&VideoMeta>,
        audio_tracks: Arc<Vec<AudioMeta>>,
        config: PacketFeedConfig,
    ) -> Self {
        let (nalu_len_size, mut sps_pps_cache) = config
            .video_sequence_header
            .as_deref()
            .map(parse_video_sequence_header)
            .unwrap_or((4, Vec::new()));
        if sps_pps_cache.is_empty()
            && let Some(parameter_sets) = config.raw_video_parameter_sets
        {
            sps_pps_cache = parameter_sets;
        }
        let num_streams = video.is_some() as usize + audio_tracks.len();
        let service_metadata = config
            .service_metadata
            .unwrap_or_else(TsServiceMetadata::disabled);
        let audio_track_indices = audio_tracks.iter().map(|track| track.track_index).collect();
        let audio_sample_rates = audio_tracks.iter().map(|track| track.sample_rate).collect();
        let audio_channels = audio_tracks.iter().map(|track| track.channels).collect();

        Self {
            muxer: TsMuxer::new_with_metadata(video, &audio_tracks, service_metadata),
            dts_enforcer: DtsEnforcer::new(num_streams),
            audio_tracks,
            audio_track_indices,
            audio_sample_rates,
            audio_channels,
            has_video: video.is_some(),
            waiting_for_video_startup: video.is_some(),
            nalu_len_size,
            sps_pps_cache,
            video_conv_buf: Vec::new(),
            audio_conv_buf: Vec::new(),
        }
    }

    pub fn audio_tracks(&self) -> &Arc<Vec<AudioMeta>> {
        &self.audio_tracks
    }

    pub fn needs_raw_video_parameter_sets(&self) -> bool {
        self.has_video && self.sps_pps_cache.is_empty()
    }

    pub fn set_raw_video_parameter_sets_if_empty(&mut self, parameter_sets: &[u8]) -> bool {
        if !self.needs_raw_video_parameter_sets() || parameter_sets.is_empty() {
            return false;
        }
        self.sps_pps_cache.extend_from_slice(parameter_sets);
        true
    }

    /// Parse an FLV/RTMP video sequence header (AVCC format) and use the
    /// resulting SPS/PPS bytes and NALU-length-size to prime the feeder.
    ///
    /// This is the header stored by the RTMP handler in the engine's ingest
    /// state (`cache_sequence_header`).  It must be called before processing
    /// the first FLV-format video packet so that `video_for_ts_into` has the
    /// SPS/PPS needed to build a correct Annex-B frame.
    ///
    /// Safe to call repeatedly: if the cache is already non-empty this is a
    /// no-op, matching the "if empty" semantics of
    /// `set_raw_video_parameter_sets_if_empty`.
    pub fn set_video_sequence_header_from_avcc(&mut self, flv_sequence_header: &[u8]) {
        if !self.needs_raw_video_parameter_sets() {
            return;
        }
        let (nalu_len_size, sps_pps) = parse_video_sequence_header(flv_sequence_header);
        if !sps_pps.is_empty() {
            self.nalu_len_size = nalu_len_size;
            self.sps_pps_cache = sps_pps;
        }
    }

    pub fn extend_ts_for_packet(&mut self, packet: &MediaPacket, output: &mut Vec<u8>) -> bool {
        let (payload, stream_idx) = match packet.media_type {
            MediaType::Video => {
                if self.waiting_for_video_startup {
                    match prepare_video_startup_packet(packet, &mut self.sps_pps_cache) {
                        VideoStartupAction::Skip => return false,
                        VideoStartupAction::Emit => {}
                    }
                }
                match video_for_ts_into(
                    &packet.payload,
                    packet.format,
                    &mut self.nalu_len_size,
                    &mut self.sps_pps_cache,
                    &mut self.video_conv_buf,
                ) {
                    Some(payload) => {
                        self.waiting_for_video_startup = false;
                        (payload, 0)
                    }
                    None => return false,
                }
            }
            MediaType::Audio => {
                if self.has_video && self.waiting_for_video_startup {
                    return false;
                }
                let Some(track_index) = self
                    .audio_track_indices
                    .iter()
                    .position(|&track_index| track_index == packet.track_index)
                else {
                    return false;
                };
                match audio_for_ts_into(
                    &packet.payload,
                    packet.format,
                    self.audio_sample_rates[track_index],
                    self.audio_channels[track_index],
                    &mut self.audio_conv_buf,
                ) {
                    Some(payload) => (payload, track_index + self.has_video as usize),
                    None => return false,
                }
            }
        };

        let (pts, dts) = self
            .dts_enforcer
            .enforce(stream_idx, packet.pts, packet.dts);
        // `stream_idx` above is already the muxer's stream index (video is
        // always 0 when present; audio tracks are pushed onto both
        // `audio_track_indices` and the muxer's `streams` in the same order
        // from the same source slice at construction), so skip mux_packet's
        // redundant linear (media_type, track_index) scan.
        let ts_bytes = self.muxer.mux_packet_by_stream_idx(
            stream_idx,
            packet.media_type,
            pts,
            dts,
            packet.is_keyframe,
            payload,
        );
        if ts_bytes.is_empty() {
            return false;
        }
        output.extend_from_slice(ts_bytes);
        true
    }
}

fn parse_video_sequence_header(flv_sequence_header: &[u8]) -> (usize, Vec<u8>) {
    if flv_sequence_header.len() > 5 {
        crate::media::codec::parse_avcc_config(&flv_sequence_header[5..])
    } else {
        (4, Vec::new())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoStartupAction {
    Skip,
    Emit,
}

fn prepare_video_startup_packet(
    packet: &MediaPacket,
    sps_pps_cache: &mut Vec<u8>,
) -> VideoStartupAction {
    match packet.format {
        crate::media::packet::PayloadFormat::Raw => {
            if let Some(parameter_sets) =
                crate::media::codec::annexb_parameter_sets(&packet.payload)
            {
                *sps_pps_cache = parameter_sets;
            }
            if crate::media::codec::raw_annexb_is_keyframe(&packet.payload) {
                VideoStartupAction::Emit
            } else {
                VideoStartupAction::Skip
            }
        }
        crate::media::packet::PayloadFormat::Flv => {
            if (packet.payload.len() > 1 && packet.payload[1] == 0) || packet.is_keyframe {
                VideoStartupAction::Emit
            } else {
                VideoStartupAction::Skip
            }
        }
    }
}

#[cfg(test)]
#[path = "feeder_parameter_set_behavior_tests.rs"]
mod parameter_set_behavior_tests;
#[cfg(test)]
#[path = "feeder_startup_gating_tests.rs"]
mod startup_gating_tests;
#[cfg(test)]
#[path = "feeder_transport_remux_tests.rs"]
mod transport_remux_tests;
