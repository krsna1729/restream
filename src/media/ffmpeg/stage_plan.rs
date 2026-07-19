//! Typed stage plan for FFmpeg-backed stages.
//!
//! This is the narrow waist between the planner and the execution backends.
//! Both external (child-process) and internal (in-process libav) backends must
//! consume the same plan so they do not independently parse stringified stage
//! keys or preset names.

use crate::domain::stage::StageKey;
use crate::media::engine::{AudioMeta, VideoMeta};

/// Codec kinds that can appear in a stage plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VideoCodecKind {
    H264,
    Hevc,
}

impl VideoCodecKind {
    pub fn from_codec_name(name: &str) -> Self {
        match name.to_ascii_lowercase().as_str() {
            "hevc" | "h265" | "h.265" => Self::Hevc,
            _ => Self::H264,
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::Hevc => "hevc",
        }
    }
}

/// What the stage should do to video.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum VideoStageOp {
    /// Copy the video stream unchanged.
    Passthrough,
    /// Scale to a named preset (e.g. "720p", "1080p").
    ScalePreset { preset: String },
    /// Transcode from one codec to another.
    CodecEdge { op: CodecEdgeOp },
    /// Produce a browser-safe preview output.
    Preview { preset: String },
}

/// Well-known codec-edge operations.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CodecEdgeOp {
    HevcToH264,
}

/// What the stage should do to audio.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioStageOp {
    Passthrough,
    Drop,
    SelectTracks(Vec<usize>),
    Downmix { track: usize },
    Remap { track: usize, channels: Vec<usize> },
}

/// Input metadata required to configure the stage decoder/demuxer.
#[derive(Clone, Debug)]
pub struct StageInputSpec {
    pub codec_hint: VideoCodecKind,
    pub video_meta: Option<VideoMeta>,
    pub audio_tracks: Vec<AudioMeta>,
}

/// Startup discipline for the stage.
#[derive(Clone, Debug, Default)]
pub struct StageStartupPolicy {
    pub keyframe_preroll_packets: usize,
    pub require_video_parameter_sets: bool,
    pub wait_for_first_keyframe: bool,
}

/// Timeline normalization policy.
#[derive(Clone, Debug)]
pub struct TimelinePolicy {
    pub normalize_to_stage_zero: bool,
    pub unwrap_discontinuities: bool,
    pub enforce_dts_monotonicity: bool,
}

impl Default for TimelinePolicy {
    fn default() -> Self {
        Self {
            normalize_to_stage_zero: true,
            unwrap_discontinuities: true,
            enforce_dts_monotonicity: true,
        }
    }
}

/// Backend-neutral plan for a single FFmpeg stage.
#[derive(Clone, Debug)]
pub struct FfmpegStagePlan {
    pub stage_key: StageKey,
    pub pipeline_id: String,
    pub input: StageInputSpec,
    pub video: VideoStageOp,
    pub audio: AudioStageOp,
    pub output_codec: VideoCodecKind,
    pub output_profile: Option<String>,
    pub include_audio: bool,
    pub startup: StageStartupPolicy,
    pub timeline: TimelinePolicy,
}

impl FfmpegStagePlan {
    /// Convenience constructor for the common video-preset case.
    pub fn video_preset(
        stage_key: StageKey,
        pipeline_id: impl Into<String>,
        preset: impl Into<String>,
        input: StageInputSpec,
        output_codec: VideoCodecKind,
    ) -> Self {
        Self {
            stage_key,
            pipeline_id: pipeline_id.into(),
            input,
            video: VideoStageOp::ScalePreset {
                preset: preset.into(),
            },
            audio: AudioStageOp::Passthrough,
            output_codec,
            output_profile: None,
            include_audio: true,
            startup: StageStartupPolicy {
                keyframe_preroll_packets: 64,
                require_video_parameter_sets: true,
                wait_for_first_keyframe: true,
            },
            timeline: TimelinePolicy::default(),
        }
    }

