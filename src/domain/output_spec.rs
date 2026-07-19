//! Typed parsing for output encodings, egress protocols, codec families, and
//! output configuration payloads.

use crate::domain::audio_routing::{AudioRouting, is_audio_operation};

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
        match url::Url::parse(url.trim())
            .ok()
            .map(|parsed| parsed.scheme().to_ascii_lowercase())
            .as_deref()
        {
            Some("rtmp") => Self::Rtmp,
            Some("rtmps") => Self::Rtmps,
            Some("srt") => Self::Srt,
            Some("hls") => Self::Hls,
            Some("http") => Self::Http,
            Some("https") => Self::Https,
            _ => Self::Unknown,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RtmpOutputMode {
    Legacy,
    Enhanced,
}

impl Default for RtmpOutputMode {
    fn default() -> Self {
        Self::Legacy
    }
}

impl RtmpOutputMode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Legacy => "legacy",
            Self::Enhanced => "enhanced",
        }
    }

    pub fn is_legacy(&self) -> bool {
        matches!(self, Self::Legacy)
    }

    pub fn is_enhanced(self) -> bool {
        matches!(self, Self::Enhanced)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum OutputProtocolConfig {
    Auto,
    Rtmp {
        #[serde(default)]
        mode: RtmpOutputMode,
    },
}

impl Default for OutputProtocolConfig {
    fn default() -> Self {
        Self::Auto
    }
}

impl OutputProtocolConfig {
    pub fn is_auto(&self) -> bool {
        matches!(self, Self::Auto)
    }

    pub fn rtmp_mode(&self) -> Option<RtmpOutputMode> {
        match self {
            Self::Rtmp { mode } => Some(*mode),
            Self::Auto => None,
        }
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

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
            video: OutputVideoConfig::Source,
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
            },
            ..Self::default()
        }
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
            Some(audio) if matches!(self.video, OutputVideoConfig::Source) => audio,
            Some(audio) => format!("{video}+{audio}"),
            None => video.to_string(),
        }
    }

    pub fn is_custom_output(&self) -> bool {
        self.video.is_custom()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "mode", rename_all = "camelCase")]
pub enum OutputVideoConfig {
    Source,
    Preset { preset: String },
    Custom,
}

