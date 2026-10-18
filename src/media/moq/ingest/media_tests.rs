//! Catalog → track selection and LOC / legacy frame → packet conversion.

// Test code may index, unwrap and do plain arithmetic; the parent module's
// deny lints guard the parser, not its tests.
#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::time::Duration;

use super::*;
use crate::media::codec::{MAX_ADTS_PAYLOAD, build_adts_header_for};
use crate::media::moq::media::{LocBuffer, from_hang_catalog};

const SPS: &[u8] = &[0x67, 0x42, 0x00, 0x1e, 0xf4, 0x05, 0x01, 0xec, 0x80];
const PPS: &[u8] = &[0x68, 0xce, 0x06, 0xe2];
const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x00];
/// H.264 non-IDR slice with `nal_ref_idc` 1: its byte (0x21) reads as an
/// H.265 IRAP type to a codec-blind scan.
const P_SLICE: &[u8] = &[0x21, 0x9a, 0x02];
/// AAC-LC, 48 kHz, stereo.
const ASC: &[u8] = &[0x11, 0x90];

fn prefixed(nalus: &[&[u8]], size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for nalu in nalus {
        out.extend_from_slice(&u32::try_from(nalu.len()).unwrap().to_be_bytes()[4 - size..]);
        out.extend_from_slice(nalu);
    }
    out
}

fn annexb(nalus: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for nalu in nalus {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nalu);
    }
    out
}

/// An avcC record with 2-byte NAL lengths.
fn avcc() -> Bytes {
    let mut record = vec![1, 0x42, 0, 0x1e, 0xfd, 0xe1];
    record.extend_from_slice(&u16::try_from(SPS.len()).unwrap().to_be_bytes());
    record.extend_from_slice(SPS);
    record.push(1);
    record.extend_from_slice(&u16::try_from(PPS.len()).unwrap().to_be_bytes());
    record.extend_from_slice(PPS);
    Bytes::from(record)
}

/// A hang legacy frame, as hang writes it.
fn legacy(us: u64, payload: &[u8]) -> Bytes {
    let mut frame = BytesMut::new();
    hang::container::Frame {
        timestamp: hang::container::Timestamp::from_micros(us).unwrap(),
        payload: Bytes::copy_from_slice(payload),
    }
    .encode(&mut frame)
    .unwrap();
    frame.freeze()
}

fn video_config(codec: VideoCodec, description: Option<Bytes>) -> VideoConfig {
    VideoConfig {
        codec,
        description,
        coded_width: Some(640),
        coded_height: Some(360),
        framerate: Some(30.0),
        jitter: None,
        container: TrackContainer::Legacy,
    }
}

fn aac_track(asc: Option<&[u8]>) -> AudioConfig {
    AudioConfig {
        sample_rate: 48_000,
        channels: 2,
        asc: asc.map(Bytes::copy_from_slice),
        container: TrackContainer::Legacy,
    }
}

fn video_set(config: VideoConfig) -> TrackSet {
    TrackSet::new(CatalogTracks {
        video: Some(("hd".to_string(), config)),
        ..CatalogTracks::default()
    })
}

/// Convert one frame on track 0, expecting exactly one packet.
fn convert(set: &mut TrackSet, frame: Bytes) -> Result<MediaPacket, ConvertError> {
    let mut out = Vec::new();
    set.track_mut(0).unwrap().push(frame, &mut out)?;
    assert_eq!(out.len(), 1);
    Ok(out.remove(0))
}

