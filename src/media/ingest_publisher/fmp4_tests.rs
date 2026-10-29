#![allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::num::NonZeroU32;

use shiguredo_mp4::TrackKind;
use shiguredo_mp4::bitstream::aac::{
    AudioObjectType, AudioSpecificConfig, ChannelConfiguration, Mp4aSampleEntryConfig,
    SamplingFrequency, build_mp4a_box,
};
use shiguredo_mp4::bitstream::h264::{
    H264SampleEntryConfig, LengthSize, build_avc1_box_from_annexb,
};
use shiguredo_mp4::bitstream::h265::{
    H265ConstantFrameRate, H265SampleEntryConfig, build_hev1_box_from_annexb,
    build_hvc1_box_from_annexb,
};
use shiguredo_mp4::mux::{Fmp4SegmentMuxer, Sample};

use super::*;
use restream_core::codec::build_adts_header_for;

const VIDEO_TIMESCALE: u32 = 90_000;
const AUDIO_TIMESCALE: u32 = 48_000;
const MAX_INIT: usize = 4 * 1024 * 1024;

/// The first keyframe of a checked-in TS fixture, as Annex B (it carries
/// the parameter sets a sample entry is built from).
fn fixture_keyframe(codec: &str) -> Vec<u8> {
    let (_, _, packets) = crate::test_fixtures::primary_av_packets_for_codec(codec).unwrap();
    packets
        .into_iter()
        .find(|packet| packet.media_type == MediaType::Video && packet.is_keyframe)
        .unwrap()
        .payload
        .to_vec()
}

fn avc1(length_size: LengthSize) -> SampleEntry {
    let config = H264SampleEntryConfig { length_size };
    SampleEntry::Avc1(build_avc1_box_from_annexb(&fixture_keyframe("h264"), &config).unwrap())
}

fn mp4a() -> SampleEntry {
    let asc = AudioSpecificConfig {
        audio_object_type: AudioObjectType::AacLc,
        sampling_frequency: SamplingFrequency::from_hz(AUDIO_TIMESCALE).unwrap(),
        channel_configuration: ChannelConfiguration::Stereo,
    };
    let config = Mp4aSampleEntryConfig {
        es_id: 1,
        buffer_size_db: 0,
        max_bitrate: 128_000,
        avg_bitrate: 128_000,
    };
    SampleEntry::Mp4a(build_mp4a_box(&asc, &config).unwrap())
}

/// One sample to mux: its entry, duration, keyframe flag, composition
/// offset and payload.
struct In {
    kind: TrackKind,
    entry: SampleEntry,
    duration: u32,
    keyframe: bool,
    cto: Option<i64>,
    data: Vec<u8>,
}

fn video(entry: &SampleEntry, duration: u32, keyframe: bool, cto: i64, data: Vec<u8>) -> In {
    In {
        kind: TrackKind::Video,
        entry: entry.clone(),
        duration,
        keyframe,
        cto: Some(cto),
        data,
    }
}

fn audio(entry: &SampleEntry, data: Vec<u8>) -> In {
    In {
        kind: TrackKind::Audio,
        entry: entry.clone(),
        duration: 1024,
        keyframe: true,
        cto: None,
        data,
    }
}

/// One media segment (`moof` + `mdat`) of `samples`, each track's payload
/// contiguous, in the order given.
fn segment(muxer: &mut Fmp4SegmentMuxer, samples: &[In]) -> Vec<u8> {
    let mut offset = 0u64;
    let described: Vec<Sample> = samples
        .iter()
        .map(|sample| {
            let described = Sample {
                track_kind: sample.kind,
                sample_entry: Some(sample.entry.clone()),
                keyframe: sample.keyframe,
                timescale: NonZeroU32::new(match sample.kind {
                    TrackKind::Audio => AUDIO_TIMESCALE,
                    _ => VIDEO_TIMESCALE,
                })
                .unwrap(),
                duration: sample.duration,
                composition_time_offset: sample.cto,
                data_offset: offset,
                data_size: sample.data.len(),
            };
            offset += sample.data.len() as u64;
            described
        })
        .collect();
    let mut bytes = muxer.create_media_segment_metadata(&described).unwrap();
    for sample in samples {
        bytes.extend_from_slice(&sample.data);
    }
    bytes
}

