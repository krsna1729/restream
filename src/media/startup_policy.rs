//! Central startup/join policy for live readers and FFmpeg probe budgets.

use crate::domain::output_spec::{OutputEncodingSpec, VideoCodecKind, VideoSelector};
use crate::media::profiles;

const DEFAULT_KEYFRAME_PREROLL_PACKETS: usize = 32;
// These are safety floors, not the normal live policy. Restream measures the
// encoded payload rate from the source ring and covers one bounded media-time
// window below. The floor handles a stage created before a useful rate sample
// exists; the cap prevents a high-rate source from turning probe data into an
// unbounded allocation or multi-second startup wait.
const EXT_STAGE_ANALYZE_DURATION_US_DEFAULT: u64 = 1_000_000;
const EXT_STAGE_PROBE_SIZE_BYTES_DEFAULT: usize = 128 * 1024;
// External stages consume a persistent MPEG-TS pipe, not a finite file. A
// previous high-bitrate probe pass raised HEVC to 4 s / 2 MiB, which left a
// low-bitrate SRT HEVC publisher in `firstInput` until it had supplied 2 MiB;
// no downstream output could start meanwhile. `TsPacketFeeder` supplies
// VPS/SPS/PPS before frames, but AAC still needs a bounded window to establish
// its sample rate. 512 KiB / 1 s is sufficient for that header while staying
// below the live-progress budget; it must never drift back to a multi-megabyte
// file-style probe. Keep the pipe-open HEVC regression test in
// `external_transcoder/ffmpeg_process.rs` paired with this policy.
const EXT_STAGE_ANALYZE_DURATION_US_HEVC: u64 = 1_000_000;
const EXT_STAGE_PROBE_SIZE_BYTES_HEVC: usize = 512 * 1024;
const EXT_STAGE_PROBE_SIZE_BYTES_AUDIO_TRACK: usize = 16 * 1024;
const EXT_STAGE_PROBE_SIZE_BYTES_MAX: usize = 1024 * 1024;
const EXT_STAGE_PROBE_RATE_MARGIN_NUMERATOR: u64 = 5;
const EXT_STAGE_PROBE_RATE_MARGIN_DENOMINATOR: u64 = 4;

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

pub fn ext_stage_keyframe_preroll_packets() -> usize {
    DEFAULT_KEYFRAME_PREROLL_PACKETS
}

pub fn internal_transcoder_keyframe_preroll_packets() -> usize {
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
    let stream_probe_size = observed_window_bytes
        .unwrap_or(base_probe_size)
        .max(base_probe_size);

    let audio_tracks = if context.include_audio {
        context.audio_track_count
    } else {
        0
    };
    let audio_probe_size = audio_tracks
        .saturating_sub(1)
        .saturating_mul(EXT_STAGE_PROBE_SIZE_BYTES_AUDIO_TRACK);
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
    fn external_stage_preroll_keeps_buffered_join_window() {
        assert_eq!(
            ext_stage_keyframe_preroll_packets(),
            DEFAULT_KEYFRAME_PREROLL_PACKETS
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

        assert_eq!(budget(640_000), (1_000_000, 128 * 1024));
        assert_eq!(budget(1_500_000), (1_000_000, 234_375));
        assert_eq!(budget(8_000_000), (1_000_000, 1024 * 1024));
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
            (1_000_000, 267_143)
        );
        assert_eq!(
            ext_stage_probe_budget_for(ExtStageProbeContext {
                codec: VideoCodecKind::Hevc,
                include_audio: true,
                audio_track_count: 30,
                passthrough: false,
                observed_bitrate_bps: Some(8_000_000),
            }),
            (1_000_000, 1024 * 1024)
        );
    }
}
