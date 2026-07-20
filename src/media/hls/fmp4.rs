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

#[cfg(test)]
use std::num::NonZeroU32;

#[cfg(test)]
use bytes::Bytes;
#[cfg(test)]
use shiguredo_mp4::{
    TrackKind, Uint,
    boxes::{Avc1Box, AvccBox, SampleEntry, VisualSampleEntryFields},
    mux::{Fmp4SegmentMuxer, Sample},
};

#[cfg(test)]
use super::HlsConfig;
#[cfg(test)]
use crate::media::engine::MediaEngine;
#[cfg(test)]
use crate::media::metadata::{AudioMeta, VideoMeta};
#[cfg(test)]
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};

mod codec;
mod rendition;
mod segmenter;
mod store;

pub use segmenter::start_hls_fmp4_segmenter;
pub use store::Fmp4HlsStore;

#[cfg(test)]
use codec::{
    VIDEO_TIMESCALE, build_h264_sample_entry_from_flv_sequence_header,
    build_h264_sample_entry_from_video_packet, build_mux_samples, parse_avcc_box, rescale_ms,
};
#[cfg(test)]
use rendition::BufferedSample;
#[cfg(test)]
use segmenter::{relative_to_hls_zero_ms, resolve_hls_sequence_headers};
#[cfg(test)]
use store::PLAYLIST_RETENTION_GRACE_SEGMENTS;

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

    #[tokio::test]
    async fn input_preview_resolves_late_join_sequence_headers_from_input_session() {
        let engine = MediaEngine::new();
        let registration = engine
            .try_register_pipeline_input_attempt(
                "pipeline",
                "standby-input",
                "stream-key",
                "rtmp",
                false,
            )
            .await
            .expect("register standby input");
        let expected = Bytes::from(high_profile_sequence_header());
        engine
            .cache_ingest_session_sequence_header(&registration, true, expected.clone())
            .await;
        let resource_id = crate::media::engine_hls::input_hls_preview_resource_id("standby-input");

        let (video, audio) = resolve_hls_sequence_headers(&engine, &resource_id).await;

        assert_eq!(video, Some(expected));
        assert_eq!(audio, None);
    }

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
        assert!(playlist.contains("#EXT-X-TARGETDURATION:4"));
        assert!(playlist.contains("#EXTINF:2.250,\nseg0.m4s"));
        assert!(playlist.contains("#EXTINF:3.500,\nseg1.m4s"));
    }

    #[test]
    fn target_duration_starts_from_media_and_never_decreases() {
        let store = Fmp4HlsStore::with_config(HlsConfig {
            max_segments: 1,
            ..HlsConfig::default()
        });
        store.put_video_init_segment(Bytes::from_static(b"init"));
        store.push_video_segment(0, 3.5, Bytes::from_static(b"long"));
        store.push_video_segment(1, 1.0, Bytes::from_static(b"short"));

        let playlist = store.get_primary_playlist().expect("playlist");
        assert!(playlist.contains("#EXT-X-TARGETDURATION:4"));
        assert!(!playlist.contains("seg0.m4s"));
        assert!(playlist.contains("#EXTINF:1.000,\nseg1.m4s"));
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
    fn hls_boundary_timestamps_are_relative_to_preview_zero() {
        let zero_ms = 486_000_000;
        let next_segment_boundary_ms = zero_ms + 2_000;

        let relative_boundary_ms = relative_to_hls_zero_ms(next_segment_boundary_ms, zero_ms);

        assert_eq!(relative_boundary_ms, 2_000);
        assert_eq!(rescale_ms(relative_boundary_ms, VIDEO_TIMESCALE), 180_000);
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
    fn playlist_advertises_window_but_retains_grace_segments() {
        let store = Fmp4HlsStore::with_config(HlsConfig {
            max_segments: 3,
            ..HlsConfig::default()
        });
        store.put_video_init_segment(Bytes::from_static(b"init"));

        for index in 0..9u64 {
            store.push_video_segment(index, 1.0, Bytes::from(index.to_string()));
        }

        let playlist = store.get_video_playlist().expect("playlist");
        assert!(playlist.contains("#EXT-X-MEDIA-SEQUENCE:6"));
        assert!(!playlist.contains("\nseg5.m4s\n"));
        assert!(playlist.contains("\nseg6.m4s\n"));
        assert!(playlist.contains("\nseg8.m4s\n"));
        assert_eq!(playlist.matches(".m4s").count(), 3);
        assert!(store.get_video_segment(0).is_some());
        assert!(store.get_video_segment(6).is_some());

        store.push_video_segment(9, 1.0, Bytes::from_static(b"9"));

        assert!(store.get_video_segment(0).is_none());
        assert!(store.get_video_segment(6).is_some());
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
    fn avcc_box_rejects_sps_ok_but_missing_pps_count_byte() {
        // Truncate right after the valid SPS, before the mandatory numPPS
        // byte. A partial SPS-only sample entry would be worse than none
        // (playback can't decode without a PPS), so this must fail closed.
        let mut header = high_profile_sequence_header();
        header.truncate(38);
        assert!(
            build_h264_sample_entry_from_flv_sequence_header(&header, &test_video_meta()).is_none()
        );
    }

    #[test]
    fn avcc_box_rejects_sps_ok_but_pps_length_truncated() {
        // numPPS = 1 is present but the PPS length/body never arrives.
        let mut header = high_profile_sequence_header();
        header.truncate(39);
        assert!(
            build_h264_sample_entry_from_flv_sequence_header(&header, &test_video_meta()).is_none()
        );
    }

    #[test]
    fn avcc_box_rejects_max_declared_sps_length_with_tiny_buffer() {
        // Overwrite the declared SPS length (bytes 11..13) with 0xFFFF but
        // keep only the original short buffer trailing it.
        let mut header = high_profile_sequence_header();
        header[11] = 0xFF;
        header[12] = 0xFF;
        header.truncate(15);
        assert!(
            build_h264_sample_entry_from_flv_sequence_header(&header, &test_video_meta()).is_none()
        );
    }

    #[test]
    fn sps_exp_golomb_run_of_32_zero_bits_fails_closed_instead_of_panicking() {
        // seq_parameter_set_id is the first ue(v) field read from the SPS,
        // before the profile_idc early-return check. A run of 32 leading
        // zero bits there used to overflow `1u32 << leading_zero_bits` in
        // H264BitReader::read_exp_golomb (checked-shift panic in debug
        // builds, silent wraparound to a wrong value in release builds).
        let malformed_sps: Vec<u8> = vec![
            0x67, 0x64, 0x00, 0x1F, // nal header, profile_idc, constraints, level_idc
            0x00, 0x00, 0x00, 0x00, // 32 leading zero bits for seq_parameter_set_id's ue(v)
            0x80, 0xFF, 0xFF, 0xFF,
            0xFF, // terminating 1 bit + enough bits for the ue(v) suffix
        ];
        let mut header = vec![
            0x17, 0x00, 0x00, 0x00, 0x00, // FLV video header + AVC sequence header
            0x01, // configurationVersion
            0x64, // profile
            0x00, // profile compatibility
            0x1F, // level
            0xFF, // lengthSizeMinusOne = 3
            0xE1, // num SPS = 1
        ];
        header.extend_from_slice(&(malformed_sps.len() as u16).to_be_bytes());
        header.extend_from_slice(&malformed_sps);
        header.push(0); // num PPS = 0

        let sample_entry =
            build_h264_sample_entry_from_flv_sequence_header(&header, &test_video_meta())
                .expect("SPS/PPS list itself is well-formed and must still parse");
        let SampleEntry::Avc1(avc1) = sample_entry else {
            panic!("expected Avc1 sample entry");
        };
        assert!(
            avc1.avcc_box.chroma_format.is_none(),
            "malformed exp-golomb run must fail closed (no profile fields), not panic or wrap"
        );
    }

    proptest! {
        #[test]
        fn parse_avcc_box_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
            let _ = parse_avcc_box(&bytes);
        }

        #[test]
        fn parse_avcc_box_truncation_always_fails_closed(
            profile in any::<u8>(),
            compat in any::<u8>(),
            level in any::<u8>(),
            length_size in any::<u8>(),
            sps_bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..16), 0..3),
            pps_bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..16), 0..3),
        ) {
            let mut data = vec![0u8, profile, compat, level, length_size];
            data.push(0xE0 | (sps_bodies.len() as u8 & 0x1F));
            for sps in &sps_bodies {
                data.extend_from_slice(&(sps.len() as u16).to_be_bytes());
                data.extend_from_slice(sps);
            }
            data.push(pps_bodies.len() as u8);
            for pps in &pps_bodies {
                data.extend_from_slice(&(pps.len() as u16).to_be_bytes());
                data.extend_from_slice(pps);
            }

            let parsed = parse_avcc_box(&data).expect("well-formed input must parse");
            prop_assert_eq!(&parsed.sps_list, &sps_bodies);
            prop_assert_eq!(&parsed.pps_list, &pps_bodies);

            // Any strict prefix of a well-formed box must fail closed, never
            // yielding a partial SPS/PPS list.
            for cut in 0..data.len() {
                prop_assert!(
                    parse_avcc_box(&data[..cut]).is_none(),
                    "truncated at {cut} produced Some(..)"
                );
            }
        }
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

            let expected_retained =
                durations.len().min(max_segments + PLAYLIST_RETENTION_GRACE_SEGMENTS);
            prop_assert_eq!(store.segment_count(), expected_retained);

            let playlist = store.get_primary_playlist().expect("playlist must exist");
            let expected_advertised = durations.len().min(max_segments);
            let expected_first = durations.len().saturating_sub(expected_advertised) as u64;
            let expected_seq_line = format!("#EXT-X-MEDIA-SEQUENCE:{expected_first}");
            prop_assert!(playlist.contains(&expected_seq_line));
            prop_assert_eq!(playlist.matches(".m4s").count(), expected_advertised);
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