/// FFmpeg's media segment head: `styp` and `sidx` ahead of the fragment.
fn ffmpeg_head() -> Vec<u8> {
    let styp = [
        &24u32.to_be_bytes()[..],
        b"styp",
        b"msdh",
        &[0; 4],
        b"msdhmsix",
    ]
    .concat();
    let sidx = [&12u32.to_be_bytes()[..], b"sidx", &[0; 4]].concat();
    [styp, sidx].concat()
}

/// NAL units behind `length_size`-byte big-endian lengths.
fn length_prefixed(nalus: &[&[u8]], length_size: usize) -> Vec<u8> {
    let mut out = Vec::new();
    for nalu in nalus {
        out.extend_from_slice(&(nalu.len() as u32).to_be_bytes()[4 - length_size..]);
        out.extend_from_slice(nalu);
    }
    out
}

/// Every packet of `segments` through the demux, whole objects at once.
fn demux_all(init: &[u8], segments: &[Vec<u8>], chunk: usize) -> Vec<MediaPacket> {
    let mut demux = Fmp4Demux::default();
    demux.apply_init(init, MAX_INIT).unwrap();
    let mut out = Vec::new();
    for segment in segments {
        demux.begin_object(&Bytes::from(segment.clone())).unwrap();
        for piece in segment.chunks(chunk) {
            demux.feed(piece).unwrap();
            demux.drain_into(&mut out);
        }
        demux.end_object().unwrap();
        demux.drain_into(&mut out);
    }
    out
}

fn of_type(packets: &[MediaPacket], media_type: MediaType) -> Vec<&MediaPacket> {
    packets
        .iter()
        .filter(|packet| packet.media_type == media_type)
        .collect()
}

const IDR: &[u8] = &[0x65, 0x88, 0x84, 0x21];
const P_SLICE: &[u8] = &[0x41, 0x9A, 0x02];

/// A segment as FFmpeg writes it (`styp sidx moof mdat`) with reordered
/// video (composition offsets) and AAC audio: timestamps, keyframes, Annex B
/// and ADTS come out as a TS input's would.
#[test]
fn an_ffmpeg_shaped_segment_demuxes_like_a_ts_input() {
    let (avc1, mp4a) = (avc1(LengthSize::FourBytes), mp4a());
    let mut muxer = Fmp4SegmentMuxer::new().unwrap();
    let samples = [
        video(&avc1, 3000, true, 6000, length_prefixed(&[IDR], 4)),
        video(&avc1, 3000, false, 0, length_prefixed(&[P_SLICE], 4)),
        video(&avc1, 3000, false, 3000, length_prefixed(&[P_SLICE], 4)),
        audio(&mp4a, vec![0x21; 10]),
        audio(&mp4a, vec![0x22; 12]),
    ];
    let media = [ffmpeg_head(), segment(&mut muxer, &samples)].concat();
    let init = muxer.init_segment_bytes().unwrap();

    let packets = demux_all(&init, &[media], usize::MAX);
    let videos = of_type(&packets, MediaType::Video);
    let dts: Vec<i64> = videos.iter().map(|packet| packet.dts).collect();
    let pts: Vec<i64> = videos.iter().map(|packet| packet.pts).collect();
    assert_eq!(dts, vec![0, 33, 67], "3000 ticks of 90 kHz is 33.3 ms");
    assert_eq!(pts, vec![67, 33, 100], "PTS adds the composition offset");
    let keyframes: Vec<bool> = videos.iter().map(|packet| packet.is_keyframe).collect();
    assert_eq!(keyframes, vec![true, false, false]);

    let keyframe = &videos[0].payload;
    assert!(
        keyframe.starts_with(&[0, 0, 0, 1, 0x67]),
        "the avcC SPS/PPS lead a keyframe that lacks them"
    );
    assert!(keyframe.ends_with(&[&[0, 0, 0, 1][..], IDR].concat()));
    assert_eq!(
        &videos[1].payload[..],
        &[&[0, 0, 0, 1][..], P_SLICE].concat()[..]
    );

    let audios = of_type(&packets, MediaType::Audio);
    assert_eq!(
        audios.iter().map(|packet| packet.dts).collect::<Vec<_>>(),
        vec![0, 21]
    );
    assert!(audios.iter().all(|packet| !packet.is_keyframe));
    let header = build_adts_header_for(10, 2, 3, 2).unwrap();
    assert_eq!(
        &audios[0].payload[..],
        &[&header[..], &[0x21; 10]].concat()[..]
    );

    let sorted = packets.windows(2).all(|pair| pair[0].dts <= pair[1].dts);
    assert!(sorted, "released in decode order across tracks");
}

