use crate::domain::audio_routing::AudioRouting;

use super::protocol::{EgressProtocol, OutputProtocolConfig, RtmpOutputMode};
use super::video::{OutputVideoCodec, OutputVideoConfig, VideoCodecKind};

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(rename_all = "camelCase")]
pub struct OutputConfig {
    pub video: OutputVideoConfig,
    pub audio: AudioRouting,
    #[serde(default, skip_serializing_if = "OutputProtocolConfig::is_auto")]
    pub protocol: OutputProtocolConfig,
}

impl Default for OutputConfig {
    fn default() -> Self {
        Self {
            video: OutputVideoConfig::Source {
                codec: OutputVideoCodec::Auto,
            },
            audio: AudioRouting::Passthrough,
            protocol: OutputProtocolConfig::Auto,
        }
    }
}

impl OutputConfig {
    pub fn source() -> Self {
        Self::default()
    }

    pub fn preset(preset: impl Into<String>) -> Self {
        Self {
            video: OutputVideoConfig::Preset {
                preset: preset.into(),
                codec: OutputVideoCodec::Auto,
            },
            ..Self::default()
        }
    }

    pub fn with_video_codec(mut self, codec: OutputVideoCodec) -> Self {
        self.video.set_codec(codec);
        self
    }

    pub fn with_audio(mut self, audio: AudioRouting) -> Self {
        self.audio = audio;
        self
    }

    pub fn with_rtmp_mode(mut self, mode: RtmpOutputMode) -> Self {
        self.protocol = OutputProtocolConfig::Rtmp { mode };
        self
    }

    pub fn rtmp_mode(&self) -> RtmpOutputMode {
        self.protocol.rtmp_mode().unwrap_or_default()
    }

    pub fn stage_encoding_label(&self) -> String {
        let video = self.video.encoding_label();
        match self.audio.operation_string() {
            Some(audio) if matches!(self.video, OutputVideoConfig::Source { .. }) => audio,
            Some(audio) => format!("{video}+{audio}"),
            None => video.to_string(),
        }
    }

    pub fn is_custom_output(&self) -> bool {
        self.video.is_custom()
    }

    pub fn validate_capabilities(
        &self,
        capabilities: ProtocolCapabilities,
    ) -> Result<(), OutputConfigError> {
        let Some(codec) = self.video.codec().explicit_kind() else {
            return Ok(());
        };
        if capabilities.supports(codec) {
            Ok(())
        } else {
            Err(OutputConfigError::UnsupportedCodecForProtocol)
        }
    }

    pub fn resolve_for_input_codec(
        &self,
        capabilities: ProtocolCapabilities,
        input_codec: VideoCodecKind,
    ) -> Result<ResolvedOutputConfig, OutputConfigError> {
        self.validate_capabilities(capabilities)?;
        let codec = self
            .video
            .codec()
            .explicit_kind()
            .or_else(|| {
                capabilities
                    .supports(input_codec)
                    .then_some(input_codec)
                    .filter(|codec| !matches!(codec, VideoCodecKind::Unknown))
            })
            .unwrap_or_else(|| capabilities.default_codec());
        if !capabilities.supports(codec) {
            return Err(OutputConfigError::UnsupportedCodecForProtocol);
        }
        let video = match &self.video {
            OutputVideoConfig::Source { .. } | OutputVideoConfig::Custom => {
                ResolvedOutputVideo::Source { codec }
            }
            OutputVideoConfig::Preset { preset, .. } => ResolvedOutputVideo::Preset {
                preset: preset.clone(),
                codec,
            },
        };
        Ok(ResolvedOutputConfig {
            video,
            audio: self.audio.clone(),
            protocol: capabilities.protocol,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolCapabilities {
    pub protocol: EgressProtocol,
    pub rtmp_mode: Option<RtmpOutputMode>,
}

impl ProtocolCapabilities {
    pub fn from_output(url: &str, config: &OutputConfig) -> Self {
        let protocol = EgressProtocol::from_url(url);
        Self {
            protocol,
            rtmp_mode: protocol.is_rtmp().then(|| config.rtmp_mode()),
        }
    }

    pub const fn supports(self, codec: VideoCodecKind) -> bool {
        matches!(
            (self.protocol, self.rtmp_mode, codec),
            (
                EgressProtocol::Rtmp,
                Some(RtmpOutputMode::Legacy),
                VideoCodecKind::H264
            ) | (
                EgressProtocol::Rtmp,
                Some(RtmpOutputMode::Enhanced),
                VideoCodecKind::H264 | VideoCodecKind::Hevc,
            ) | (
                EgressProtocol::Srt,
                _,
                VideoCodecKind::H264 | VideoCodecKind::Hevc
            ) | (EgressProtocol::Hls, _, VideoCodecKind::H264)
                | (
                    EgressProtocol::Sink,
                    _,
                    VideoCodecKind::H264 | VideoCodecKind::Hevc | VideoCodecKind::Unknown,
                )
        )
    }

    pub const fn default_codec(self) -> VideoCodecKind {
        match (self.protocol, self.rtmp_mode) {
            (EgressProtocol::Rtmp, Some(RtmpOutputMode::Legacy)) | (EgressProtocol::Hls, _) => {
                VideoCodecKind::H264
            }
            _ => VideoCodecKind::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputConfigError {
    UnsupportedCodecForProtocol,
}

impl OutputConfigError {
    pub const fn message(self) -> &'static str {
        match self {
            Self::UnsupportedCodecForProtocol => {
                "Output video codec is not supported by the selected protocol mode"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedOutputVideo {
    Source {
        codec: VideoCodecKind,
    },
    Preset {
        preset: String,
        codec: VideoCodecKind,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedOutputConfig {
    pub video: ResolvedOutputVideo,
    pub audio: AudioRouting,
    pub protocol: EgressProtocol,
}
