//! Backend-neutral FFmpeg operation compiled from an `FfmpegStagePlan`.
//!
//! Both the external-process and in-process backends consume the same
//! `FfmpegOperation` so they do not independently interpret stage strings.

use crate::media::engine::{AudioMeta, VideoMeta};

/// Video encoder settings derived from a transcode profile.
#[derive(Clone, Debug, Default)]
pub struct VideoEncoderSettings {
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub bitrate: usize,
    pub max_bitrate: usize,
    pub gop: u32,
    pub bframes: usize,
    pub preset: String,
    pub tune: String,
    pub crf: i32,
    pub use_crf: bool,
}

/// Supported video codecs for stage output.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum VideoCodec {
    #[default]
    H264,
    Hevc,
}

/// Audio operation applied by the stage.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioOperation {
    /// Copy all audio tracks unchanged.
    CopyAll,
    /// Drop all audio.
    Drop,
    /// Select a subset of tracks by source index.
    SelectTracks(Vec<usize>),
    /// Downmix a single track to stereo.
    Downmix { track: usize },
    /// Remap channels of a single track.
    Remap { track: usize, channels: Vec<usize> },
}

/// Backend-neutral operation describing one FFmpeg stage.
#[derive(Clone, Debug)]
pub struct FfmpegOperation {
    pub input_codec: VideoCodec,
    pub output_codec: VideoCodec,
    pub scale: Option<(u32, u32)>,
    pub video_encoder: VideoEncoderSettings,
    pub audio: AudioOperation,
    pub video_meta: Option<VideoMeta>,
    pub audio_tracks: Vec<AudioMeta>,
}

impl FfmpegOperation {
    /// True if the stage changes the video resolution.
    pub fn scales(&self) -> bool {
        self.scale.is_some()
    }

    /// True if the stage changes the video codec.
    pub fn transcodes_codec(&self) -> bool {
        self.input_codec != self.output_codec
    }
}
