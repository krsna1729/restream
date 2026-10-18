use bytes::Bytes;

use super::*;
use crate::media::codec::{
    build_aac_sequence_header, build_avcc_sequence_header, build_hevc_enhanced_rtmp_sequence_header,
};
use crate::media::moq::egress::startup::{MoqAudioStartup, MoqVideoStartup};
use crate::media::moq::media::{TrackContainer, parse_legacy_frame};
use crate::media::mpegts::TsDemuxer;

/// Baseline 3.0 SPS (640x480) and a PPS, Annex-B.
const PARAMETER_SETS: &[u8] = &[
    0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2,
];
const AUD: &[u8] = &[0x09, 0xF0];
const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x00, 0x21];
const SLICE: &[u8] = &[0x41, 0x9A, 0x02];

fn annexb(nalus: &[&[u8]]) -> Vec<u8> {
    nalus
        .iter()
        .flat_map(|nalu| [&[0, 0, 0, 1][..], nalu].concat())
        .collect()
}

fn length_prefixed(width: usize, nalus: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for nalu in nalus {
        out.extend_from_slice(&(nalu.len() as u32).to_be_bytes()[4 - width..]);
        out.extend_from_slice(nalu);
    }
    out
}

fn packet(
    media_type: MediaType,
    format: PayloadFormat,
    keyframe: bool,
    pts: i64,
    payload: Vec<u8>,
) -> MediaPacket {
    MediaPacket {
        media_type,
        track_index: 0,
        pts,
        dts: pts,
        is_keyframe: keyframe,
        format,
        payload: Bytes::from(payload),
    }
}

fn aac_audio(track_index: u32, sample_rate: u32) -> MoqAudioStartup {
    MoqAudioStartup {
        track_index,
        config: AudioConfig {
            sample_rate,
            channels: 2,
            asc: Some(build_aac_sequence_header(sample_rate, 2).slice(2..)),
            container: TrackContainer::Legacy,
        },
    }
}

fn converter(codec: Option<VideoCodec>, audio: Option<MoqAudioStartup>) -> MediaConverter {
    MediaConverter::new(&MoqPublishStartup {
        video: codec.map(|codec| MoqVideoStartup {
            codec,
            config: None,
        }),
        audio,
        ..MoqPublishStartup::default()
    })
}