    /// Convenience constructor for the HEVC→H.264 codec edge.
    pub fn hevc_to_h264(
        stage_key: StageKey,
        pipeline_id: impl Into<String>,
        input: StageInputSpec,
    ) -> Self {
        Self {
            stage_key,
            pipeline_id: pipeline_id.into(),
            input,
            video: VideoStageOp::CodecEdge {
                op: CodecEdgeOp::HevcToH264,
            },
            audio: AudioStageOp::Passthrough,
            output_codec: VideoCodecKind::H264,
            output_profile: None,
            include_audio: true,
            startup: StageStartupPolicy {
                keyframe_preroll_packets: 128,
                require_video_parameter_sets: true,
                wait_for_first_keyframe: true,
            },
            timeline: TimelinePolicy::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::stage::{StageKey as DomainStageKey, StageKind};

    fn input_spec() -> StageInputSpec {
        StageInputSpec {
            codec_hint: VideoCodecKind::H264,
            video_meta: None,
            audio_tracks: Vec::new(),
        }
    }

    #[test]
    fn from_codec_name_matches_hevc_spellings_case_insensitively() {
        for spelling in ["hevc", "HEVC", "Hevc", "h265", "H265", "h.265", "H.265"] {
            assert_eq!(
                VideoCodecKind::from_codec_name(spelling),
                VideoCodecKind::Hevc,
                "expected {spelling:?} to resolve to Hevc"
            );
        }
    }

    #[test]
    fn from_codec_name_defaults_unrecognized_inputs_to_h264() {
        for input in [
            "", "h264", "avc", "vp9", "av1", "mjpeg", "unknown", " hevc", "hevc ", "hevcx",
            "xhevc", "hvc1", "hev1", " ",
        ] {
            assert_eq!(
                VideoCodecKind::from_codec_name(input),
                VideoCodecKind::H264,
                "expected {input:?} to default to H264"
            );
        }
    }

    #[test]
    fn from_codec_name_handles_malformed_and_extreme_input() {
        // Non-ASCII lookalikes: `to_ascii_lowercase` only folds ASCII, so
        // Unicode homoglyphs of "hevc" must not accidentally match.
        assert_eq!(
            VideoCodecKind::from_codec_name("һevc"),
            VideoCodecKind::H264,
            "Cyrillic 'һ' homoglyph must not match ASCII 'h'"
        );

        // Embedded NUL and control bytes must not panic or be stripped into a match.
        assert_eq!(
            VideoCodecKind::from_codec_name("he\u{0}vc"),
            VideoCodecKind::H264
        );

        // Very long input must not panic and must fall through to the default.
        let long_garbage = "x".repeat(64 * 1024);
        assert_eq!(
            VideoCodecKind::from_codec_name(&long_garbage),
            VideoCodecKind::H264
        );

        // A long string that legitimately contains "hevc" as a substring but
        // is not an exact match must still default to H264 (no substring matching).
        let padded = format!("hevc{}", "y".repeat(4096));
        assert_eq!(
            VideoCodecKind::from_codec_name(&padded),
            VideoCodecKind::H264
        );
    }

    #[test]
    fn as_str_round_trips_through_from_codec_name() {
        for kind in [VideoCodecKind::H264, VideoCodecKind::Hevc] {
            assert_eq!(VideoCodecKind::from_codec_name(kind.as_str()), kind);
        }
    }

    #[test]
    fn video_preset_constructor_applies_expected_defaults() {
        let key = DomainStageKey::new("pipe-1", StageKind::video_preset("720p"));
        let plan = FfmpegStagePlan::video_preset(
            key,
            "pipe-1",
            "720p",
            input_spec(),
            VideoCodecKind::H264,
        );

        assert_eq!(
            plan.video,
            VideoStageOp::ScalePreset {
                preset: "720p".to_string()
            }
        );
        assert_eq!(plan.audio, AudioStageOp::Passthrough);
        assert_eq!(plan.output_codec, VideoCodecKind::H264);
        assert_eq!(plan.output_profile, None);
        assert!(plan.include_audio);
        assert_eq!(plan.startup.keyframe_preroll_packets, 64);
        assert!(plan.startup.require_video_parameter_sets);
        assert!(plan.startup.wait_for_first_keyframe);
        assert!(plan.timeline.normalize_to_stage_zero);
        assert!(plan.timeline.unwrap_discontinuities);
        assert!(plan.timeline.enforce_dts_monotonicity);
    }

    #[test]
    fn hevc_to_h264_constructor_applies_expected_defaults() {
        let key = DomainStageKey::new(
            "pipe-1",
            StageKind::codec_edge("hevc_to_h264", StageKind::source()),
        );
        // The constructor's output codec is always H264 regardless of the
        // codec hint carried in the input spec (it names the *output*, not
        // the input the edge is converting from).
        let mut input = input_spec();
        input.codec_hint = VideoCodecKind::Hevc;
        let plan = FfmpegStagePlan::hevc_to_h264(key, "pipe-1", input);

        assert_eq!(
            plan.video,
            VideoStageOp::CodecEdge {
                op: CodecEdgeOp::HevcToH264
            }
        );
        assert_eq!(plan.output_codec, VideoCodecKind::H264);
        assert_eq!(plan.input.codec_hint, VideoCodecKind::Hevc);
        assert_eq!(plan.startup.keyframe_preroll_packets, 128);
        assert!(plan.startup.require_video_parameter_sets);
        assert!(plan.startup.wait_for_first_keyframe);
    }
}
