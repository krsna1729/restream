//! Canonical fixture contract for tests and benchmarks.
//!
//! Non-integration fixtures live in git under `test/fixtures/transport/`. Integration
//! media that exercises the public media-library path stays under
//! `test/fixtures/media-library/`.
//! Tests and benches should resolve them through this module so fixture drift
//! fails loudly instead of silently depending on whatever happens to be local.

use bytes::Bytes;
use std::path::PathBuf;
use std::sync::Arc;

use crate::media::codec::{
    audio_for_rtmp_into, build_aac_sequence_header, build_avcc_sequence_header, split_annexb_nalus,
    video_for_rtmp_into,
};
use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::mpegts::TsDemuxer;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AvMarkerBframeMode {
    Bf0,
    Bf2,
}

impl AvMarkerBframeMode {
    const fn file_suffix(self) -> &'static str {
        match self {
            Self::Bf0 => "-bf0",
            Self::Bf2 => "",
        }
    }
}

pub const REQUIRED_CHECKED_IN_FIXTURES: &[&str] = &[
    // Canonical H.264 MPEG-TS correctness source: single-video/single-audio
    // packet fixture used by unit tests and non-measurement protocol gates.
    "test/fixtures/transport/correctness-h264.ts",
    // Canonical HEVC MPEG-TS correctness source: exercises HEVC demux,
    // passthrough, and H.265->H.264 compatibility conversion paths.
    "test/fixtures/transport/correctness-h265.ts",
    // H.264 1.5 Mbps single-audio transport fixture for resource/throughput
    // sweeps where bitrate shape matters more than signal markers.
    "test/fixtures/transport/bench-h264-1_5m.ts",
    // H.264 4 Mbps single-audio transport fixture for medium-bitrate scaling
    // and ramp measurements.
    "test/fixtures/transport/bench-h264-4m.ts",
    // H.264 8 Mbps single-audio transport fixture for high-bitrate scaling
    // and buffer-pressure measurements.
    "test/fixtures/transport/bench-h264-8m.ts",
    // H.264 1.5 Mbps two-audio transport fixture for multi-audio resource
    // sweeps and selected-track routing without synthetic generation.
    "test/fixtures/transport/bench-h264-1_5m-2a.ts",
    // H.264 4 Mbps two-audio transport fixture for multi-audio bitrate-sweep
    // coverage at the medium-bitrate point.
    "test/fixtures/transport/bench-h264-4m-2a.ts",
    // H.264 8 Mbps two-audio transport fixture for multi-audio bitrate-sweep
    // coverage at the high-bitrate point.
    "test/fixtures/transport/bench-h264-8m-2a.ts",
    // HEVC 1.5 Mbps single-audio transport fixture for low-bitrate HEVC
    // scaling and RTMP compatibility-edge measurements.
    "test/fixtures/transport/bench-h265-1_5m.ts",
    // HEVC 4 Mbps single-audio transport fixture for medium-bitrate HEVC
    // scaling and codec-edge resource measurements.
    "test/fixtures/transport/bench-h265-4m.ts",
    // HEVC 8 Mbps single-audio transport fixture for high-bitrate HEVC
    // scaling and stress of the in-process H.265->H.264 path.
    "test/fixtures/transport/bench-h265-8m.ts",
    // HEVC 1.5 Mbps two-audio transport fixture for multi-audio HEVC
    // resource sweeps and selected-track routing.
    "test/fixtures/transport/bench-h265-1_5m-2a.ts",
    // HEVC 4 Mbps two-audio transport fixture for multi-audio bitrate-sweep
    // coverage at the medium-bitrate point.
    "test/fixtures/transport/bench-h265-4m-2a.ts",
    // HEVC 8 Mbps two-audio transport fixture for multi-audio bitrate-sweep
    // coverage at the high-bitrate point.
    "test/fixtures/transport/bench-h265-8m-2a.ts",
    // H.264 marker oracle: black video with periodic white flashes and
    // matching 1 kHz beeps for A/V sync, drift, and PCM-quality assertions.
    "test/fixtures/transport/av-marker-h264.ts",
    // HEVC marker oracle with the same flash/beep timing as the H.264 marker
    // fixture, used to verify signal quality through HEVC paths.
    "test/fixtures/transport/av-marker-h265.ts",
    // H.264 marker oracle with two AAC tracks: track 0 has 1 kHz beeps and
    // track 1 has 2 kHz beeps for multi-audio routing and future frequency
    // identity checks.
    "test/fixtures/transport/av-marker-h264-2a.ts",
    // H.264 marker oracle with B-frames disabled so harnesses can verify
    // source-side no-reorder ingest against the same flash/beep timing.
    "test/fixtures/transport/av-marker-h264-bf0.ts",
    // H.264 two-audio marker oracle with B-frames disabled for multi-audio
    // source-reorder matrix coverage.
    "test/fixtures/transport/av-marker-h264-bf0-2a.ts",
    // HEVC marker oracle with two AAC tracks, covering HEVC multi-audio
    // routing plus RTMP compatibility conversion under signal validation.
    "test/fixtures/transport/av-marker-h265-2a.ts",
    // HEVC marker oracle with B-frames disabled so mixed input can cover
    // HEVC no-reorder ingest without dropping A/V marker signal.
    "test/fixtures/transport/av-marker-h265-bf0.ts",
    // HEVC two-audio marker oracle with B-frames disabled for multi-audio
    // no-reorder ingest coverage.
    "test/fixtures/transport/av-marker-h265-bf0-2a.ts",
    // HLS edge-case fixture whose first segment starts audio-only, guarding
    // preview and playlist startup behavior when video arrives later.
    "test/fixtures/transport/hls-first-audio-only-6s.ts",
    // Sparse-GOP MP4 fixture for file-live-edge optimization tests that verify
    // GOP normalization and recording duration behavior.
    "test/fixtures/media-library/sparse-gop-5s.mp4",
    // Public media-library fixture with two video tracks and sixteen audio
    // tracks; validates high-track-count probing and UI/audio-routing behavior.
    "test/fixtures/media-library/colorbar-timer-2v16a.mp4",
    // MediaMTX sink configuration fixture used by integration harnesses for
    // deterministic local RTMP/SRT/HLS sink behavior.
    "test/harness/mediamtx-sink.yml",
    // Self-signed cert+key for harness RTMPS coverage: mediamtx's RTMP-over-TLS
    // server cert, trusted by restream's egress client via
    // RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM. 20-year validity, CN=localhost.
    "test/fixtures/tls/mediamtx-rtmps-cert.pem",
    "test/fixtures/tls/mediamtx-rtmps-key.pem",
];

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