impl OutputVideoConfig {
    pub fn encoding_label(&self) -> &str {
        match self {
            Self::Source => "source",
            Self::Preset { preset } => preset.as_str(),
            Self::Custom => "custom",
        }
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, Self::Custom)
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
        assert_eq!(
            OutputUrlScheme::from_url(" RTMP://EXAMPLE/live "),
            OutputUrlScheme::Rtmp
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
    fn output_config_serde_uses_typed_shape() {
        let config =
            OutputConfig::source().with_audio(AudioRouting::SelectTracks { tracks: vec![0, 2] });
        let value = serde_json::to_value(&config).unwrap();
        assert_eq!(
            value,
            serde_json::json!({
                "video": {"mode": "source"},
                "audio": {"mode": "selectTracks", "tracks": [0, 2]}
            })
        );
    }

    #[test]
    fn output_url_scheme_from_url_covers_every_variant_and_malformed_input() {
        assert_eq!(
            OutputUrlScheme::from_url("rtmp://example/live"),
            OutputUrlScheme::Rtmp
        );
        assert_eq!(
            OutputUrlScheme::from_url("rtmps://example/live"),
            OutputUrlScheme::Rtmps
        );
        assert_eq!(
            OutputUrlScheme::from_url("srt://example:9000"),
            OutputUrlScheme::Srt
        );
        assert_eq!(
            OutputUrlScheme::from_url("hls://example/out"),
            OutputUrlScheme::Hls
        );
        assert_eq!(
            OutputUrlScheme::from_url("http://example/out"),
            OutputUrlScheme::Http
        );
        assert_eq!(
            OutputUrlScheme::from_url("https://example/out"),
            OutputUrlScheme::Https
        );
        assert_eq!(
            OutputUrlScheme::from_url("udp://example"),
            OutputUrlScheme::Unknown
        );
        assert_eq!(OutputUrlScheme::from_url(""), OutputUrlScheme::Unknown);
        assert_eq!(
            OutputUrlScheme::from_url("not a url at all"),
            OutputUrlScheme::Unknown
        );
        assert_eq!(
            OutputUrlScheme::from_url("://missing-scheme"),
            OutputUrlScheme::Unknown
        );
    }

    #[test]
    fn output_url_scheme_is_supported_output_is_false_only_for_unknown() {
        assert!(OutputUrlScheme::Rtmp.is_supported_output());
        assert!(OutputUrlScheme::Rtmps.is_supported_output());
        assert!(OutputUrlScheme::Srt.is_supported_output());
        assert!(OutputUrlScheme::Hls.is_supported_output());
        assert!(OutputUrlScheme::Http.is_supported_output());
        assert!(OutputUrlScheme::Https.is_supported_output());
        assert!(!OutputUrlScheme::Unknown.is_supported_output());
    }

    #[test]
    fn output_url_scheme_family_and_protocol_classification_is_exhaustive() {
        let cases = [
            (
                OutputUrlScheme::Rtmp,
                true,
                false,
                false,
                EgressProtocol::Rtmp,
            ),
            (
                OutputUrlScheme::Rtmps,
                true,
                false,
                false,
                EgressProtocol::Rtmp,
            ),
            (
                OutputUrlScheme::Srt,
                false,
                false,
                true,
                EgressProtocol::Srt,
            ),
            (
                OutputUrlScheme::Hls,
                false,
                true,
                false,
                EgressProtocol::Hls,
            ),
            (
                OutputUrlScheme::Http,
                false,
                true,
                true,
                EgressProtocol::Hls,
            ),
            (
                OutputUrlScheme::Https,
                false,
                true,
                true,
                EgressProtocol::Hls,
            ),
            (
                OutputUrlScheme::Unknown,
                false,
                false,
                false,
                EgressProtocol::Unknown,
            ),
        ];
        for (scheme, is_rtmp_family, is_hls_family, supports_monitoring, protocol) in cases {
            assert_eq!(
                scheme.is_rtmp_family(),
                is_rtmp_family,
                "is_rtmp_family for {scheme:?}"
            );
            assert_eq!(
                scheme.is_hls_family(),
                is_hls_family,
                "is_hls_family for {scheme:?}"
            );
            assert_eq!(
                scheme.supports_monitoring(),
                supports_monitoring,
                "supports_monitoring for {scheme:?}"
            );
            assert_eq!(scheme.protocol(), protocol, "protocol for {scheme:?}");
        }
    }

    #[test]
    fn egress_protocol_is_rtmp_and_as_str_cover_every_variant() {
        assert!(EgressProtocol::Rtmp.is_rtmp());
        assert!(!EgressProtocol::Srt.is_rtmp());
        assert!(!EgressProtocol::Hls.is_rtmp());
        assert!(!EgressProtocol::Unknown.is_rtmp());

        assert_eq!(EgressProtocol::Rtmp.as_str(), "rtmp");
        assert_eq!(EgressProtocol::Srt.as_str(), "srt");
        assert_eq!(EgressProtocol::Hls.as_str(), "hls");
        assert_eq!(EgressProtocol::Unknown.as_str(), "unknown");
    }

    #[test]
    fn video_codec_kind_is_hevc_is_true_only_for_hevc() {
        assert!(!VideoCodecKind::H264.is_hevc());
        assert!(VideoCodecKind::Hevc.is_hevc());
        assert!(!VideoCodecKind::Unknown.is_hevc());
    }

    #[test]
    fn video_selector_stage_preset_and_as_encoding_str_and_is_custom() {
        let source = VideoSelector::Source;
        assert_eq!(source.stage_preset(), None);
        assert_eq!(source.as_encoding_str(), "source");
        assert!(!source.is_custom());

        let custom = VideoSelector::Custom;
        assert_eq!(custom.stage_preset(), None);
        assert_eq!(custom.as_encoding_str(), "custom");
        assert!(custom.is_custom());

        let preset = VideoSelector::Preset("720p".to_string());
        assert_eq!(preset.stage_preset(), Some("720p"));
        assert_eq!(preset.as_encoding_str(), "720p");
        assert!(!preset.is_custom());
    }

    #[test]
    fn output_video_config_is_custom_and_encoding_label() {
        assert_eq!(OutputVideoConfig::Source.encoding_label(), "source");
        assert!(!OutputVideoConfig::Source.is_custom());

        assert_eq!(OutputVideoConfig::Custom.encoding_label(), "custom");
        assert!(OutputVideoConfig::Custom.is_custom());

        let preset = OutputVideoConfig::Preset {
            preset: "480p".to_string(),
        };
        assert_eq!(preset.encoding_label(), "480p");
        assert!(!preset.is_custom());
    }

    #[test]
    fn output_config_is_custom_output_reflects_video_selector() {
        assert!(!OutputConfig::default().is_custom_output());
        assert!(
            OutputConfig {
                video: OutputVideoConfig::Custom,
                ..OutputConfig::default()
            }
            .is_custom_output()
        );
        assert!(!OutputConfig::preset("720p").is_custom_output());
    }

    #[test]
    fn output_config_defaults_missing_protocol_to_auto_legacy_rtmp() {
        let value = serde_json::json!({
            "video": {"mode": "source"},
            "audio": {"mode": "all"}
        });

        let config: OutputConfig = serde_json::from_value(value).unwrap();

        assert_eq!(config.protocol, OutputProtocolConfig::Auto);
        assert_eq!(config.rtmp_mode(), RtmpOutputMode::Legacy);
    }

    #[test]
    fn output_config_serializes_enhanced_rtmp_mode_under_protocol() {
        let config = OutputConfig::source().with_rtmp_mode(RtmpOutputMode::Enhanced);
        let value = serde_json::to_value(&config).unwrap();

        assert_eq!(
            value["protocol"],
            serde_json::json!({"type": "rtmp", "mode": "enhanced"})
        );
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
