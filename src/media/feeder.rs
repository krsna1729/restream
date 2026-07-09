//! Shared packet feeder primitives for TS-producing stages.
//!
//! Recording, HLS, and transcoder stdin stages all perform the same packet
//! work: convert payloads into TS-ready elementary stream bytes, map media
//! packets to muxer stream indexes, enforce monotonic DTS, and append MPEG-TS
//! packets to a sink. Keeping that logic here gives stage code a smaller
//! surface area: read bursts, feed packets, flush bytes.

use std::sync::Arc;

use crate::media::codec::{audio_for_ts_into, video_for_ts_into};
use crate::media::engine::{AudioMeta, VideoMeta};
use crate::media::mpegts::{TsMuxer, TsServiceMetadata};
use crate::media::ring_buffer::{DtsEnforcer, MediaPacket, MediaType};

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
        crate::media::ring_buffer::PayloadFormat::Raw => {
            if let Some(parameter_sets) =
                crate::media::codec::annexb_parameter_sets(&packet.payload)
            {
                *sps_pps_cache = parameter_sets;
            }
            if packet.is_keyframe {
                VideoStartupAction::Emit
            } else {
                VideoStartupAction::Skip
            }
        }
        crate::media::ring_buffer::PayloadFormat::Flv => {
            if (packet.payload.len() > 1 && packet.payload[1] == 0) || packet.is_keyframe {
                VideoStartupAction::Emit
            } else {
                VideoStartupAction::Skip
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;

    fn audio_track(index: u32) -> AudioMeta {
        AudioMeta {
            codec: "aac".to_string(),
            sample_rate: 48_000,
            channels: 2,
            channel_layout: None,
            track_index: index,
            pid: None,
            language: None,
            title: None,
            profile: None,
        }
    }

    fn video_meta() -> VideoMeta {
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

    #[test]
    fn feeder_skips_unknown_audio_track_to_protect_dts_state() {
        let audio_tracks = Arc::new(vec![audio_track(0)]);
        let mut feeder = TsPacketFeeder::new(None, audio_tracks, PacketFeedConfig::default());
        let packet = MediaPacket {
            media_type: MediaType::Audio,
            format: crate::media::ring_buffer::PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 7,
            pts: 0,
            dts: 0,
            payload: Bytes::from_static(&[0x00]),
        };
        let mut output = Vec::new();

        assert!(!feeder.extend_ts_for_packet(&packet, &mut output));
        assert!(output.is_empty());
    }

    #[test]
    fn feeder_holds_audio_until_video_startup_packet() {
        let video = video_meta();
        let audio_tracks = Arc::new(vec![audio_track(0)]);
        let mut feeder = TsPacketFeeder::new(
            Some(&video),
            audio_tracks,
            PacketFeedConfig {
                raw_video_parameter_sets: Some(vec![
                    0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01,
                    0x68, 0xCE, 0x38, 0x80,
                ]),
                ..PacketFeedConfig::default()
            },
        );
        let audio = MediaPacket {
            media_type: MediaType::Audio,
            format: crate::media::ring_buffer::PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 0,
            pts: 0,
            dts: 0,
            payload: Bytes::from_static(&[0x11; 32]),
        };
        let keyframe = MediaPacket {
            media_type: MediaType::Video,
            format: crate::media::ring_buffer::PayloadFormat::Raw,
            is_keyframe: true,
            track_index: 0,
            pts: 33,
            dts: 33,
            payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80]),
        };
        let mut output = Vec::new();

        assert!(
            !feeder.extend_ts_for_packet(&audio, &mut output),
            "audio must not lead the TS stream before video startup"
        );
        assert!(output.is_empty());
        assert!(
            feeder.extend_ts_for_packet(&keyframe, &mut output),
            "first keyframe should unlock video startup"
        );
        assert!(!output.is_empty());
    }

    #[test]
    fn feeder_matches_manual_codec_mux_and_dts_path() {
        use crate::media::codec::{audio_for_ts_into, video_for_ts_into};
        use crate::media::mpegts::TsMuxer;
        use crate::media::ring_buffer::{DtsEnforcer, PayloadFormat};

        let video = video_meta();
        let audio_tracks = Arc::new(vec![audio_track(0), audio_track(1)]);
        let packets = vec![
            MediaPacket {
                media_type: MediaType::Video,
                format: PayloadFormat::Raw,
                is_keyframe: true,
                track_index: 0,
                pts: 0,
                dts: 0,
                payload: Bytes::from_static(&[
                    0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x84, 0x21, 0xA0,
                ]),
            },
            MediaPacket {
                media_type: MediaType::Audio,
                format: PayloadFormat::Raw,
                is_keyframe: false,
                track_index: 0,
                pts: 20,
                dts: 20,
                payload: Bytes::from_static(&[0x11; 32]),
            },
            MediaPacket {
                media_type: MediaType::Audio,
                format: PayloadFormat::Raw,
                is_keyframe: false,
                track_index: 1,
                pts: 21,
                dts: 21,
                payload: Bytes::from_static(&[0x22; 24]),
            },
        ];

        let mut feeder = TsPacketFeeder::new(
            Some(&video),
            audio_tracks.clone(),
            PacketFeedConfig::default(),
        );
        let mut feeder_output = Vec::new();
        for packet in &packets {
            assert!(feeder.extend_ts_for_packet(packet, &mut feeder_output));
        }

        let mut muxer = TsMuxer::new(Some(&video), &audio_tracks);
        let mut dts = DtsEnforcer::new(1 + audio_tracks.len());
        let mut video_conv_buf = Vec::new();
        let mut audio_conv_buf = Vec::new();
        let mut nalu_len_size = 4usize;
        let mut sps_pps_cache = Vec::new();
        let mut manual_output = Vec::new();

        for packet in &packets {
            let payload = match packet.media_type {
                MediaType::Video => video_for_ts_into(
                    &packet.payload,
                    packet.format,
                    &mut nalu_len_size,
                    &mut sps_pps_cache,
                    &mut video_conv_buf,
                )
                .expect("video packet should convert"),
                MediaType::Audio => {
                    let track = audio_tracks
                        .iter()
                        .find(|a| a.track_index == packet.track_index)
                        .expect("test packet track exists");
                    audio_for_ts_into(
                        &packet.payload,
                        packet.format,
                        track.sample_rate,
                        track.channels,
                        &mut audio_conv_buf,
                    )
                    .expect("audio packet should convert")
                }
            };
            let stream_idx = match packet.media_type {
                MediaType::Video => 0,
                MediaType::Audio => {
                    1 + audio_tracks
                        .iter()
                        .position(|a| a.track_index == packet.track_index)
                        .expect("test packet track exists")
                }
            };
            let (pts, dts_value) = dts.enforce(stream_idx, packet.pts, packet.dts);
            manual_output.extend_from_slice(muxer.mux_packet(
                packet.media_type,
                packet.track_index,
                pts,
                dts_value,
                packet.is_keyframe,
                payload,
            ));
        }

        assert_eq!(feeder_output, manual_output);
    }

    #[test]
    fn feeder_remuxed_h265_multi_audio_fixture_decodes_cleanly() {
        let fixture = crate::test_fixtures::bench_transport_fixture("h265", "1.5M", true)
            .expect("fixture must exist");
        let input = std::fs::read(&fixture).expect("fixture must be readable");
        let mut demuxer = crate::media::mpegts::TsDemuxer::new();
        let mut packets = Vec::new();

        for chunk in input.chunks(1316) {
            demuxer.feed(chunk);
            demuxer.drain_into(&mut packets);
        }
        demuxer.flush();
        demuxer.drain_into(&mut packets);

        let probe = demuxer.take_probe().expect("fixture must probe");
        let video = probe.video.expect("fixture must have video");
        let audio_tracks = Arc::new(probe.audio_tracks);
        assert_eq!(
            audio_tracks.len(),
            2,
            "regression fixture must keep both audio tracks"
        );

        let mut feeder =
            TsPacketFeeder::new(Some(&video), audio_tracks, PacketFeedConfig::default());
        let mut output = Vec::new();
        for packet in &packets {
            feeder.extend_ts_for_packet(packet, &mut output);
        }
        assert!(
            !output.is_empty(),
            "remuxed fixture must produce MPEG-TS bytes"
        );

        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("clock must be valid")
            .as_nanos();
        let output_path = std::env::temp_dir().join(format!(
            "restream-h265-multi-remux-{}-{unique}.ts",
            std::process::id()
        ));
        std::fs::write(&output_path, &output).expect("remuxed fixture must be writable");

        let ffmpeg = std::process::Command::new("ffmpeg")
            .args(["-nostdin", "-hide_banner", "-v", "warning", "-i"])
            .arg(&output_path)
            .args(["-t", "5", "-map", "0", "-f", "null", "-"])
            .output()
            .expect("ffmpeg must run");
        let _ = std::fs::remove_file(&output_path);
        let stderr = String::from_utf8_lossy(&ffmpeg.stderr);
        let stderr_lower = stderr.to_ascii_lowercase();

        assert!(
            ffmpeg.status.success(),
            "ffmpeg should decode remuxed fixture: {stderr}"
        );
        assert!(
            !stderr_lower.contains("non monoton"),
            "remuxed fixture must not contain duplicate DTS: {stderr}"
        );
    }

    #[test]
    fn feeder_seeds_raw_h265_parameter_sets_for_late_joining_keyframes() {
        let video = VideoMeta {
            codec: "hevc".to_string(),
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
        };
        let parameter_sets = vec![
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        ];
        let mut feeder = TsPacketFeeder::new(
            Some(&video),
            Arc::new(Vec::new()),
            PacketFeedConfig {
                raw_video_parameter_sets: Some(parameter_sets.clone()),
                ..PacketFeedConfig::default()
            },
        );
        let packet = MediaPacket {
            media_type: MediaType::Video,
            format: crate::media::ring_buffer::PayloadFormat::Raw,
            is_keyframe: true,
            track_index: 0,
            pts: 0,
            dts: 0,
            payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xDD]),
        };

        let mut output = Vec::new();
        assert!(feeder.extend_ts_for_packet(&packet, &mut output));

        let mut demuxer = crate::media::mpegts::TsDemuxer::new();
        let mut remuxed_packets = Vec::new();
        for chunk in output.chunks(188) {
            demuxer.feed(chunk);
            demuxer.drain_into(&mut remuxed_packets);
        }
        demuxer.flush();
        demuxer.drain_into(&mut remuxed_packets);

        let remuxed = remuxed_packets
            .iter()
            .find(|packet| packet.media_type == MediaType::Video)
            .expect("remuxed output should contain video");
        assert!(remuxed.payload.starts_with(&parameter_sets));
        assert!(remuxed.payload.ends_with(&packet.payload));
    }

    #[test]
    fn feeder_skips_pre_keyframe_raw_video_until_clean_startup_boundary() {
        let video = VideoMeta {
            codec: "hevc".to_string(),
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
        };
        let parameter_sets = vec![
            0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
            0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
        ];
        let mut feeder = TsPacketFeeder::new(
            Some(&video),
            Arc::new(Vec::new()),
            PacketFeedConfig {
                raw_video_parameter_sets: Some(parameter_sets),
                ..PacketFeedConfig::default()
            },
        );
        let pre_keyframe = MediaPacket {
            media_type: MediaType::Video,
            format: crate::media::ring_buffer::PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 0,
            pts: 33,
            dts: 33,
            payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0xDD]),
        };
        let keyframe = MediaPacket {
            media_type: MediaType::Video,
            format: crate::media::ring_buffer::PayloadFormat::Raw,
            is_keyframe: true,
            track_index: 0,
            pts: 66,
            dts: 66,
            payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xEE]),
        };

        let mut output = Vec::new();
        assert!(!feeder.extend_ts_for_packet(&pre_keyframe, &mut output));
        assert!(output.is_empty());
        assert!(feeder.extend_ts_for_packet(&keyframe, &mut output));
        assert!(
            !output.is_empty(),
            "once the first keyframe arrives, the feeder should start emitting TS"
        );
    }

    #[test]
    fn feeder_keeps_waiting_after_parameter_sets_only_raw_packet() {
        let video = VideoMeta {
            codec: "hevc".to_string(),
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
        };
        let mut feeder = TsPacketFeeder::new(
            Some(&video),
            Arc::new(Vec::new()),
            PacketFeedConfig::default(),
        );
        let parameter_sets_only = MediaPacket {
            media_type: MediaType::Video,
            format: crate::media::ring_buffer::PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 0,
            pts: 0,
            dts: 0,
            payload: Bytes::from_static(&[
                0x00, 0x00, 0x00, 0x01, 0x40, 0x01, 0xAA, 0x00, 0x00, 0x00, 0x01, 0x42, 0x01, 0xBB,
                0x00, 0x00, 0x00, 0x01, 0x44, 0x01, 0xCC,
            ]),
        };
        let delta = MediaPacket {
            media_type: MediaType::Video,
            format: crate::media::ring_buffer::PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 0,
            pts: 33,
            dts: 33,
            payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x02, 0x01, 0xDD]),
        };
        let keyframe = MediaPacket {
            media_type: MediaType::Video,
            format: crate::media::ring_buffer::PayloadFormat::Raw,
            is_keyframe: true,
            track_index: 0,
            pts: 66,
            dts: 66,
            payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x26, 0x01, 0xEE]),
        };

        let mut output = Vec::new();
        assert!(
            !feeder.extend_ts_for_packet(&parameter_sets_only, &mut output),
            "parameter sets alone should prime the cache but not unlock startup"
        );
        assert!(!feeder.extend_ts_for_packet(&delta, &mut output));
        assert!(
            output.is_empty(),
            "delta frames must stay suppressed until a true random-access frame arrives"
        );
        assert!(feeder.extend_ts_for_packet(&keyframe, &mut output));
        assert!(!output.is_empty());
    }
}
