use std::sync::Arc;

use crate::application::hls_preview::{
    HlsPreviewReadError, build_h264_codec_string, build_hevc_codec_string, build_hls_audio_codec,
    build_hls_audio_track_name, build_hls_codec_list, build_hls_master_playlist,
    build_hls_video_codec, estimate_audio_bandwidth, estimate_h264_level_idc,
    estimate_hls_master_bandwidth, parse_h264_level_idc, parse_h265_level_tenths, primary_playlist,
    quote_hls_attr, video_segment,
};
use crate::domain::stage::{StageKey, StageKind};
use crate::media::engine::MediaEngine;
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};
use crate::media::stage_runtime::StageRuntimeManager;

#[tokio::test]
async fn primary_playlist_reports_graph_planned_blocked_stage_cause() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "app-hls-preview-blocked";
    engine
        .try_register_ingest(pipeline_id, "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "hevc".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    engine.ensure_hls_preview_segmenter(pipeline_id).await;

    let stage_key = StageKey::new(pipeline_id, StageKind::preview("720p", StageKind::source()));
    let manager = StageRuntimeManager::new(engine.clone());
    let (handle, _) = manager
        .ensure_stage(stage_key.clone(), Arc::new(RingBuffer::new(16)), None)
        .await;
    handle.lifecycle.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    let err = primary_playlist(engine, pipeline_id).await.unwrap_err();

    assert_eq!(
        err,
        HlsPreviewReadError::NoSegments {
            blocked_by: Some(crate::application::hls_preview::HlsPreviewBlockedCause {
                stage: stage_key.to_string(),
                phase: "waitingForCapacity".to_string(),
            })
        }
    );
}

#[tokio::test]
async fn video_segment_rejects_invalid_segment_name_in_application_service() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "app-hls-preview-invalid-segment";
    engine.ensure_hls_preview_segmenter(pipeline_id).await;

    let err = video_segment(engine, pipeline_id, "init.mp4")
        .await
        .unwrap_err();

    assert_eq!(err, HlsPreviewReadError::InvalidSegmentName);
}

// -- quote_hls_attr --

#[test]
fn quote_hls_attr_wraps_empty_string() {
    assert_eq!(quote_hls_attr(""), "\"\"");
}

#[test]
fn quote_hls_attr_escapes_backslash_before_quote() {
    // Escaping order matters: escaping `\` first, then `"`, avoids
    // double-escaping quote characters that came from the backslash step.
    let input = "back\\slash\"quote";
    assert_eq!(quote_hls_attr(input), "\"back\\\\slash\\\"quote\"");
}

#[test]
fn quote_hls_attr_escapes_repeated_quotes() {
    assert_eq!(quote_hls_attr("\"\""), "\"\\\"\\\"\"");
}

// -- build_hls_audio_track_name --

#[test]
fn audio_track_name_prefers_trimmed_title() {
    let track = AudioMeta {
        title: Some("  Commentary  ".to_string()),
        language: Some("en".to_string()),
        ..Default::default()
    };
    assert_eq!(build_hls_audio_track_name(&track, 0), "Commentary");
}

#[test]
fn audio_track_name_falls_back_to_language_when_title_is_whitespace() {
    let track = AudioMeta {
        title: Some("   ".to_string()),
        language: Some(" es ".to_string()),
        ..Default::default()
    };
    assert_eq!(build_hls_audio_track_name(&track, 1), "Track 2 (es)");
}

#[test]
fn audio_track_name_falls_back_to_ordinal_only_when_title_and_language_absent() {
    let track = AudioMeta {
        title: None,
        language: None,
        ..Default::default()
    };
    assert_eq!(build_hls_audio_track_name(&track, 2), "Track 3");
}

#[test]
fn audio_track_name_treats_whitespace_only_language_as_absent() {
    let track = AudioMeta {
        title: None,
        language: Some("   ".to_string()),
        ..Default::default()
    };
    assert_eq!(build_hls_audio_track_name(&track, 0), "Track 1");
}

// -- estimate_hls_master_bandwidth / estimate_audio_bandwidth --

#[test]
fn master_bandwidth_falls_back_to_default_when_no_video_or_audio() {
    assert_eq!(estimate_hls_master_bandwidth(None, &[]), 8_000_000);
}

#[test]
fn master_bandwidth_ignores_non_finite_or_non_positive_video_bandwidth() {
    for bw in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 0.0, -1.0] {
        let video = VideoMeta {
            bw: Some(bw),
            ..Default::default()
        };
        assert_eq!(
            estimate_hls_master_bandwidth(Some(&video), &[]),
            8_000_000,
            "bw={bw} should be treated as absent"
        );
    }
}