#[test]
fn loc_frames_convert_like_legacy_ones_and_markers_are_skipped() {
    let loc = |us: u64, payload: &[u8]| LocBuffer::from_payload(payload).into_frame(us).unwrap();
    let mut set = video_set(VideoConfig {
        container: TrackContainer::Loc,
        ..video_config(VideoCodec::H264, None)
    });
    let packet = convert(&mut set, loc(40_000, &annexb(&[SPS, PPS, IDR]))).unwrap();
    assert_eq!((packet.pts, packet.is_keyframe), (40, true));
    assert_eq!(&packet.payload[..], &annexb(&[SPS, PPS, IDR])[..]);
    // The end-of-GOP marker moq-mux writes: a timestamp, no payload.
    let mut out = Vec::new();
    set.track_mut(0)
        .unwrap()
        .push(loc(73_000, &[]), &mut out)
        .unwrap();
    assert!(out.is_empty(), "a marker is no packet");
    // A legacy frame on a LOC track is malformed.
    assert!(convert(&mut set, legacy(80_000, &annexb(&[IDR]))).is_err());
}

#[test]
fn loc_annexb_conversion_preserves_payload_ownership_and_timescale() {
    for (codec, nalu) in [
        (VideoCodec::H264, P_SLICE),
        (VideoCodec::H265, &[1 << 1, 1, 0xd0][..]),
    ] {
        let mut set = video_set(VideoConfig {
            container: TrackContainer::Loc,
            ..video_config(codec, None)
        });
        let payload = annexb(&[nalu]);
        // Timescale 90 kHz, timestamp 90 kHz = one second.
        let mut wire = BytesMut::from(&[
            10, 0x08, 0x80, 0x01, 0x5f, 0x90, 0x08, 0x80, 0x01, 0x5f, 0x90,
        ][..]);
        wire.extend_from_slice(&payload);
        let wire = wire.freeze();
        let at = wire[11..].as_ptr();
        let packet = convert(&mut set, wire).unwrap();
        assert_eq!((packet.pts, packet.dts), (1_000, 1_000));
        assert_eq!(packet.payload.as_ref(), payload.as_slice());
        assert_eq!(packet.payload.as_ptr(), at, "Annex-B LOC remains a shared slice");
        for malformed in [
            &[0][..], // No timestamp.
            &[5, 0x10, 1][..], // Truncated property block.
            &[4, 0x08, 0, 0x08, 1][..], // Zero timescale.
        ] {
            assert!(matches!(
                convert(&mut set, Bytes::copy_from_slice(malformed)),
                Err(ConvertError::Frame(_)),
            ));
        }
        let packet = convert(
            &mut set,
            LocBuffer::from_payload(&payload).into_frame(1_001_000).unwrap(),
        ).unwrap();
        assert_eq!((packet.pts, packet.dts), (1_001, 1_001), "bad frames do not advance time");
    }
}

#[test]
fn a_catalog_yields_one_video_and_its_aac_tracks_in_order() {
    let json = br#"{
        "video": {"renditions": {
            "hd": {"codec": "avc3.64001f", "codedWidth": 1280, "codedHeight": 720}
        }},
        "audio": {"renditions": {
            "a": {"codec": "mp4a.40.2", "sampleRate": 44100, "numberOfChannels": 1,
                  "container": {"kind": "cmaf", "init": "AAAA"}},
            "b": {"codec": "mp4a.40.2", "sampleRate": 48000, "numberOfChannels": 2},
            "c": {"codec": "mp4a.40.2", "sampleRate": 44100, "numberOfChannels": 1},
            "x": {"codec": "opus", "sampleRate": 48000, "numberOfChannels": 2}
        }}
    }"#;
    let set = TrackSet::new(from_hang_catalog(&hang::Catalog::from_slice(json).unwrap()));
    let taken: Vec<(&str, TrackKind, u32)> = set
        .tracks()
        .iter()
        .map(|track| (track.name.as_str(), track.kind, track.track_index))
        .collect();
    assert_eq!(
        taken,
        [
            ("hd", TrackKind::Video, 0),
            ("b", TrackKind::Audio, 0),
            ("c", TrackKind::Audio, 1)
        ],
        "a refused CMAF init takes no ring index"
    );
    let mut rejected: Vec<&str> = set
        .rejected()
        .iter()
        .map(|(name, _)| name.as_str())
        .collect();
    rejected.sort_unstable();
    assert_eq!(rejected, ["a", "x"]);
    let probe = set.probe();
    assert!(probe.is_none(), "avc3 video waits for its first keyframe");
    assert!(TrackSet::new(CatalogTracks::default()).is_empty());
}

