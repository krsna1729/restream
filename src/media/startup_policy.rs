//! Central startup/join policy for live readers and FFmpeg probe budgets.

use crate::domain::output_spec::{OutputEncodingSpec, VideoCodecKind, VideoSelector};
use crate::media::profiles;

const DEFAULT_KEYFRAME_PREROLL_PACKETS: usize = 32;
// These are fallbacks, not the normal live policy. Restream measures the
// aggregate encoded payload rate from the source ring and covers one bounded
// media-time window below. The fallbacks are used only when a stage is created
// before a useful rate sample exists; the cap prevents corrupt or extreme
// probe data from turning startup into an unbounded read.
const EXT_STAGE_ANALYZE_DURATION_US_DEFAULT: u64 = 1_000_000;
const EXT_STAGE_PROBE_SIZE_BYTES_DEFAULT: usize = 128 * 1024;
// External stages consume a persistent MPEG-TS pipe, not a finite file. A
// previous high-bitrate probe pass raised the *fallback* HEVC budget to
// 4 s / 2 MiB, which left a low-bitrate SRT HEVC publisher in `firstInput`
// until it had supplied 2 MiB; no downstream output could start meanwhile.
// `TsPacketFeeder` supplies VPS/SPS/PPS before frames, but AAC still needs a
// bounded window to establish its sample rate. Keep the no-observation fallback
// at 512 KiB / 1 s and allow the larger global cap only for measured live-rate
// data. That gives high-rate/VBR inputs room without regressing low-rate live
// startup. Keep the pipe-open HEVC regression test in
// `external_transcoder/ffmpeg_process.rs` paired with this policy.
const EXT_STAGE_ANALYZE_DURATION_US_HEVC: u64 = 1_000_000;
const EXT_STAGE_PROBE_SIZE_BYTES_HEVC: usize = 512 * 1024;
const EXT_STAGE_PROBE_SIZE_BYTES_AUDIO_TRACK: usize = 16 * 1024;
const EXT_STAGE_PROBE_SIZE_BYTES_MAX: usize = 2 * 1024 * 1024;
const EXT_STAGE_PROBE_RATE_MARGIN_NUMERATOR: u64 = 3;
const EXT_STAGE_PROBE_RATE_MARGIN_DENOMINATOR: u64 = 2;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExtStageProbeContext {
    pub codec: VideoCodecKind,
    pub include_audio: bool,
    pub audio_track_count: usize,
    pub passthrough: bool,
    /// Aggregate encoded payload bitrate observed by Restream's source ring.
    pub observed_bitrate_bps: Option<u64>,
}

impl ExtStageProbeContext {
    pub fn transcode(codec: VideoCodecKind) -> Self {
        Self {
            codec,
            include_audio: true,
            audio_track_count: 1,
            passthrough: false,
            observed_bitrate_bps: None,
        }
    }
}

pub fn rtmp_egress_keyframe_preroll_packets() -> usize {
    DEFAULT_KEYFRAME_PREROLL_PACKETS
}

pub fn recording_keyframe_preroll_packets() -> usize {
    DEFAULT_KEYFRAME_PREROLL_PACKETS
}

pub fn srt_egress_keyframe_preroll_packets(encoding: &str) -> usize {
    let spec = OutputEncodingSpec::parse(encoding);
    match spec.video() {
        VideoSelector::Preset(preset) => profiles::dimensions_for_preset(preset)
            .filter(|(_, height)| *height >= 1080)
            .map(|_| DEFAULT_KEYFRAME_PREROLL_PACKETS)
            .unwrap_or(0),
        VideoSelector::Source | VideoSelector::Custom => 0,
    }
}

pub fn ext_stage_probe_budget(codec: VideoCodecKind) -> (u64, usize) {
    ext_stage_probe_budget_for(ExtStageProbeContext::transcode(codec))
}