#[test]
fn master_bandwidth_never_reports_zero() {
    // A video bandwidth that rounds down to 0 with no audio tracks must
    // still surface a non-zero BANDWIDTH attribute (players may treat 0
    // as invalid).
    let video = VideoMeta {
        bw: Some(0.4),
        ..Default::default()
    };
    assert_eq!(estimate_hls_master_bandwidth(Some(&video), &[]), 1);
}

#[test]
fn master_bandwidth_saturates_instead_of_overflowing() {
    let video = VideoMeta {
        bw: Some(f64::MAX),
        ..Default::default()
    };
    let audio = vec![AudioMeta {
        codec: "aac".to_string(),
        channels: 2,
        ..Default::default()
    }];
    // Must not panic; saturates at u64::MAX rather than wrapping.
    let bandwidth = estimate_hls_master_bandwidth(Some(&video), &audio);
    assert_eq!(bandwidth, u64::MAX);
}

#[test]
fn audio_bandwidth_table_covers_known_codecs_and_channel_tiers() {
    let cases = [
        ("aac", 0, 96_000),
        ("aac", 1, 96_000),
        ("aac", 2, 128_000),
        ("aac", 6, 192_000),
        ("mp3", 2, 128_000),
        ("opus", 0, 64_000),
        ("opus", 2, 128_000),
        ("opus", 6, 160_000),
        ("unknown_codec", 2, 128_000),
    ];
    for (codec, channels, expected) in cases {
        let track = AudioMeta {
            codec: codec.to_string(),
            channels,
            ..Default::default()
        };
        assert_eq!(
            estimate_audio_bandwidth(&track),
            expected,
            "codec={codec} channels={channels}"
        );
    }
}

#[test]
fn audio_bandwidth_codec_match_is_case_insensitive() {
    let track = AudioMeta {
        codec: "AAC".to_string(),
        channels: 2,
        ..Default::default()
    };
    assert_eq!(estimate_audio_bandwidth(&track), 128_000);
}

// -- build_hls_codec_list / build_hls_video_codec / build_hls_audio_codec --

#[test]
fn codec_list_is_none_when_video_and_audio_are_unrecognized() {
    let video = VideoMeta {
        codec: "vp9".to_string(),
        ..Default::default()
    };
    let audio = vec![AudioMeta {
        codec: "flac".to_string(),
        ..Default::default()
    }];
    assert_eq!(build_hls_codec_list(Some(&video), &audio), None);
}

#[test]
fn codec_list_dedupes_identical_audio_codec_strings() {
    let audio = vec![
        AudioMeta {
            codec: "aac".to_string(),
            ..Default::default()
        },
        AudioMeta {
            codec: "aac".to_string(),
            ..Default::default()
        },
    ];
    assert_eq!(
        build_hls_codec_list(None, &audio),
        Some("mp4a.40.2".to_string())
    );
}

#[test]
fn video_codec_is_unrecognized_for_unknown_codec_string() {
    let video = VideoMeta {
        codec: "vp9".to_string(),
        ..Default::default()
    };
    assert_eq!(build_hls_video_codec(&video), None);
}

#[test]
fn video_codec_matches_h264_case_and_whitespace_insensitively() {
    let video = VideoMeta {
        codec: "  H264 ".to_string(),
        ..Default::default()
    };
    assert!(build_hls_video_codec(&video).is_some());
}

#[test]
fn video_codec_av1_uses_fixed_codec_string() {
    let video = VideoMeta {
        codec: "av1".to_string(),
        ..Default::default()
    };
    assert_eq!(
        build_hls_video_codec(&video),
        Some("av01.0.08M.08".to_string())
    );
}

#[test]
fn audio_codec_aac_profile_variants_and_default() {
    let cases = [
        (Some("Main"), "mp4a.40.1"),
        (Some("SSR"), "mp4a.40.3"),
        (Some("LTP/Reserved"), "mp4a.40.4"),
        (Some("LC"), "mp4a.40.2"),
        (None, "mp4a.40.2"),
    ];
    for (profile, expected) in cases {
        let track = AudioMeta {
            codec: "aac".to_string(),
            profile: profile.map(str::to_string),
            ..Default::default()
        };
        assert_eq!(
            build_hls_audio_codec(&track),
            Some(expected.to_string()),
            "profile={profile:?}"
        );
    }
}