pub fn checked_in_fixture(relative_path: &str) -> Result<PathBuf, String> {
    let path = repo_root().join(relative_path);
    if path.is_file() {
        Ok(path)
    } else {
        Err(format!(
            "required checked-in fixture missing at {}; restore it from git",
            path.display()
        ))
    }
}

pub fn canonical_h264_ts_fixture() -> Result<PathBuf, String> {
    checked_in_fixture("test/fixtures/transport/correctness-h264.ts")
}

pub fn canonical_h265_ts_fixture() -> Result<PathBuf, String> {
    checked_in_fixture("test/fixtures/transport/correctness-h265.ts")
}

pub fn sparse_gop_mp4_fixture() -> Result<PathBuf, String> {
    checked_in_fixture("test/fixtures/media-library/sparse-gop-5s.mp4")
}

/// Self-signed cert+key pair (`CN=localhost`, SAN `localhost`/`127.0.0.1`,
/// 20-year validity) for harness RTMPS coverage. mediamtx uses it as its
/// RTMP-over-TLS server cert; restream's egress client trusts it via
/// `RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM` pointed at the same cert file —
/// RTMPS here is server-authenticated TLS only, so no client cert is needed.
pub fn rtmps_harness_cert_fixture() -> Result<(PathBuf, PathBuf), String> {
    Ok((
        checked_in_fixture("test/fixtures/tls/mediamtx-rtmps-cert.pem")?,
        checked_in_fixture("test/fixtures/tls/mediamtx-rtmps-key.pem")?,
    ))
}

pub fn canonical_ts_fixture(codec: &str) -> Result<PathBuf, String> {
    match codec {
        "h264" | "avc" => canonical_h264_ts_fixture(),
        "h265" | "hevc" => canonical_h265_ts_fixture(),
        other => Err(format!("unsupported transport fixture codec {other:?}")),
    }
}

pub fn bench_transport_fixture(
    codec: &str,
    bitrate_label: &str,
    multi_audio: bool,
) -> Result<PathBuf, String> {
    let codec = match codec {
        "h264" | "avc" => "h264",
        "h265" | "hevc" => "h265",
        other => return Err(format!("unsupported benchmark fixture codec {other:?}")),
    };
    let bitrate = bitrate_label.to_ascii_lowercase().replace('.', "_");
    let suffix = if multi_audio { "-2a" } else { "" };
    checked_in_fixture(&format!(
        "test/fixtures/transport/bench-{codec}-{bitrate}{suffix}.ts"
    ))
}

pub fn av_marker_transport_fixture(codec: &str, multi_audio: bool) -> Result<PathBuf, String> {
    av_marker_transport_fixture_for_bframes(codec, multi_audio, AvMarkerBframeMode::Bf2)
}

pub fn av_marker_transport_fixture_for_bframes(
    codec: &str,
    multi_audio: bool,
    bframes: AvMarkerBframeMode,
) -> Result<PathBuf, String> {
    let codec = match codec {
        "h264" | "avc" => "h264",
        "h265" | "hevc" => "h265",
        other => return Err(format!("unsupported A/V marker fixture codec {other:?}")),
    };
    let suffix = if multi_audio { "-2a" } else { "" };
    checked_in_fixture(&format!(
        "test/fixtures/transport/av-marker-{codec}{}{suffix}.ts",
        bframes.file_suffix()
    ))
}

