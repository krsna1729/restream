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
#[derive(Clone, Debug)]
pub struct StageStartupPolicy {
    pub keyframe_preroll_packets: usize,
    pub require_video_parameter_sets: bool,
    pub wait_for_first_keyframe: bool,
}

impl Default for StageStartupPolicy {
    fn default() -> Self {
        Self {
            keyframe_preroll_packets: 0,
            require_video_parameter_sets: false,
            wait_for_first_keyframe: false,
        }
    }
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
