// Test code may index, unwrap and do plain arithmetic; the parent module's
// deny lints guard the parser, not its tests.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use hang::container::{Frame, Timestamp};

use super::*;
use restream_moq::media::*;
const VIDEO_TRACK: &str = "video";
const AUDIO_TRACK: &str = "audio";
use crate::media::codec::annexb_parameter_sets;
use crate::media::mpegts::TsDemuxer;
use crate::media::packet::MediaType;
use bytes::{Bytes, BytesMut};
use moq_net::VarInt;
use std::time::Duration;

fn annexb(nalus: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for nalu in nalus {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nalu);
    }
    out
}

fn length_prefixed(nalus: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for nalu in nalus {
        out.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
        out.extend_from_slice(nalu);
    }
    out
}

/// The parameter sets of a checked-in fixture's first keyframe.
fn fixture_parameter_sets(path: std::path::PathBuf) -> Vec<u8> {
    let ts = std::fs::read(&path).unwrap();
    let mut demuxer = TsDemuxer::new();
    for chunk in ts.chunks(64 << 10) {
        demuxer.feed(chunk);
        for packet in demuxer.drain() {
            if packet.media_type == MediaType::Video && packet.is_keyframe {
                return annexb_parameter_sets(&packet.payload).expect("in-band parameter sets");
            }
        }
    }
    panic!("{} has no keyframe", path.display());
}

#[test]
fn loc_frames_are_moq_locs_and_frame_the_payload_in_place() {
    for us in [
        0,
        63,
        64,
        16_383,
        16_384,
        (1 << 30) - 1,
        1 << 30,
        (1 << 62) - 1,
    ] {
        let payload = b"frame";
        let buffer = LocBuffer::from_payload(payload);
        let at = buffer.payload().as_ptr();
        let ours = buffer.into_frame(us).unwrap();
        let decoded = moq_loc::decode(ours.clone()).unwrap();
        assert_eq!(
            decoded.timescale,
            Some(1_000_000),
            "media time, not Unix time"
        );
        assert_eq!(decoded.timestamp, us);
        assert_eq!(decoded.payload.as_ref(), payload);
        assert_eq!(
            ours[ours.len() - payload.len()..].as_ptr(),
            at,
            "{us}: the payload was not moved"
        );
        let (timestamp, decoded) = parse_loc_frame(ours).unwrap();
        assert_eq!(timestamp, us);
        assert_eq!(decoded.as_ref(), payload);
        assert_eq!(decoded.as_ptr(), at, "LOC decode must only slice the input");
    }
    assert_eq!(
        LocBuffer::from_payload(b"x").into_frame(1 << 62),
        Err(MediaError::TimestampTooLarge)
    );
}

#[test]
fn loc_frames_honour_a_timescale_and_need_a_timestamp() {
    // Timescale 90 000 (0x08), then Timestamp 90 000 (0x10, delta 8): 1 s.
    let mut frame = vec![
        10, 0x08, 0x80, 0x01, 0x5F, 0x90, 0x08, 0x80, 0x01, 0x5F, 0x90,
    ];
    frame.extend_from_slice(b"au");
    assert_eq!(
        parse_loc_frame(Bytes::from(frame)),
        Ok((1_000_000, Bytes::from_static(b"au")))
    );
    // Only a timescale; then a block longer than the frame.
    for frame in [&[2, 0x08, 0x01, b'a'][..], &[5, 0x10, 0x01]] {
        assert!(parse_loc_frame(Bytes::copy_from_slice(frame)).is_err());
    }
}

#[test]
fn loc_frames_reject_zero_timescale_and_timestamp_scaling_overflow() {
    let scaled = |scale: u64, timestamp: u64| {
        let mut properties = BytesMut::new();
        for value in [0x08, scale, 0x08, timestamp] {
            VarInt::from_u64(value)
                .unwrap()
                .encode_quic(&mut properties)
                .unwrap();
        }
        let mut frame = BytesMut::new();
        VarInt::from_u64(properties.len() as u64)
            .unwrap()
            .encode_quic(&mut frame)
            .unwrap();
        frame.extend_from_slice(&properties);
        frame.extend_from_slice(b"au");
        frame.freeze()
    };
    assert!(parse_loc_frame(scaled(0, 1)).is_err());
    assert_eq!(
        parse_loc_frame(scaled(1, (1 << 62) - 1)),
        Err(WireError::Invalid("LOC timestamp out of range")),
    );
    assert_eq!(
        parse_loc_frame(scaled(90_000, 90_045)),
        Ok((1_000_500, Bytes::from_static(b"au"))),
    );
}