pub fn primary_av_packets_for_codec(
    codec: &str,
) -> Result<(VideoMeta, Vec<AudioMeta>, Vec<MediaPacket>), String> {
    let path = canonical_ts_fixture(codec)?;
    let file_bytes = std::fs::read(&path)
        .map_err(|e| format!("failed to read fixture {}: {e}", path.display()))?;

    let mut demuxer = TsDemuxer::new();
    let mut all_packets = Vec::new();

    for chunk in file_bytes.chunks(1316) {
        demuxer.feed(chunk);
        demuxer.drain_into(&mut all_packets);
    }
    demuxer.flush();
    demuxer.drain_into(&mut all_packets);

    let mut probe = demuxer
        .take_probe()
        .ok_or_else(|| format!("failed to probe transport fixture {}", path.display()))?;
    let video = probe.video.ok_or_else(|| {
        format!(
            "missing video metadata in transport fixture {}",
            path.display()
        )
    })?;

    let mut audio_tracks: Vec<AudioMeta> = probe.audio_tracks.drain(..).take(1).collect();
    let keep_audio_track_index = audio_tracks.first().map(|a| a.track_index).unwrap_or(0);
    if let Some(track) = audio_tracks.first_mut() {
        track.track_index = 0;
    }

    let mut packets = Vec::new();
    for mut packet in all_packets {
        if packet.media_type == MediaType::Video {
            packets.push(packet);
        } else if packet.media_type == MediaType::Audio
            && packet.track_index == keep_audio_track_index
        {
            packet.track_index = 0;
            packets.push(packet);
        }
    }

    Ok((video, audio_tracks, packets))
}

pub fn wrap_packets_for_rtmp_ingest(
    video: &VideoMeta,
    audio_tracks: &[AudioMeta],
    packets: &mut [MediaPacket],
) -> (Option<Bytes>, Option<Bytes>) {
    let video_sequence_header = if is_hevc_codec(video.codec.as_str()) {
        None
    } else {
        packets
            .iter()
            .find(|packet| packet.media_type == MediaType::Video && packet.is_keyframe)
            .and_then(|packet| build_avcc_sequence_header(&packet.payload))
    };
    let audio_sequence_header = audio_tracks
        .first()
        .map(|track| build_aac_sequence_header(track.sample_rate, track.channels));

    let mut video_buf = Vec::new();
    let mut audio_buf = Vec::new();

    for packet in packets {
        match packet.media_type {
            MediaType::Video => {
                let wrote_video = if is_hevc_codec(video.codec.as_str()) {
                    hevc_video_for_rtmp_into(&packet.payload, packet.is_keyframe, &mut video_buf)
                } else {
                    video_for_rtmp_into(&packet.payload, packet.is_keyframe, &mut video_buf)
                };
                if wrote_video {
                    packet.payload = Bytes::copy_from_slice(&video_buf);
                    packet.format = PayloadFormat::Flv;
                }
            }
            MediaType::Audio => {
                audio_for_rtmp_into(&packet.payload, &mut audio_buf);
                packet.payload = Bytes::copy_from_slice(&audio_buf);
                packet.format = PayloadFormat::Flv;
            }
        }
    }

    (video_sequence_header, audio_sequence_header)
}

pub fn count_ts_feedable_packets(
    video: &VideoMeta,
    audio_tracks: &[AudioMeta],
    packets: &[MediaPacket],
    video_sequence_header: Option<&Bytes>,
) -> usize {
    let mut feeder = TsPacketFeeder::new(
        Some(video),
        Arc::new(audio_tracks.to_vec()),
        PacketFeedConfig {
            video_sequence_header: video_sequence_header.map(|header| header.to_vec()),
            ..PacketFeedConfig::default()
        },
    );
    let mut output = Vec::new();
    packets
        .iter()
        .filter(|packet| {
            output.clear();
            feeder.extend_ts_for_packet(packet, &mut output)
        })
        .count()
}

fn is_hevc_codec(codec: &str) -> bool {
    codec.eq_ignore_ascii_case("hevc") || codec.eq_ignore_ascii_case("h265")
}

fn hevc_video_for_rtmp_into(payload: &[u8], is_keyframe: bool, out: &mut Vec<u8>) -> bool {
    out.clear();
    out.extend_from_slice(&[
        if is_keyframe { 0x1C } else { 0x2C },
        0x01,
        0x00,
        0x00,
        0x00,
    ]);
    let start = out.len();
    for nalu in split_annexb_nalus(payload) {
        if nalu.is_empty() {
            continue;
        }
        out.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
        out.extend_from_slice(nalu);
    }
    out.len() > start
}