pub fn ext_stage_probe_budget_for(context: ExtStageProbeContext) -> (u64, usize) {
    let (analyze_duration_us, base_probe_size) = if context.codec.is_hevc() || context.passthrough {
        (
            EXT_STAGE_ANALYZE_DURATION_US_HEVC,
            EXT_STAGE_PROBE_SIZE_BYTES_HEVC,
        )
    } else {
        (
            EXT_STAGE_ANALYZE_DURATION_US_DEFAULT,
            EXT_STAGE_PROBE_SIZE_BYTES_DEFAULT,
        )
    };

    let observed_window_bytes = context.observed_bitrate_bps.map(|bitrate_bps| {
        bitrate_bps
            .saturating_mul(analyze_duration_us)
            .div_ceil(8 * 1_000_000)
            .saturating_mul(EXT_STAGE_PROBE_RATE_MARGIN_NUMERATOR)
            .div_ceil(EXT_STAGE_PROBE_RATE_MARGIN_DENOMINATOR) as usize
    });
    // The observed rate already includes every retained audio and video
    // payload. Do not clamp it to a codec-specific byte target or add another
    // per-audio allowance: either would turn a one-second live-data budget
    // back into a fixed byte wait for low-rate streams. Stream-shape fallbacks
    // remain useful only before Restream has a sufficiently long sample.
    let stream_probe_size = observed_window_bytes.unwrap_or(base_probe_size);

    let audio_tracks = if context.include_audio {
        context.audio_track_count
    } else {
        0
    };
    let audio_probe_size = if observed_window_bytes.is_some() {
        0
    } else {
        audio_tracks
            .saturating_sub(1)
            .saturating_mul(EXT_STAGE_PROBE_SIZE_BYTES_AUDIO_TRACK)
    };
    let probe_size_bytes = stream_probe_size
        .saturating_add(audio_probe_size)
        .min(EXT_STAGE_PROBE_SIZE_BYTES_MAX);

    (analyze_duration_us, probe_size_bytes)
}

