//! Typed parsing for output encodings, egress protocols, and codec families.

use crate::domain::audio_routing::is_audio_operation;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EgressProtocol {
    Rtmp,
    Srt,
    Hls,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutputUrlScheme {
    Rtmp,
    Rtmps,
    Srt,
    Hls,
    Http,
    Https,
    Unknown,
}

impl OutputUrlScheme {
    pub fn from_url(url: &str) -> Self {
        if url.starts_with("rtmp://") {
            Self::Rtmp
        } else if url.starts_with("rtmps://") {
            Self::Rtmps
        } else if url.starts_with("srt://") {
            Self::Srt
        } else if url.starts_with("hls://") {
            Self::Hls
        } else if url.starts_with("http://") {
            Self::Http
        } else if url.starts_with("https://") {
            Self::Https
        } else {
            Self::Unknown
        }
    }

    pub fn is_supported_output(self) -> bool {
        !matches!(self, Self::Unknown)
    }

    pub fn supports_monitoring(self) -> bool {
        matches!(self, Self::Srt | Self::Http | Self::Https)
    }

    pub fn is_rtmp_family(self) -> bool {
        matches!(self, Self::Rtmp | Self::Rtmps)
    }

    pub fn is_hls_family(self) -> bool {
        matches!(self, Self::Hls | Self::Http | Self::Https)
    }

    pub fn protocol(self) -> EgressProtocol {
        match self {
            Self::Rtmp | Self::Rtmps => EgressProtocol::Rtmp,
            Self::Srt => EgressProtocol::Srt,
            Self::Hls | Self::Http | Self::Https => EgressProtocol::Hls,
            Self::Unknown => EgressProtocol::Unknown,
        }
    }
}

impl EgressProtocol {
    pub fn from_url(url: &str) -> Self {
        OutputUrlScheme::from_url(url).protocol()
    }

    pub fn is_rtmp(self) -> bool {
        matches!(self, Self::Rtmp)
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rtmp => "rtmp",
            Self::Srt => "srt",
            Self::Hls => "hls",
            Self::Unknown => "unknown",
        }
    }
}

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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputEncodingSpec {
    video: VideoSelector,
    audio_operation: Option<String>,
}

impl OutputEncodingSpec {
    pub fn parse(encoding: &str) -> Self {
        let mut parts = encoding.splitn(2, '+');
        let first_part = parts.next().unwrap_or("source");
        let second_part = parts.next().filter(|value| !value.is_empty());
        let (video_part, audio_operation) = if is_audio_operation(first_part) {
            ("source", Some(first_part.to_string()))
        } else {
            (first_part, second_part.map(str::to_string))
        };

        let video = match video_part {
            "" | "source" => VideoSelector::Source,
            "custom" => VideoSelector::Custom,
            preset => VideoSelector::Preset(preset.to_string()),
        };

        Self {
            video,
            audio_operation,
        }
    }

    pub fn video(&self) -> &VideoSelector {
        &self.video
    }

    pub fn audio_operation(&self) -> Option<&str> {
        self.audio_operation.as_deref()
    }

