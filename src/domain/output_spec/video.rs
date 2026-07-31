#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodecKind {
    H264,
    Hevc,
    Unknown,
}

impl VideoCodecKind {
    pub fn from_codec_name(codec: &str) -> Self {
        if codec.eq_ignore_ascii_case("h264") || codec.eq_ignore_ascii_case("avc") {
            Self::H264
        } else if codec.eq_ignore_ascii_case("h265") || codec.eq_ignore_ascii_case("hevc") {
            Self::Hevc
        } else {
            Self::Unknown
        }
    }

    pub fn is_hevc(self) -> bool {
        matches!(self, Self::Hevc)
    }

    pub const fn as_stage_codec(self) -> Option<&'static str> {
        match self {
            Self::H264 => Some("h264"),
            Self::Hevc => Some("hevc"),
            Self::Unknown => None,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum OutputVideoCodec {
    #[default]
    Auto,
    #[serde(rename = "h264")]
    H264,
    #[serde(rename = "h265", alias = "hevc")]
    Hevc,
}

impl OutputVideoCodec {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::H264 => "h264",
            Self::Hevc => "h265",
        }
    }

    pub const fn explicit_kind(self) -> Option<VideoCodecKind> {
        match self {
            Self::Auto => None,
            Self::H264 => Some(VideoCodecKind::H264),
            Self::Hevc => Some(VideoCodecKind::Hevc),
        }
    }

    pub const fn is_auto(&self) -> bool {
        matches!(self, Self::Auto)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VideoSelector {
    Source,
    Custom,
    Preset(String),
}

impl VideoSelector {
    pub fn stage_preset(&self) -> Option<&str> {
        match self {
            Self::Preset(name) => Some(name.as_str()),
            Self::Source | Self::Custom => None,
        }
    }

    pub fn as_encoding_str(&self) -> &str {
        match self {
            Self::Source => "source",
            Self::Custom => "custom",
            Self::Preset(name) => name.as_str(),
        }
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, Self::Custom)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum OutputVideoConfig {
    Source {
        #[serde(default, skip_serializing_if = "OutputVideoCodec::is_auto")]
        codec: OutputVideoCodec,
    },
    Preset {
        preset: String,
        #[serde(default, skip_serializing_if = "OutputVideoCodec::is_auto")]
        codec: OutputVideoCodec,
    },
    Custom,
}

impl OutputVideoConfig {
    pub fn encoding_label(&self) -> &str {
        match self {
            Self::Source { .. } => "source",
            Self::Preset { preset, .. } => preset.as_str(),
            Self::Custom => "custom",
        }
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, Self::Custom)
    }

    pub fn is_source_auto(&self) -> bool {
        matches!(
            self,
            Self::Source {
                codec: OutputVideoCodec::Auto
            }
        )
    }

    pub fn codec(&self) -> OutputVideoCodec {
        match self {
            Self::Source { codec } | Self::Preset { codec, .. } => *codec,
            Self::Custom => OutputVideoCodec::Auto,
        }
    }

    pub(super) fn set_codec(&mut self, codec: OutputVideoCodec) {
        match self {
            Self::Source { codec: current } | Self::Preset { codec: current, .. } => {
                *current = codec;
            }
            Self::Custom => {}
        }
    }
}