#[test]
fn audio_tracks_past_the_bound_are_refused_with_a_reason() {
    let audio = (0..=MAX_AUDIO_TRACKS)
        .map(|index| (format!("a{index}"), aac_track(Some(ASC))))
        .collect();
    let set = TrackSet::new(CatalogTracks {
        audio,
        ..CatalogTracks::default()
    });
    assert_eq!(set.tracks().len(), MAX_AUDIO_TRACKS);
    assert_eq!(set.rejected().len(), 1);
    assert!(set.rejected()[0].1.contains("audio tracks"));
    let probe = set.probe().expect("audio only: complete at once");
    assert_eq!(probe.audio_tracks.len(), MAX_AUDIO_TRACKS);
    assert!(probe.video.is_none());
}

#[test]
fn aac_config_comes_from_the_asc_or_the_rate_and_channels() {
    let config = aac_config(Some(ASC), 44_100, 1).unwrap();
    assert_eq!(
        (config.sample_rate(), config.channels()),
        (48_000, 2),
        "the ASC wins"
    );
    let lc = aac_config(None, 44_100, 2).unwrap();
    assert_eq!((lc.sample_rate(), lc.channels()), (44_100, 2));
    assert!(aac_config(None, 12_345, 2).is_err(), "no ADTS index");
    assert!(aac_config(Some(&[]), 48_000, 2).is_err());
}

#[test]
fn an_avc1_track_knows_its_probe_up_front_and_injects_record_sets() {
    let mut set = video_set(video_config(VideoCodec::H264, Some(avcc())));
    assert!(set.probe().expect("record sets").video.is_some());

    let key = convert(&mut set, legacy(2_000_000, &prefixed(&[IDR], 2))).unwrap();
    assert!(key.is_keyframe);
    assert_eq!((key.media_type, key.track_index), (MediaType::Video, 0));
    assert_eq!((key.pts, key.dts), (2_000, 2_000));
    assert_eq!(key.payload.as_ref(), annexb(&[SPS, PPS, IDR]).as_slice());

    let delta = convert(&mut set, legacy(2_033_333, &prefixed(&[P_SLICE], 2))).unwrap();
    assert!(
        !delta.is_keyframe,
        "a codec-aware scan does not misread 0x21"
    );
    assert_eq!((delta.pts, delta.dts), (2_033, 2_033));
    assert_eq!(delta.payload.as_ref(), annexb(&[P_SLICE]).as_slice());
}

#[test]
fn a_malformed_record_refuses_the_track() {
    let set = video_set(video_config(VideoCodec::H264, Some(avcc().slice(..8))));
    assert!(set.is_empty());
    assert!(set.rejected()[0].1.contains("description"));
}

#[test]
fn an_avc3_track_passes_annex_b_through_and_learns_inline_sets() {
    let mut set = video_set(video_config(VideoCodec::H264, None));
    let bare = convert(&mut set, legacy(0, &annexb(&[IDR]))).unwrap();
    assert_eq!(
        bare.payload.as_ref(),
        annexb(&[IDR]).as_slice(),
        "nothing to inject yet"
    );
    assert!(set.probe().is_none());
    convert(&mut set, legacy(1_000, &annexb(&[SPS, PPS, IDR]))).unwrap();
    let later = convert(&mut set, legacy(2_000, &annexb(&[IDR]))).unwrap();
    assert_eq!(later.payload.as_ref(), annexb(&[SPS, PPS, IDR]).as_slice());
    assert!(set.probe().unwrap().video.is_some());
}