    pub fn is_custom_output(&self) -> bool {
        self.video.is_custom()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePresetSpec {
    video: VideoSelector,
    audio_operation: Option<String>,
}

impl StagePresetSpec {
    pub fn parse(preset: &str) -> Self {
        if let Some(video) = preset.strip_prefix("video:") {
            return Self {
                video: match video {
                    "" | "source" => VideoSelector::Source,
                    "custom" => VideoSelector::Custom,
                    name => VideoSelector::Preset(name.to_string()),
                },
                audio_operation: None,
            };
        }

        if let Some(rest) = preset.strip_prefix("audio:") {
            let operation = rest.rsplit_once(":from:").map(|(op, _)| op).unwrap_or(rest);
            return Self {
                video: VideoSelector::Source,
                audio_operation: Some(operation.to_string()),
            };
        }

        let output = OutputEncodingSpec::parse(preset);
        Self {
            video: output.video,
            audio_operation: output.audio_operation,
        }
    }

    pub fn video(&self) -> &VideoSelector {
        &self.video
    }

    pub fn video_encoding(&self) -> &str {
        self.video.as_encoding_str()
    }

    pub fn audio_operation(&self) -> Option<&str> {
        self.audio_operation.as_deref()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_from_url_classifies_known_outputs() {
        assert_eq!(
            EgressProtocol::from_url("rtmp://example/live"),
            EgressProtocol::Rtmp
        );
        assert_eq!(
            EgressProtocol::from_url("rtmps://example/live"),
            EgressProtocol::Rtmp
        );
        assert_eq!(
            EgressProtocol::from_url("srt://example:9000"),
            EgressProtocol::Srt
        );
        assert_eq!(
            EgressProtocol::from_url("https://example/hls"),
            EgressProtocol::Hls
        );
        assert_eq!(
            EgressProtocol::from_url("udp://example"),
            EgressProtocol::Unknown
        );
    }

    #[test]
    fn output_url_scheme_tracks_specific_scheme_capabilities() {
        assert_eq!(
            OutputUrlScheme::from_url("rtmps://example/live"),
            OutputUrlScheme::Rtmps
        );
        assert!(OutputUrlScheme::from_url("https://example/out").supports_monitoring());
        assert!(!OutputUrlScheme::from_url("hls://preview").supports_monitoring());
        assert!(OutputUrlScheme::from_url("http://example/out").is_hls_family());
        assert!(OutputUrlScheme::from_url("rtmp://example/live").is_rtmp_family());
    }

    #[test]
    fn codec_kind_normalizes_hevc_aliases() {
        assert_eq!(
            VideoCodecKind::from_codec_name("h264"),
            VideoCodecKind::H264
        );
        assert_eq!(VideoCodecKind::from_codec_name("avc"), VideoCodecKind::H264);
        assert_eq!(
            VideoCodecKind::from_codec_name("h265"),
            VideoCodecKind::Hevc
        );
        assert_eq!(
            VideoCodecKind::from_codec_name("hevc"),
            VideoCodecKind::Hevc
        );
        assert_eq!(
            VideoCodecKind::from_codec_name("vp9"),
            VideoCodecKind::Unknown
        );
    }

    #[test]
    fn output_encoding_spec_parses_video_and_audio_parts() {
        let spec = OutputEncodingSpec::parse("720p+atrack:0");
        assert_eq!(spec.video(), &VideoSelector::Preset("720p".to_string()));
        assert_eq!(spec.audio_operation(), Some("atrack:0"));
    }

    #[test]
    fn output_encoding_spec_treats_standalone_audio_op_as_source() {
        let spec = OutputEncodingSpec::parse("downmix:1");
        assert_eq!(spec.video(), &VideoSelector::Source);
        assert_eq!(spec.audio_operation(), Some("downmix:1"));
    }

    #[test]
    fn output_encoding_spec_recognizes_passthrough_variants() {
        assert_eq!(
            OutputEncodingSpec::parse("source").video(),
            &VideoSelector::Source
        );
        assert_eq!(
            OutputEncodingSpec::parse("custom").video(),
            &VideoSelector::Custom
        );
    }

    #[test]
    fn output_encoding_spec_reports_custom_video_selector() {
        assert!(OutputEncodingSpec::parse("custom+atrack:0").is_custom_output());
    }

    #[test]
    fn stage_preset_spec_parses_stage_key_variants() {
        let video = StagePresetSpec::parse("video:720p");
        assert_eq!(video.video_encoding(), "720p");
        assert_eq!(video.audio_operation(), None);

        let audio = StagePresetSpec::parse("audio:downmix:1:from:video:720p");
        assert_eq!(audio.video_encoding(), "source");
        assert_eq!(audio.audio_operation(), Some("downmix:1"));

        let output = StagePresetSpec::parse("1080p+atrack:0");
        assert_eq!(output.video_encoding(), "1080p");
        assert_eq!(output.audio_operation(), Some("atrack:0"));
    }
}