/// Chunks pace the release; whatever the chunk size, the same packets
/// leave in the same order.
#[test]
fn chunked_delivery_releases_the_same_packets() {
    let (avc1, mp4a) = (avc1(LengthSize::FourBytes), mp4a());
    let mut muxer = Fmp4SegmentMuxer::new().unwrap();
    let samples: Vec<In> = (0..8)
        .map(|index| video(&avc1, 3000, index == 0, 0, length_prefixed(&[P_SLICE], 4)))
        .chain((0..8).map(|_| audio(&mp4a, vec![0x11; 40])))
        .collect();
    let media = segment(&mut muxer, &samples);
    let init = muxer.init_segment_bytes().unwrap();
    let whole = demux_all(&init, std::slice::from_ref(&media), usize::MAX);
    let chunked = demux_all(&init, std::slice::from_ref(&media), 16);
    assert_eq!(whole.len(), 16);
    let key = |packets: &[MediaPacket]| {
        packets
            .iter()
            .map(|packet| (packet.media_type, packet.dts, packet.payload.clone()))
            .collect::<Vec<_>>()
    };
    assert_eq!(key(&whole), key(&chunked));
}

/// `avcC` length sizes 1, 2 and 4 each convert to Annex B; a keyframe that
/// carries its own SPS gets no second copy.
#[test]
fn every_nalu_length_size_converts_to_annex_b() {
    let sps = [0x67, 0x42, 0x00, 0x1E];
    for (length_size, width) in [
        (LengthSize::OneByte, 1),
        (LengthSize::TwoBytes, 2),
        (LengthSize::FourBytes, 4),
    ] {
        let entry = avc1(length_size);
        let mut muxer = Fmp4SegmentMuxer::new().unwrap();
        let samples = [
            video(&entry, 3000, true, 0, length_prefixed(&[&sps, IDR], width)),
            video(
                &entry,
                3000,
                false,
                0,
                length_prefixed(&[P_SLICE, P_SLICE], width),
            ),
        ];
        let media = segment(&mut muxer, &samples);
        let init = muxer.init_segment_bytes().unwrap();
        let packets = demux_all(&init, &[media], usize::MAX);
        let start = [0u8, 0, 0, 1];
        assert_eq!(
            &packets[0].payload[..],
            &[&start[..], &sps, &start, IDR].concat()[..],
            "length size {width}"
        );
        assert_eq!(
            &packets[1].payload[..],
            &[&start[..], P_SLICE, &start, P_SLICE].concat()[..],
            "length size {width}"
        );
    }
}

/// The ADTS fields come from the `esds` AudioSpecificConfig; what ADTS
/// cannot carry is refused.
#[test]
fn adts_fields_come_from_the_audio_specific_config() {
    // AAC-LC (2), 44.1 kHz (index 4), stereo (2).
    let config = AdtsConfig::from_audio_specific_config(&[0x12, 0x10]).unwrap();
    assert_eq!(
        config,
        AdtsConfig {
            object_type: 2,
            freq_index: 4,
            channel_config: 2
        }
    );
    assert_eq!((config.sample_rate(), config.channels()), (44_100, 2));
    // AAC Main (1), 48 kHz (3), 5.1 (6).
    let main = AdtsConfig::from_audio_specific_config(&[0x09, 0xB0]).unwrap();
    assert_eq!(
        (main.object_type, main.freq_index, main.channel_config),
        (1, 3, 6)
    );

    let refused = |asc: &[u8]| AdtsConfig::from_audio_specific_config(asc).unwrap_err();
    // HE-AAC (5) has no ADTS profile.
    assert!(matches!(
        refused(&[0x2A, 0x10]),
        MediaRejection::Unsupported(_)
    ));
    // An explicit 24-bit frequency (index 15).
    assert!(matches!(
        refused(&[0x17, 0x80]),
        MediaRejection::Unsupported(_)
    ));
    // Channel configuration 0 (defined in a PCE ADTS would need).
    assert!(matches!(
        refused(&[0x12, 0x00]),
        MediaRejection::Unsupported(_)
    ));
    // Reserved frequency index 13.
    assert!(matches!(
        refused(&[0x16, 0x90]),
        MediaRejection::Malformed(_)
    ));
    assert!(matches!(refused(&[0x12]), MediaRejection::Malformed(_)));
}