#[test]
fn legacy_frames_split_without_a_copy() {
    let mut ours = BytesMut::new();
    Frame {
        timestamp: Timestamp::from_micros(16_384).unwrap(),
        payload: Bytes::from_static(b"frame"),
    }
    .encode(&mut ours)
    .unwrap();
    assert_eq!(
        parse_legacy_frame(ours.freeze()),
        Ok((16_384, Bytes::from_static(b"frame")))
    );
    // A two-byte varint cut after its first byte, and an empty frame.
    for frame in [&[0x40][..], &[]] {
        assert_eq!(
            parse_legacy_frame(Bytes::copy_from_slice(frame)),
            Err(WireError::Invalid("truncated legacy frame timestamp"))
        );
    }
    // A timestamp with no payload is a valid (empty) frame.
    assert_eq!(
        parse_legacy_frame(Bytes::from_static(&[0x05])),
        Ok((5, Bytes::new()))
    );
}

#[test]
fn parameter_sets_go_out_of_band() {
    let aud: &[u8] = &[0x09, 0xF0];
    let sps: &[u8] = &[0x67, 0x64, 0x00, 0x1F, 0xAC];
    let pps: &[u8] = &[0x68, 0xEE, 0x3C, 0x80];
    let sei: &[u8] = &[0x06, 0x05, 0x01, 0x80];
    let idr: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x21];
    let mut out = BytesMut::new();
    annexb_to_length_prefixed(
        VideoCodec::H264,
        &annexb(&[aud, sps, pps, sei, idr]),
        &mut out,
    );
    assert_eq!(out, length_prefixed(&[sei, idr]));

    // H.265: AUD, VPS, SPS, PPS dropped; IDR_N_LP (0x28, an H.264 PPS byte
    // if misread) and STSA_N (0x08) kept.
    let nalus: [&[u8]; 6] = [
        &[0x46, 0x01, 0x50],
        &[0x40, 0x01, 0x0C],
        &[0x42, 0x01, 0x01],
        &[0x44, 0x01, 0xC1],
        &[0x28, 0x01, 0xAF],
        &[0x08, 0x01, 0xD0],
    ];
    let mut out = BytesMut::new();
    annexb_to_length_prefixed(VideoCodec::H265, &annexb(&nalus), &mut out);
    assert_eq!(out, length_prefixed(&nalus[4..]));
}

#[test]
fn fixture_configs_carry_records_sizes_and_codec_strings() {
    let cases = [
        (
            VideoCodec::H264,
            crate::test_fixtures::canonical_h264_ts_fixture().unwrap(),
            "avc1.640032",
        ),
        (
            VideoCodec::H265,
            crate::test_fixtures::canonical_h265_ts_fixture().unwrap(),
            "hvc1.1.6.L120.90",
        ),
    ];
    for (codec, fixture, codec_string) in cases {
        let sets = fixture_parameter_sets(fixture);
        let config = VideoConfig::from_parameter_sets(codec, &sets).expect("config");
        assert_eq!(
            (config.coded_width, config.coded_height),
            (Some(1920), Some(1080))
        );
        // The record reads back as the same parameter sets, 4-byte lengths.
        assert_eq!(config.parameter_sets(), Some((Bytes::from(sets), 4)));

        let catalog = hang_catalog(Some(&config), None).unwrap();
        let rendition = &catalog.video.renditions[VIDEO_TRACK];
        assert_eq!(rendition.codec.to_string(), codec_string);
        assert_eq!(rendition.description, config.description);
    }
}