fn convert(
    converter: &mut MediaConverter,
    packet: &MediaPacket,
    config: Option<&TrackConfig>,
) -> Vec<HangFrame> {
    let mut out = Vec::new();
    converter.convert(packet, config, &mut out).unwrap();
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
fn a_raw_keyframe_takes_its_ring_config_out_of_band_and_publishes_a_catalog() {
    let mut converter = converter(Some(VideoCodec::H264), None);
    assert!(
        matches!(converter.catalog(), Ok(None)),
        "video waits for a configuration"
    );
    let config = TrackConfig {
        generation: 1,
        bytes: Bytes::from_static(PARAMETER_SETS),
    };
    let keyframe = packet(
        MediaType::Video,
        PayloadFormat::Raw,
        true,
        40,
        [annexb(&[AUD]), PARAMETER_SETS.to_vec(), annexb(&[IDR])].concat(),
    );

    let frames = convert(&mut converter, &keyframe, Some(&config));

    assert_eq!(frames.len(), 1);
    assert!(frames[0].keyframe);
    assert_eq!(frames[0].pts_us, 40_000);
    assert_eq!(
        parse_legacy_frame(frames[0].data.clone()),
        Ok((40_000, Bytes::from(length_prefixed(4, &[IDR]))))
    );
    assert_eq!(frames[0].payload(), Bytes::from(length_prefixed(4, &[IDR])));
    assert_eq!(converter.catalog_version(), 1);
    let expected = VideoConfig::from_parameter_sets(VideoCodec::H264, PARAMETER_SETS).unwrap();
    assert_eq!(converter.video_config(), Some(&expected));
    let catalog = converter.catalog().unwrap().expect("configured");
    assert_eq!(
        catalog.video.renditions["video"].description,
        expected.description
    );
    assert_eq!(catalog.video.renditions["video"].coded_width, Some(640));

    // The same generation again: no lookup work, no new catalog.
    let delta = packet(
        MediaType::Video,
        PayloadFormat::Raw,
        false,
        80,
        annexb(&[SLICE]),
    );
    let frames = convert(&mut converter, &delta, Some(&config));
    assert!(!frames[0].keyframe);
    assert_eq!(converter.catalog_version(), 1);
}

#[test]
fn a_new_config_generation_republishes_only_when_the_record_changes() {
    let mut converter = converter(Some(VideoCodec::H264), None);
    let keyframe = packet(
        MediaType::Video,
        PayloadFormat::Raw,
        true,
        0,
        annexb(&[IDR]),
    );
    let first = TrackConfig {
        generation: 1,
        bytes: Bytes::from_static(PARAMETER_SETS),
    };
    convert(&mut converter, &keyframe, Some(&first));
    // Same sets under a new generation: the catalog stays.
    let same = TrackConfig {
        generation: 2,
        bytes: Bytes::from_static(PARAMETER_SETS),
    };
    convert(&mut converter, &keyframe, Some(&same));
    assert_eq!(converter.catalog_version(), 1);
    // Different sets: republished.
    let mut other = PARAMETER_SETS.to_vec();
    *other.last_mut().unwrap() ^= 0x01;
    let changed = TrackConfig {
        generation: 3,
        bytes: Bytes::from(other.clone()),
    };
    convert(&mut converter, &keyframe, Some(&changed));
    assert_eq!(converter.catalog_version(), 2);
    assert_eq!(
        converter.video_config(),
        VideoConfig::from_parameter_sets(VideoCodec::H264, &other).as_ref()
    );
}

#[test]
fn a_raw_keyframe_without_a_ring_config_uses_its_in_band_sets() {
    let mut converter = converter(Some(VideoCodec::H264), None);
    let keyframe = packet(
        MediaType::Video,
        PayloadFormat::Raw,
        true,
        0,
        [PARAMETER_SETS.to_vec(), annexb(&[IDR])].concat(),
    );
    convert(&mut converter, &keyframe, None);
    assert_eq!(
        converter.video_config(),
        VideoConfig::from_parameter_sets(VideoCodec::H264, PARAMETER_SETS).as_ref()
    );
}

#[test]
fn flv_avc_takes_its_sequence_header_as_the_record_and_copies_four_byte_units() {
    let mut converter = converter(Some(VideoCodec::H264), None);
    let header = build_avcc_sequence_header(PARAMETER_SETS).unwrap();
    assert!(
        convert(
            &mut converter,
            &packet(
                MediaType::Video,
                PayloadFormat::Flv,
                false,
                0,
                header.to_vec()
            ),
            None
        )
        .is_empty()
    );
    assert_eq!(
        converter.video_config().unwrap().description,
        Some(header.slice(5..))
    );
    assert_eq!(converter.video_config().unwrap().coded_height, Some(480));
    assert_eq!(converter.catalog_version(), 1);

    let body = length_prefixed(4, &[IDR]);
    let tag = [&[0x17, 0x01, 0, 0, 0][..], &body].concat();
    let frames = convert(
        &mut converter,
        &packet(MediaType::Video, PayloadFormat::Flv, true, 33, tag),
        None,
    );
    assert_eq!(frames.len(), 1);
    assert!(frames[0].keyframe);
    assert_eq!(frames[0].payload(), Bytes::from(body));
    assert_eq!(frames[0].pts_us, 33_000);
}

#[test]
fn flv_avc_with_two_byte_lengths_is_rewritten_to_four() {
    let mut converter = converter(Some(VideoCodec::H264), None);
    let mut header = build_avcc_sequence_header(PARAMETER_SETS).unwrap().to_vec();
    // lengthSizeMinusOne = 1.
    header[5 + 4] = 0xFC | 1;
    convert(
        &mut converter,
        &packet(MediaType::Video, PayloadFormat::Flv, false, 0, header),
        None,
    );

    let tag = [&[0x27, 0x01, 0, 0, 0][..], &length_prefixed(2, &[SLICE])].concat();
    let frames = convert(
        &mut converter,
        &packet(MediaType::Video, PayloadFormat::Flv, false, 1, tag),
        None,
    );
    assert_eq!(
        frames[0].payload(),
        Bytes::from(length_prefixed(4, &[SLICE]))
    );
    assert!(!frames[0].keyframe);
}

#[test]
fn enhanced_flv_hevc_records_its_hvcc_and_slices_coded_frames() {
    let sets = fixture_parameter_sets(crate::test_fixtures::canonical_h265_ts_fixture().unwrap());
    let header = build_hevc_enhanced_rtmp_sequence_header(&sets).unwrap();
    let mut converter = converter(Some(VideoCodec::H265), None);
    convert(
        &mut converter,
        &packet(
            MediaType::Video,
            PayloadFormat::Flv,
            false,
            0,
            header.to_vec(),
        ),
        None,
    );
    let config = converter.video_config().expect("hvcC recorded");
    assert_eq!(config.description, Some(header.slice(5..)));
    assert_eq!(
        (config.coded_width, config.coded_height),
        (Some(1920), Some(1080))
    );

    let idr: &[u8] = &[0x26, 0x01, 0xAF, 0x02];
    let body = length_prefixed(4, &[idr]);
    // CodedFrames (type 1) carries a 3-byte composition time; CodedFramesX
    // (type 3) none.
    for (tag, start) in [
        ([&[0x91][..], b"hvc1", &[0, 0, 40], &body].concat(), 8),
        ([&[0x93][..], b"hvc1", &body].concat(), 5),
    ] {
        let frames = convert(
            &mut converter,
            &packet(MediaType::Video, PayloadFormat::Flv, true, 0, tag.clone()),
            None,
        );
        assert_eq!(frames[0].payload(), Bytes::copy_from_slice(&tag[start..]));
        assert!(frames[0].keyframe);
    }
}

#[test]
fn flv_video_of_the_wrong_codec_or_too_short_is_refused() {
    let mut avc = converter(Some(VideoCodec::H264), None);
    let mut out = Vec::new();
    for (payload, error) in [
        (
            vec![0x91, b'h', b'v', b'c', b'1', 0, 0, 0],
            ConvertError::UnsupportedFlvVideo(0x91),
        ),
        (
            vec![0x1C, 1, 0, 0, 0, 1],
            ConvertError::UnsupportedFlvVideo(0x1C),
        ),
        (vec![0x17], ConvertError::TruncatedFlv),
        (vec![0x17, 1, 0], ConvertError::TruncatedFlv),
    ] {
        let packet = packet(MediaType::Video, PayloadFormat::Flv, false, 0, payload);
        assert_eq!(avc.convert(&packet, None, &mut out), Err(error));
    }
    let mut hevc = converter(Some(VideoCodec::H265), None);
    let packet = packet(
        MediaType::Video,
        PayloadFormat::Flv,
        false,
        0,
        vec![0x17, 1, 0, 0, 0, 1],
    );
    assert_eq!(
        hevc.convert(&packet, None, &mut out),
        Err(ConvertError::UnsupportedFlvVideo(0x17))
    );
    assert!(out.is_empty());
}

#[test]
fn flv_aac_strips_its_header_and_a_new_asc_republishes() {
    let mut converter = converter(None, Some(aac_audio(0, 48_000)));
    assert!(
        converter.catalog().unwrap().is_some(),
        "audio-only needs no video config"
    );
    let raw = vec![0xAF, 0x01, 0x21, 0x10, 0x05];
    let frames = convert(
        &mut converter,
        &packet(MediaType::Audio, PayloadFormat::Flv, true, 20, raw),
        None,
    );
    assert_eq!(frames[0].payload(), Bytes::from_static(&[0x21, 0x10, 0x05]));
    assert_eq!(frames[0].track, TrackKind::Audio);

    // The startup ASC again: unchanged. A different one: republished.
    let same = build_aac_sequence_header(48_000, 2).to_vec();
    convert(
        &mut converter,
        &packet(MediaType::Audio, PayloadFormat::Flv, false, 0, same),
        None,
    );
    assert_eq!(converter.catalog_version(), 0);
    let mono = build_aac_sequence_header(44_100, 1);
    convert(
        &mut converter,
        &packet(
            MediaType::Audio,
            PayloadFormat::Flv,
            false,
            0,
            mono.to_vec(),
        ),
        None,
    );
    assert_eq!(converter.catalog_version(), 1);
    assert_eq!(converter.audio_config().unwrap().asc, Some(mono.slice(2..)));

    let mut out = Vec::new();
    let mp3 = packet(
        MediaType::Audio,
        PayloadFormat::Flv,
        true,
        0,
        vec![0x2F, 0x01, 0],
    );
    assert_eq!(
        converter.convert(&mp3, None, &mut out),
        Err(ConvertError::UnsupportedFlvAudio(0x2F))
    );
}

#[test]
fn raw_adts_splits_into_access_units_one_aac_frame_apart() {
    let mut converter = converter(None, Some(aac_audio(0, 48_000)));
    let adts = |raw: &[u8]| {
        let mut frame = crate::media::codec::build_adts_header(raw.len(), 48_000, 2)
            .unwrap()
            .to_vec();
        frame.extend_from_slice(raw);
        frame
    };
    let payload = [adts(&[1, 2, 3]), adts(&[4, 5])].concat();
    let frames = convert(
        &mut converter,
        &packet(MediaType::Audio, PayloadFormat::Raw, true, 1000, payload),
        None,
    );
    assert_eq!(frames.len(), 2);
    assert_eq!(frames[0].payload(), Bytes::from_static(&[1, 2, 3]));
    assert_eq!(frames[1].payload(), Bytes::from_static(&[4, 5]));
    assert_eq!(frames[0].pts_us, 1_000_000);
    // 1024 samples at 48 kHz.
    assert_eq!(frames[1].pts_us, 1_000_000 + 21_333);

    // A raw AU without ADTS passes whole.
    let frames = convert(
        &mut converter,
        &packet(MediaType::Audio, PayloadFormat::Raw, true, 0, vec![9, 9]),
        None,
    );
    assert_eq!(frames[0].payload(), Bytes::from_static(&[9, 9]));
}

#[test]
fn other_tracks_are_ignored_and_negative_times_clamp_to_zero() {
    let mut converter = converter(Some(VideoCodec::H264), Some(aac_audio(1, 48_000)));
    let mut audio0 = packet(MediaType::Audio, PayloadFormat::Raw, true, 0, vec![1]);
    assert!(convert(&mut converter, &audio0, None).is_empty());
    audio0.track_index = 1;
    audio0.pts = -5;
    assert_eq!(convert(&mut converter, &audio0, None)[0].pts_us, 0);
    let mut video1 = packet(
        MediaType::Video,
        PayloadFormat::Raw,
        true,
        0,
        annexb(&[IDR]),
    );
    video1.track_index = 1;
    assert!(convert(&mut converter, &video1, None).is_empty());
    assert!(!converter.wants_track_config(&video1));

    let no_video = super::MediaConverter::new(&MoqPublishStartup::default());
    let video0 = packet(
        MediaType::Video,
        PayloadFormat::Raw,
        true,
        0,
        annexb(&[IDR]),
    );
    assert!(!no_video.wants_track_config(&video0));
    assert!(!no_video.has_video() && !no_video.has_audio());
}