#[test]
fn malformed_frames_are_refused_and_leave_track_state_alone() {
    let mut set = video_set(video_config(VideoCodec::H264, None));
    assert!(matches!(
        convert(&mut set, Bytes::new()),
        Err(ConvertError::Frame(_))
    ));
    assert!(matches!(
        convert(&mut set, legacy(0, &[1, 2, 3])),
        Err(ConvertError::MalformedVideo(_))
    ));
    let mut record = video_set(video_config(VideoCodec::H264, Some(avcc())));
    let mut truncated = prefixed(&[SPS, PPS, IDR], 2);
    truncated.pop();
    assert!(matches!(
        convert(&mut record, legacy(0, &truncated)),
        Err(ConvertError::MalformedVideo(_))
    ));
    assert!(matches!(
        convert(&mut record, legacy(0, &[])),
        Err(ConvertError::MalformedVideo(_))
    ));
    assert!(set.probe().is_none(), "a refused keyframe sets no metadata");
}

#[test]
fn hevc_keyframes_are_irap_pictures_only() {
    let mut set = video_set(video_config(VideoCodec::H265, None));
    let irap: &[u8] = &[19 << 1, 0x01, 0xaf];
    let trail: &[u8] = &[1 << 1, 0x01, 0xd0];
    assert!(
        convert(&mut set, legacy(0, &annexb(&[irap])))
            .unwrap()
            .is_keyframe
    );
    assert!(
        !convert(&mut set, legacy(1, &annexb(&[trail])))
            .unwrap()
            .is_keyframe
    );
    assert!(
        !convert(&mut set, legacy(2, &annexb(&[IDR])))
            .unwrap()
            .is_keyframe,
        "an H.264 IDR byte is not an HEVC IRAP"
    );
}

#[test]
fn decode_time_trails_presentation_by_the_jitter_and_never_goes_back() {
    let mut config = video_config(VideoCodec::H264, None);
    config.jitter = Some(Duration::from_millis(66));
    let mut set = video_set(config);
    // I P B B in decode order: presentation 0, 99, 33, 66 ms.
    let times: Vec<(i64, i64)> = [0u64, 99_000, 33_000, 66_000]
        .into_iter()
        .map(|pts| {
            let packet = convert(&mut set, legacy(2_000_000 + pts, &annexb(&[P_SLICE]))).unwrap();
            (packet.pts, packet.dts)
        })
        .collect();
    assert_eq!(
        times,
        [
            (2_000, 1_934),
            (2_099, 2_033),
            (2_033, 2_033),
            (2_066, 2_033)
        ]
    );
    assert!(times.iter().all(|(pts, dts)| dts <= pts));
}

#[test]
fn an_aac_frame_is_adts_wrapped_with_its_ring_index() {
    let mut set = TrackSet::new(CatalogTracks {
        audio: vec![
            ("en".to_string(), aac_track(Some(ASC))),
            ("fr".to_string(), aac_track(None)),
        ],
        ..CatalogTracks::default()
    });
    let raw = [0x21, 0x10, 0x05, 0x40];
    let audio = set.track_mut(1).unwrap();
    let mut out = Vec::new();
    audio.push(legacy(3_000_500, &raw), &mut out).unwrap();
    let packet = out.pop().unwrap();
    assert_eq!(
        (packet.media_type, packet.is_keyframe, packet.track_index),
        (MediaType::Audio, false, 1)
    );
    assert_eq!((packet.pts, packet.dts), (3_001, 3_001));
    let header = build_adts_header_for(raw.len(), 2, 3, 2).unwrap();
    assert_eq!(
        packet.payload.as_ref(),
        [&header[..], &raw[..]].concat().as_slice()
    );
    let too_long = vec![0u8; MAX_ADTS_PAYLOAD + 1];
    assert_eq!(
        audio.push(legacy(0, &too_long), &mut out).err(),
        Some(ConvertError::AudioFrameTooLarge(MAX_ADTS_PAYLOAD + 1))
    );
    assert!(out.is_empty());
}

#[test]
fn microseconds_round_to_the_nearest_millisecond() {
    assert_eq!(us_to_ms(0), 0);
    assert_eq!(us_to_ms(1_499), 1);
    assert_eq!(us_to_ms(1_500), 2);
    assert_eq!(us_to_ms(u64::MAX), i64::try_from(u64::MAX / 1000).unwrap());
}