#[test]
fn a_published_catalog_reads_back() {
    let sets = fixture_parameter_sets(crate::test_fixtures::canonical_h264_ts_fixture().unwrap());
    let mut video = VideoConfig::from_parameter_sets(VideoCodec::H264, &sets).unwrap();
    video.jitter = Some(Duration::from_millis(66));
    let audio = AudioConfig {
        sample_rate: 48_000,
        channels: 2,
        asc: Some(Bytes::from_static(&[0x11, 0x90])),
        container: TrackContainer::Legacy,
    };
    let json = hang_catalog(Some(&video), Some(&audio))
        .unwrap()
        .to_vec()
        .unwrap();
    let text = std::str::from_utf8(&json).unwrap();
    // WebCodecs field names; the record is hex, not base64. Every track is
    // LOC, whatever container the configurations were read from.
    assert!(text.contains(r#""codec":"mp4a.40.2""#), "{text}");
    assert!(text.contains(r#""description":"1190""#), "{text}");
    assert!(text.contains(r#""numberOfChannels":2"#), "{text}");

    let tracks = from_hang_catalog(&hang::Catalog::from_slice(&json).unwrap());
    let loc = TrackContainer::Loc;
    assert_eq!(
        tracks.video,
        Some((
            VIDEO_TRACK.to_string(),
            VideoConfig {
                container: loc.clone(),
                ..video
            }
        ))
    );
    assert_eq!(
        tracks.audio,
        vec![(
            AUDIO_TRACK.to_string(),
            AudioConfig {
                container: loc,
                ..audio
            }
        )]
    );
    assert!(tracks.rejected.is_empty());
}

#[test]
fn unsupported_renditions_are_reported() {
    let json = br#"{
        "video": {"renditions": {
            "a": {"codec": "vp09.00.10.08", "codedWidth": 1920, "codedHeight": 1080},
            "b": {"codec": "avc3.64001f", "codedWidth": 1280, "codedHeight": 720, "container": {"kind": "loc"}},
            "c": {"codec": "avc3.64001f", "codedWidth": 640, "codedHeight": 360},
            "d": {"codec": "hev1.1.6.L93.B0", "codedWidth": 320, "codedHeight": 180}
        }},
        "audio": {"renditions": {
            "x": {"codec": "opus", "sampleRate": 48000, "numberOfChannels": 2},
            "y": {"codec": "mp4a.40.2", "sampleRate": 44100, "numberOfChannels": 1,
                  "container": {"kind": "cmaf", "init": "AAAA"}}
        }}
    }"#;
    let tracks = from_hang_catalog(&hang::Catalog::from_slice(json).unwrap());
    let (name, video) = tracks.video.expect("the best supported rendition");
    assert_eq!(name, "b");
    assert_eq!(video.codec, VideoCodec::H264);
    assert_eq!(video.container, TrackContainer::Loc);
    assert_eq!(video.description, None, "avc3: parameter sets in band");
    assert_eq!(
        tracks.audio,
        vec![(
            "y".to_string(),
            AudioConfig {
                sample_rate: 44_100,
                channels: 1,
                asc: None,
                container: TrackContainer::Cmaf {
                    init: Bytes::from_static(&[0, 0, 0]),
                },
            }
        )]
    );
    let mut rejected: Vec<&str> = tracks
        .rejected
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    rejected.sort_unstable();
    assert_eq!(rejected, ["a", "c", "d", "x"]);
}

#[test]
fn a_record_is_required_to_publish_video() {
    let config = VideoConfig {
        codec: VideoCodec::H264,
        description: None,
        coded_width: None,
        coded_height: None,
        framerate: None,
        jitter: None,
        container: TrackContainer::Legacy,
    };
    assert_eq!(
        hang_catalog(Some(&config), None),
        Err(MediaError::MissingDescription)
    );
    assert_eq!(config.parameter_sets(), None);
    // A truncated hvcC record has no parameter sets either.
    let truncated = VideoConfig {
        codec: VideoCodec::H265,
        description: Some(Bytes::from_static(&[1; 22])),
        ..config
    };
    assert_eq!(truncated.parameter_sets(), None);
}