#[test]
fn ticks_convert_to_milliseconds_rounded_to_nearest() {
    assert_eq!(ticks_to_ms(3000, 90_000), Some(33));
    assert_eq!(ticks_to_ms(6000, 90_000), Some(67));
    assert_eq!(ticks_to_ms(1024, 48_000), Some(21));
    assert_eq!(ticks_to_ms(48_000, 48_000), Some(1000));
    assert_eq!(ticks_to_ms(-3000, 90_000), Some(-33));
    assert_eq!(ticks_to_ms(1, 0), None);
    assert_eq!(ticks_to_ms(i128::MAX, 1), None);
}

/// A segment whose `tfdt` jumps forward (segments lost upstream) is placed
/// by its own `tfdt`, not appended to the previous segment's end.
#[test]
fn a_forward_tfdt_skip_keeps_dts_on_the_source_clock() {
    let entry = avc1(LengthSize::FourBytes);
    let mut muxer = Fmp4SegmentMuxer::new().unwrap();
    let frame = |keyframe| length_prefixed(&[if keyframe { IDR } else { P_SLICE }], 4);
    let first = segment(
        &mut muxer,
        &[
            video(&entry, 3000, true, 0, frame(true)),
            video(&entry, 3000, false, 0, frame(false)),
        ],
    );
    // Ten seconds the ingest never receives.
    let _lost = segment(&mut muxer, &[video(&entry, 900_000, true, 0, frame(true))]);
    let after = segment(&mut muxer, &[video(&entry, 3000, true, 0, frame(true))]);
    let init = muxer.init_segment_bytes().unwrap();

    let packets = demux_all(&init, &[first, after], usize::MAX);
    let dts: Vec<i64> = packets.iter().map(|packet| packet.dts).collect();
    assert_eq!(dts, vec![0, 33, 10_067], "(6000 + 900000) / 90 kHz");
}

/// The init's tracks become the probe: H.264 metadata from the SPS, the
/// FLV sequence header from `avcC`, AAC from the AudioSpecificConfig.
#[test]
fn the_init_segment_becomes_the_probe() {
    let (avc1, mp4a) = (avc1(LengthSize::FourBytes), mp4a());
    let mut muxer = Fmp4SegmentMuxer::new().unwrap();
    segment(
        &mut muxer,
        &[
            video(&avc1, 3000, true, 0, length_prefixed(&[IDR], 4)),
            audio(&mp4a, vec![0; 8]),
        ],
    );
    let init = muxer.init_segment_bytes().unwrap();
    let mut demux = Fmp4Demux::default();
    demux.apply_init(&init, MAX_INIT).unwrap();
    let probe = demux.take_probe().unwrap();
    let video = probe.video.unwrap();
    assert_eq!(video.codec, "h264");
    assert!(video.width > 0 && video.height > 0);
    assert!(probe.video_sequence_header.is_some());
    assert_eq!(probe.video_track_count, 1);
    let audio = &probe.audio_tracks[0];
    assert_eq!(
        (audio.codec.as_str(), audio.sample_rate, audio.channels),
        ("aac", 48_000, 2)
    );
    assert!(demux.take_probe().is_none(), "one probe per init");

    assert!(matches!(
        Fmp4Demux::check_init(&init, init.len() - 1),
        Err(MediaRejection::TooLarge(_))
    ));
}