#[test]
fn audio_codec_unknown_codec_returns_none() {
    let track = AudioMeta {
        codec: "flac".to_string(),
        ..Default::default()
    };
    assert_eq!(build_hls_audio_codec(&track), None);
}

// -- H.264 level/profile handling --

#[test]
fn h264_codec_string_maps_known_profiles_to_profile_idc() {
    let cases = [
        (Some("Baseline"), "avc1.4200"),
        (Some("Main"), "avc1.4d00"),
        (Some("Extended"), "avc1.5800"),
        (Some("High"), "avc1.6400"),
        (Some("High 10"), "avc1.6e00"),
        (Some("High 4:2:2"), "avc1.7a00"),
        (Some("High 4:4:4 Predictive"), "avc1.f400"),
        (Some("Unrecognized Profile"), "avc1.6400"),
        (None, "avc1.6400"),
    ];
    for (profile, expected_prefix) in cases {
        let video = VideoMeta {
            codec: "h264".to_string(),
            profile: profile.map(str::to_string),
            level: Some("4.0".to_string()),
            ..Default::default()
        };
        let codec = build_h264_codec_string(&video).unwrap();
        assert!(
            codec.starts_with(expected_prefix),
            "profile={profile:?} got {codec}"
        );
        assert_eq!(codec, format!("{expected_prefix}28"));
    }
}

#[test]
fn h264_codec_string_falls_back_to_estimated_level_on_malformed_level() {
    let low_res = VideoMeta {
        codec: "h264".to_string(),
        width: 640,
        height: 360,
        fps: 30.0,
        level: Some("not-a-level".to_string()),
        ..Default::default()
    };
    assert_eq!(build_h264_codec_string(&low_res).unwrap(), "avc1.64001f");
}

#[test]
fn parse_h264_level_idc_accepts_none_and_rejects_empty() {
    assert_eq!(parse_h264_level_idc(None), None);
    assert_eq!(parse_h264_level_idc(Some("")), None);
    assert_eq!(parse_h264_level_idc(Some("   ")), None);
}

#[test]
fn parse_h264_level_idc_treats_missing_dot_as_zero_minor() {
    assert_eq!(parse_h264_level_idc(Some("4")), Some(40));
}

#[test]
fn parse_h264_level_idc_trims_whitespace_around_parts() {
    assert_eq!(parse_h264_level_idc(Some(" 4 . 1 ")), Some(41));
}

#[test]
fn parse_h264_level_idc_rejects_extra_dot_segments() {
    assert_eq!(parse_h264_level_idc(Some("4.1.2")), None);
}

#[test]
fn parse_h264_level_idc_rejects_non_numeric_parts() {
    assert_eq!(parse_h264_level_idc(Some("a.b")), None);
    assert_eq!(parse_h264_level_idc(Some("4.b")), None);
}

#[test]
fn parse_h264_level_idc_rejects_major_overflowing_u8() {
    assert_eq!(parse_h264_level_idc(Some("300.0")), None);
}

#[test]
fn parse_h264_level_idc_saturates_rather_than_overflows() {
    // 99 * 10 + 9 = 999, which does not fit in a u8; saturating
    // arithmetic must clamp to u8::MAX instead of panicking or wrapping.
    assert_eq!(parse_h264_level_idc(Some("99.9")), Some(u8::MAX));
}

#[test]
fn estimate_h264_level_idc_uses_high_tier_for_large_resolution() {
    let video = VideoMeta {
        width: 1920,
        height: 1080,
        fps: 30.0,
        ..Default::default()
    };
    assert_eq!(estimate_h264_level_idc(&video), 40);
}

#[test]
fn estimate_h264_level_idc_handles_zero_dimensions_and_non_finite_fps() {
    for fps in [0.0, -1.0, f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        let video = VideoMeta {
            width: 0,
            height: 0,
            fps,
            ..Default::default()
        };
        // Must not panic (zero dims, div_ceil, non-finite fps) and must
        // fall back to a sane low tier.
        assert_eq!(estimate_h264_level_idc(&video), 31, "fps={fps}");
    }
}

#[test]
fn estimate_h264_level_idc_mid_tier_boundary_at_720p60() {
    let video = VideoMeta {
        width: 1280,
        height: 720,
        fps: 60.0,
        ..Default::default()
    };
    // Exactly 216_000 macroblocks/sec: not `> 216_000.0`, so this must
    // land in the mid tier (32), not the high tier (40).
    assert_eq!(estimate_h264_level_idc(&video), 32);
}

