//! Central startup/join policy for live readers and FFmpeg probe budgets.

use crate::domain::output_spec::{OutputEncodingSpec, VideoCodecKind, VideoSelector};
use crate::media::profiles;

const DEFAULT_KEYFRAME_PREROLL_PACKETS: usize = 32;
const EXT_STAGE_ANALYZE_DURATION_US_DEFAULT: u64 = 500_000;
const EXT_STAGE_PROBE_SIZE_BYTES_DEFAULT: usize = 64 * 1024;
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
    if codec.is_hevc() {
        (
            EXT_STAGE_ANALYZE_DURATION_US_HEVC,
            EXT_STAGE_PROBE_SIZE_BYTES_HEVC,
        )
    } else {
        (
            EXT_STAGE_ANALYZE_DURATION_US_DEFAULT,
            EXT_STAGE_PROBE_SIZE_BYTES_DEFAULT,
        )
    }
}

pub fn ext_stage_passthrough_probe_budget() -> (u64, usize) {
    (
        EXT_STAGE_ANALYZE_DURATION_US_HEVC,
        EXT_STAGE_PROBE_SIZE_BYTES_HEVC,
    )
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
    fn external_stage_preroll_keeps_buffered_join_window() {
        assert_eq!(
            ext_stage_keyframe_preroll_packets(),
            DEFAULT_KEYFRAME_PREROLL_PACKETS
        );
    }
}