/// An HEVC init (`hev1` or `hvc1`) is accepted: its probe names HEVC at the
/// size the `hvcC` describes, and a keyframe that lacks the parameter sets
/// gets the VPS, SPS and PPS ahead of it, as an H.264 keyframe gets `avcC`'s.
#[test]
fn an_hevc_init_publishes_like_a_ts_hevc_input() {
    /// An IDR_W_RADL slice (NAL type 19) without its parameter sets.
    const HEVC_IDR: &[u8] = &[0x26, 0x01, 0xAF, 0x02];
    /// A TRAIL_R slice (NAL type 1).
    const HEVC_TRAIL: &[u8] = &[0x02, 0x01, 0xD0, 0x11];
    /// An SPS NAL header (type 33); its body is not read on this path.
    const HEVC_SPS: &[u8] = &[0x42, 0x01, 0x01];
    let keyframe = fixture_keyframe("h265");
    let config = H265SampleEntryConfig {
        length_size: LengthSize::FourBytes,
        avg_frame_rate: H265SampleEntryConfig::AVG_FRAME_RATE_UNSPECIFIED,
        constant_frame_rate: H265ConstantFrameRate::Unknown,
    };
    for entry in [
        SampleEntry::Hev1(build_hev1_box_from_annexb(&keyframe, &config).unwrap()),
        SampleEntry::Hvc1(build_hvc1_box_from_annexb(&keyframe, &config).unwrap()),
    ] {
        let (width, height) = match &entry {
            SampleEntry::Hev1(hevc) => (hevc.visual.width, hevc.visual.height),
            SampleEntry::Hvc1(hevc) => (hevc.visual.width, hevc.visual.height),
            _ => unreachable!(),
        };
        let mut muxer = Fmp4SegmentMuxer::new().unwrap();
        let media = segment(
            &mut muxer,
            &[
                video(&entry, 3000, true, 0, length_prefixed(&[HEVC_IDR], 4)),
                video(&entry, 3000, false, 0, length_prefixed(&[HEVC_TRAIL], 4)),
                video(
                    &entry,
                    3000,
                    true,
                    0,
                    length_prefixed(&[HEVC_SPS, HEVC_IDR], 4),
                ),
                video(&entry, 3000, true, 0, length_prefixed(&[HEVC_TRAIL], 4)),
            ],
        );
        let init = muxer.init_segment_bytes().unwrap();

        Fmp4Demux::check_init(&init, MAX_INIT).expect("an HEVC init is accepted");
        let mut demux = Fmp4Demux::default();
        demux
            .apply_init(&init, MAX_INIT)
            .expect("an HEVC init applies");
        let probe = demux.take_probe().expect("the init becomes the probe");
        assert_eq!(probe.video_track_count, 1);
        let video_meta = probe.video.expect("the probe describes the video");
        assert_eq!(video_meta.codec, "hevc");
        assert_eq!(
            (video_meta.width, video_meta.height),
            (u32::from(width), u32::from(height)),
            "the SPS gives the size the hvcC describes"
        );

        let packets = demux_all(&init, &[media], usize::MAX);
        let videos = of_type(&packets, MediaType::Video);
        assert_eq!(videos.len(), 4);
        assert!(videos[0].is_keyframe);
        let first = &videos[0].payload;
        assert!(
            first.starts_with(&[0, 0, 0, 1, 0x40]),
            "the hvcC VPS leads a keyframe that lacks it"
        );
        assert!(
            first.windows(5).any(|w| w == [0, 0, 0, 1, 0x42]),
            "and the SPS"
        );
        assert!(
            first.windows(5).any(|w| w == [0, 0, 0, 1, 0x44]),
            "and the PPS"
        );
        assert!(first.ends_with(&[&[0, 0, 0, 1][..], HEVC_IDR].concat()));
        assert_eq!(
            &videos[1].payload[..],
            &[&[0, 0, 0, 1][..], HEVC_TRAIL].concat()[..],
            "a non-keyframe is converted without parameter sets"
        );
        assert_eq!(
            &videos[2].payload[..],
            &[&[0, 0, 0, 1][..], HEVC_SPS, &[0, 0, 0, 1], HEVC_IDR].concat()[..],
            "a keyframe carrying its own SPS gets no second copy"
        );
        assert!(
            !videos[3].is_keyframe,
            "a sync-flagged sample without an IRAP picture is not a keyframe, as in TS"
        );
        assert_eq!(
            &videos[3].payload[..],
            &[&[0, 0, 0, 1][..], HEVC_TRAIL].concat()[..],
            "and gets no parameter sets"
        );
    }
}