#[test]
fn estimate_h264_level_idc_low_tier_boundary_at_720p29() {
    let video = VideoMeta {
        width: 1280,
        height: 720,
        fps: 29.0,
        ..Default::default()
    };
    assert_eq!(estimate_h264_level_idc(&video), 31);
}

// -- HEVC level/profile handling --

#[test]
fn hevc_codec_string_maps_known_profiles() {
    let base = VideoMeta {
        codec: "hevc".to_string(),
        level: Some("4.0".to_string()),
        ..Default::default()
    };
    let cases = [
        (Some("Main"), "hvc1.1.6.L120.B0"),
        (Some("Main 10"), "hvc1.2.6.L120.B0"),
        (Some("Main Still Picture"), "hvc1.3.6.L120.B0"),
        (Some("Unrecognized"), "hvc1.1.6.L120.B0"),
        (None, "hvc1.1.6.L120.B0"),
    ];
    for (profile, expected) in cases {
        let video = VideoMeta {
            profile: profile.map(str::to_string),
            ..base.clone()
        };
        assert_eq!(build_hevc_codec_string(&video), expected, "{profile:?}");
    }
}

#[test]
fn hevc_codec_string_defaults_to_sane_level_when_level_is_missing() {
    // Regression: the default fallback used to be expressed in the wrong
    // unit (a raw `general_level_idc`-looking constant fed into the
    // "level tenths -> general_level_idc" conversion), producing
    // `L360` -- a level far beyond the HEVC spec's max (L186, level 6.2)
    // -- instead of a sane default around level 4.0 (`L120`).
    let video = VideoMeta {
        codec: "hevc".to_string(),
        level: None,
        ..Default::default()
    };
    assert_eq!(build_hevc_codec_string(&video), "hvc1.1.6.L120.B0");
}

#[test]
fn hevc_codec_string_defaults_when_level_is_malformed() {
    let video = VideoMeta {
        codec: "hevc".to_string(),
        level: Some("garbage".to_string()),
        ..Default::default()
    };
    assert_eq!(build_hevc_codec_string(&video), "hvc1.1.6.L120.B0");
}

#[test]
fn hevc_codec_string_maps_high_level() {
    let video = VideoMeta {
        codec: "hevc".to_string(),
        level: Some("5.1".to_string()),
        ..Default::default()
    };
    assert_eq!(build_hevc_codec_string(&video), "hvc1.1.6.L153.B0");
}

#[test]
fn parse_h265_level_tenths_rejects_empty_and_whitespace() {
    assert_eq!(parse_h265_level_tenths(""), None);
    assert_eq!(parse_h265_level_tenths("   "), None);
}

#[test]
fn parse_h265_level_tenths_treats_missing_dot_as_zero_minor() {
    assert_eq!(parse_h265_level_tenths("5"), Some(50));
}

#[test]
fn parse_h265_level_tenths_saturates_rather_than_overflows() {
    assert_eq!(parse_h265_level_tenths("99.9"), Some(u8::MAX));
}

// -- build_hls_master_playlist --

#[test]
fn master_playlist_escapes_quotes_in_audio_track_title() {
    let audio = vec![AudioMeta {
        codec: "aac".to_string(),
        title: Some("Cast \"Live\"".to_string()),
        track_index: 0,
        ..Default::default()
    }];
    let playlist = build_hls_master_playlist(None, &audio);
    assert!(
        playlist.contains("NAME=\"Cast \\\"Live\\\"\""),
        "playlist did not contain properly escaped NAME attribute: {playlist}"
    );
}

#[test]
fn master_playlist_marks_only_first_audio_track_as_default() {
    let audio = vec![
        AudioMeta {
            codec: "aac".to_string(),
            track_index: 0,
            ..Default::default()
        },
        AudioMeta {
            codec: "aac".to_string(),
            track_index: 1,
            ..Default::default()
        },
    ];
    let playlist = build_hls_master_playlist(None, &audio);
    let default_yes = playlist.matches("DEFAULT=YES").count();
    let default_no = playlist.matches("DEFAULT=NO").count();
    assert_eq!(default_yes, 1);
    assert_eq!(default_no, 1);
}

#[test]
fn master_playlist_omits_resolution_and_frame_rate_when_absent_or_invalid() {
    let video = VideoMeta {
        codec: "h264".to_string(),
        width: 0,
        height: 0,
        fps: f64::NAN,
        ..Default::default()
    };
    let playlist = build_hls_master_playlist(Some(&video), &[]);
    assert!(!playlist.contains("RESOLUTION="));
    assert!(!playlist.contains("FRAME-RATE="));
}