pub fn ext_stage_passthrough_probe_budget() -> (u64, usize) {
    ext_stage_probe_budget_for(ExtStageProbeContext {
        codec: VideoCodecKind::Hevc,
        include_audio: true,
        audio_track_count: 1,
        passthrough: true,
        observed_bitrate_bps: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use proptest::prelude::*;

    #[test]
    fn srt_preroll_is_reserved_for_hd_presets_by_dimensions() {
        assert_eq!(srt_egress_keyframe_preroll_packets("source"), 0);
        assert_eq!(srt_egress_keyframe_preroll_packets("720p+atrack:0"), 0);
        assert_eq!(
            srt_egress_keyframe_preroll_packets("1080p"),
            DEFAULT_KEYFRAME_PREROLL_PACKETS
        );
    }

    #[test]
    fn srt_preroll_falls_back_to_zero_for_unknown_or_malformed_presets() {
        for encoding in ["", "not-a-real-preset", "1080p-typo", "custom"] {
            assert_eq!(
                srt_egress_keyframe_preroll_packets(encoding),
                0,
                "encoding={encoding:?} must fail safe to no preroll rather than panic"
            );
        }
    }

    #[test]
    fn rtmp_egress_preroll_matches_the_shared_default() {
        assert_eq!(
            rtmp_egress_keyframe_preroll_packets(),
            DEFAULT_KEYFRAME_PREROLL_PACKETS
        );
    }

    #[test]
    fn recording_preroll_matches_the_shared_default() {
        assert_eq!(
            recording_keyframe_preroll_packets(),
            DEFAULT_KEYFRAME_PREROLL_PACKETS
        );
    }

    #[test]
    fn ext_stage_probe_budget_prefers_hevc_budget_for_hevc_inputs() {
        assert_eq!(
            ext_stage_probe_budget(VideoCodecKind::H264),
            (
                EXT_STAGE_ANALYZE_DURATION_US_DEFAULT,
                EXT_STAGE_PROBE_SIZE_BYTES_DEFAULT,
            )
        );
        assert_eq!(
            ext_stage_probe_budget(VideoCodecKind::Hevc),
            (
                EXT_STAGE_ANALYZE_DURATION_US_HEVC,
                EXT_STAGE_PROBE_SIZE_BYTES_HEVC,
            )
        );
    }

    #[test]
    fn h264_probe_budget_covers_live_aac_header_without_file_style_delay() {
        assert_eq!(
            ext_stage_probe_budget(VideoCodecKind::H264),
            (1_000_000, 128 * 1024)
        );
    }

    #[test]
    fn external_passthrough_stage_keeps_full_probe_budget() {
        assert_eq!(
            ext_stage_passthrough_probe_budget(),
            (
                EXT_STAGE_ANALYZE_DURATION_US_HEVC,
                EXT_STAGE_PROBE_SIZE_BYTES_HEVC,
            )
        );
    }

    #[test]
    fn external_stage_probe_budget_scales_with_stream_count_not_resolution() {
        for (codec, base_probe_size) in [
            (VideoCodecKind::H264, 128 * 1024),
            (VideoCodecKind::Hevc, 512 * 1024),
        ] {
            for height in [240, 480, 720, 1080, 2160] {
                let _observed_source_height_from_probe = height;
                assert_eq!(
                    ext_stage_probe_budget_for(ExtStageProbeContext {
                        codec,
                        include_audio: true,
                        audio_track_count: 1,
                        passthrough: false,
                        observed_bitrate_bps: None,
                    }),
                    (1_000_000, base_probe_size),
                    "codec={codec:?} height={height}"
                );
            }
        }
    }

    #[test]
    fn external_stage_probe_budget_scales_for_h264_and_hevc_audio_counts() {
        for (codec, tracks, expected_probe_size) in [
            (VideoCodecKind::H264, 1, 128 * 1024),
            (VideoCodecKind::H264, 10, 272 * 1024),
            (VideoCodecKind::H264, 30, 592 * 1024),
            (VideoCodecKind::Hevc, 1, 512 * 1024),
            (VideoCodecKind::Hevc, 10, 656 * 1024),
            (VideoCodecKind::Hevc, 30, 976 * 1024),
        ] {
            assert_eq!(
                ext_stage_probe_budget_for(ExtStageProbeContext {
                    codec,
                    include_audio: true,
                    audio_track_count: tracks,
                    passthrough: false,
                    observed_bitrate_bps: None,
                }),
                (1_000_000, expected_probe_size),
                "codec={codec:?} tracks={tracks}"
            );
        }
    }

    #[test]
    fn video_only_external_stage_does_not_pay_multi_audio_probe_cost() {
        assert_eq!(
            ext_stage_probe_budget_for(ExtStageProbeContext {
                codec: VideoCodecKind::H264,
                include_audio: false,
                audio_track_count: 30,
                passthrough: false,
                observed_bitrate_bps: None,
            }),
            (1_000_000, 128 * 1024)
        );
    }

    #[test]
    fn external_stage_probe_budget_is_capped_to_avoid_memory_bloat() {
        assert_eq!(
            ext_stage_probe_budget_for(ExtStageProbeContext {
                codec: VideoCodecKind::Hevc,
                include_audio: true,
                audio_track_count: 100,
                passthrough: false,
                observed_bitrate_bps: None,
            }),
            (1_000_000, EXT_STAGE_PROBE_SIZE_BYTES_MAX)
        );
    }

    #[test]
    fn external_stage_probe_budget_uses_observed_payload_rate() {
        let budget = |observed_bitrate_bps| {
            ext_stage_probe_budget_for(ExtStageProbeContext {
                codec: VideoCodecKind::H264,
                include_audio: true,
                audio_track_count: 1,
                passthrough: false,
                observed_bitrate_bps: Some(observed_bitrate_bps),
            })
        };

        assert_eq!(budget(640_000), (1_000_000, 120_000));
        assert_eq!(budget(1_500_000), (1_000_000, 281_250));
        assert_eq!(budget(8_000_000), (1_000_000, 1_500_000));
    }

    #[test]
    fn observed_rate_replaces_codec_and_stream_shape_fallbacks() {
        let expected_one_second_window = 187_500;
        for codec in [VideoCodecKind::H264, VideoCodecKind::Hevc] {
            for audio_track_count in [0, 1, 30] {
                assert_eq!(
                    ext_stage_probe_budget_for(ExtStageProbeContext {
                        codec,
                        include_audio: audio_track_count > 0,
                        audio_track_count,
                        passthrough: codec.is_hevc(),
                        observed_bitrate_bps: Some(1_000_000),
                    }),
                    (1_000_000, expected_one_second_window),
                    "codec={codec:?} tracks={audio_track_count}"
                );
            }
        }
    }

    #[test]
    fn observed_rate_and_audio_count_compose_under_the_global_cap() {
        assert_eq!(
            ext_stage_probe_budget_for(ExtStageProbeContext {
                codec: VideoCodecKind::H264,
                include_audio: true,
                audio_track_count: 3,
                passthrough: false,
                observed_bitrate_bps: Some(1_500_000),
            }),
            (1_000_000, 281_250)
        );
        assert_eq!(
            ext_stage_probe_budget_for(ExtStageProbeContext {
                codec: VideoCodecKind::Hevc,
                include_audio: true,
                audio_track_count: 30,
                passthrough: false,
                observed_bitrate_bps: Some(8_000_000),
            }),
            (1_000_000, 1_500_000)
        );
        assert_eq!(
            ext_stage_probe_budget_for(ExtStageProbeContext {
                codec: VideoCodecKind::Hevc,
                include_audio: true,
                audio_track_count: 30,
                passthrough: false,
                observed_bitrate_bps: Some(20_000_000),
            }),
            (1_000_000, EXT_STAGE_PROBE_SIZE_BYTES_MAX)
        );
    }

    #[test]
    fn extreme_observed_bitrate_saturates_instead_of_overflowing_or_panicking() {
        // A corrupted or adversarial probe measurement could report an
        // absurd bitrate; the saturating arithmetic chain must still land
        // on the global cap rather than wrapping or panicking on overflow.
        for bitrate_bps in [u64::MAX, u64::MAX / 2, u64::MAX - 1] {
            assert_eq!(
                ext_stage_probe_budget_for(ExtStageProbeContext {
                    codec: VideoCodecKind::H264,
                    include_audio: true,
                    audio_track_count: 1,
                    passthrough: false,
                    observed_bitrate_bps: Some(bitrate_bps),
                }),
                (1_000_000, EXT_STAGE_PROBE_SIZE_BYTES_MAX),
                "bitrate_bps={bitrate_bps}"
            );
        }
    }

    proptest! {
        #![proptest_config(ProptestConfig::with_cases(128))]

        #[test]
        fn proptest_external_stage_probe_budget_stays_bounded_and_formula_driven(
            codec_is_hevc in any::<bool>(),
            include_audio in any::<bool>(),
            audio_track_count in 0usize..=128,
            passthrough in any::<bool>(),
            observed_bitrate_bps in prop::option::of(0u64..=80_000_000),
        ) {
            let codec = if codec_is_hevc {
                VideoCodecKind::Hevc
            } else {
                VideoCodecKind::H264
            };
            let context = ExtStageProbeContext {
                codec,
                include_audio,
                audio_track_count,
                passthrough,
                observed_bitrate_bps,
            };
            let (analyze_duration_us, probe_size_bytes) = ext_stage_probe_budget_for(context);

            prop_assert_eq!(analyze_duration_us, 1_000_000);
            prop_assert!(probe_size_bytes <= EXT_STAGE_PROBE_SIZE_BYTES_MAX);

            let base_probe_size = if codec.is_hevc() || passthrough {
                EXT_STAGE_PROBE_SIZE_BYTES_HEVC
            } else {
                EXT_STAGE_PROBE_SIZE_BYTES_DEFAULT
            };
            let expected = if let Some(bitrate_bps) = observed_bitrate_bps {
                bitrate_bps
                    .saturating_mul(analyze_duration_us)
                    .div_ceil(8 * 1_000_000)
                    .saturating_mul(EXT_STAGE_PROBE_RATE_MARGIN_NUMERATOR)
                    .div_ceil(EXT_STAGE_PROBE_RATE_MARGIN_DENOMINATOR) as usize
            } else {
                let audio_tracks = if include_audio { audio_track_count } else { 0 };
                base_probe_size.saturating_add(
                    audio_tracks
                        .saturating_sub(1)
                        .saturating_mul(EXT_STAGE_PROBE_SIZE_BYTES_AUDIO_TRACK),
                )
            }
            .min(EXT_STAGE_PROBE_SIZE_BYTES_MAX);

            prop_assert_eq!(probe_size_bytes, expected);
        }
    }
}