#[test]
fn a_media_segment_needs_an_init_and_a_valid_walk() {
    let entry = avc1(LengthSize::FourBytes);
    let mut muxer = Fmp4SegmentMuxer::new().unwrap();
    let media = segment(
        &mut muxer,
        &[video(&entry, 3000, true, 0, length_prefixed(&[IDR], 4))],
    );
    let init = muxer.init_segment_bytes().unwrap();

    let mut demux = Fmp4Demux::default();
    assert!(matches!(
        demux.begin_object(&Bytes::from(media.clone())),
        Err(MediaRejection::Malformed(_))
    ));
    demux.apply_init(&init, MAX_INIT).unwrap();
    let truncated = Bytes::copy_from_slice(&media[..media.len() - 1]);
    assert!(matches!(
        demux.begin_object(&truncated),
        Err(MediaRejection::Malformed(_))
    ));
    demux.begin_object(&Bytes::from(media)).unwrap();
    demux.end_object().unwrap();
    let mut out = Vec::new();
    assert_eq!(demux.drain_into(&mut out), 1);
}

/// A sample whose NAL length runs past it is a truncated access unit: it is
/// refused, never published in part.
#[test]
fn a_truncated_access_unit_is_refused() {
    let entry = avc1(LengthSize::FourBytes);
    let mut muxer = Fmp4SegmentMuxer::new().unwrap();
    // The second sample claims a 16-byte NAL but carries four bytes.
    let mut truncated = 16u32.to_be_bytes().to_vec();
    truncated.extend_from_slice(&[0x41, 0x9A, 0x02, 0x33]);
    let media = segment(
        &mut muxer,
        &[
            video(&entry, 3000, true, 0, length_prefixed(&[IDR], 4)),
            video(&entry, 3000, false, 0, truncated),
        ],
    );
    let init = muxer.init_segment_bytes().unwrap();

    let mut demux = Fmp4Demux::default();
    demux.apply_init(&init, MAX_INIT).unwrap();
    demux.begin_object(&Bytes::from(media)).unwrap();
    let error = demux.end_object().unwrap_err();
    assert!(
        matches!(error, MediaRejection::Malformed(_)),
        "a truncated access unit is malformed, got {error:?}"
    );

    // A zero-length NAL is the same kind of damage.
    let mut zero = 0u32.to_be_bytes().to_vec();
    zero.extend_from_slice(&[0x41, 0x9A, 0x02, 0x33]);
    let mut muxer = Fmp4SegmentMuxer::new().unwrap();
    let media = segment(&mut muxer, &[video(&entry, 3000, true, 0, zero)]);
    let init = muxer.init_segment_bytes().unwrap();
    let mut demux = Fmp4Demux::default();
    demux.apply_init(&init, MAX_INIT).unwrap();
    demux.begin_object(&Bytes::from(media)).unwrap();
    assert!(matches!(
        demux.end_object(),
        Err(MediaRejection::Malformed(_))
    ));
}

/// One fragment much larger than a chunk — FFmpeg writes one `moof` + `mdat`
/// per segment — is released at the pace of the chunks that publish it, and
/// converted only as it is released: after half the chunks about half the
/// packets are out and no more are converted, so the segment neither enters
/// the ring in one burst nor is converted in one poll.
#[test]
fn a_large_fragment_is_released_and_converted_at_chunk_pace() {
    const CHUNK: usize = crate::media::ingest_publisher::PUBLISH_CHUNK_BYTES;
    let avc1 = avc1(LengthSize::FourBytes);
    let mut muxer = Fmp4SegmentMuxer::new().unwrap();
    // 64 samples of 16 KiB: one 1 MiB fragment, 16 chunks.
    let payload = length_prefixed(&[&[0x41; 16 * 1024 - 4][..]], 4);
    let samples: Vec<In> = (0..64)
        .map(|index| video(&avc1, 3000, index == 0, 0, payload.clone()))
        .collect();
    let media = segment(&mut muxer, &samples);
    let init = muxer.init_segment_bytes().unwrap();

    let mut demux = Fmp4Demux::default();
    demux.apply_init(&init, MAX_INIT).unwrap();
    demux.begin_object(&Bytes::from(media.clone())).unwrap();
    let converted = |demux: &Fmp4Demux| {
        demux
            .object
            .as_ref()
            .and_then(|object| object.releasing.as_ref())
            .map_or(samples.len(), |releasing| releasing.next)
    };
    let mut out = Vec::new();
    let chunks: Vec<&[u8]> = media.chunks(CHUNK).collect();
    let half = chunks.len() / 2;
    for (index, chunk) in chunks.iter().enumerate() {
        demux.feed(chunk).unwrap();
        demux.drain_into(&mut out);
        if index == 0 {
            assert!(
                converted(&demux) <= CHUNK / payload.len() + 1,
                "the first chunk converted {} samples",
                converted(&demux)
            );
        }
        if index + 1 == half {
            let expected = samples.len() / 2;
            assert!(
                out.len().abs_diff(expected) <= 2,
                "after half the chunks {} of {} packets are released",
                out.len(),
                samples.len()
            );
            assert_eq!(converted(&demux), out.len(), "conversion follows release");
        }
    }
    demux.end_object().unwrap();
    demux.drain_into(&mut out);
    assert_eq!(out.len(), samples.len(), "every sample is published once");
    assert!(out.windows(2).all(|pair| pair[0].dts <= pair[1].dts));
}

/// A sample that cannot be converted in the tail `end_object` releases
/// fails the segment, so the session counts and re-anchors it.
#[test]
fn a_sample_failing_at_the_end_of_an_object_fails_the_segment() {
    let avc1 = avc1(LengthSize::FourBytes);
    let mut muxer = Fmp4SegmentMuxer::new().unwrap();
    let good = length_prefixed(&[&[0x41; 16 * 1024 - 4][..]], 4);
    // A length field that runs past the sample: a truncated access unit.
    let mut truncated = good.clone();
    truncated[..4].copy_from_slice(&u32::MAX.to_be_bytes());
    let samples: Vec<In> = (0..8)
        .map(|index| {
            let data = if index == 7 {
                truncated.clone()
            } else {
                good.clone()
            };
            video(&avc1, 3000, index == 0, 0, data)
        })
        .collect();
    let media = segment(&mut muxer, &samples);
    let init = muxer.init_segment_bytes().unwrap();

    let mut demux = Fmp4Demux::default();
    demux.apply_init(&init, MAX_INIT).unwrap();
    demux.begin_object(&Bytes::from(media.clone())).unwrap();
    demux.feed(&media[..media.len() / 2]).unwrap();
    let error = demux.end_object().unwrap_err();
    assert!(
        error.to_string().contains("NALU length"),
        "unexpected error: {error}"
    );
}

/// A segment of many tiny top-level boxes is refused at the box bound, so
/// walking it cannot hold a shared worker for millions of iterations.
#[test]
fn a_segment_of_too_many_boxes_is_refused_at_the_walk_bound() {
    let free = [0, 0, 0, 8, b'f', b'r', b'e', b'e'];
    let at_bound: Vec<u8> = free.repeat(boxes::MAX_BOXES);
    let over: Vec<u8> = free.repeat(boxes::MAX_BOXES + 1);
    let mut fragments = Vec::new();
    assert_eq!(
        boxes::media_fragments(&at_bound, &mut fragments),
        Err(boxes::WalkError::NoFragments),
        "at the bound every box is walked"
    );
    assert_eq!(
        boxes::media_fragments(&over, &mut fragments),
        Err(boxes::WalkError::TooManyBoxes),
        "one box over the bound is refused"
    );
}
