//! Every stage in the media graph has a typed `StageKind` that encodes its
//! function (video preset, audio route, codec edge, infrastructure). The
//! `StageKey` pairs a `PipelineId` with a `StageKind` for use as a typed
//! map key in engine registries. No string-based stage identity is used at
//! runtime.

use std::fmt;

pub use crate::domain::ids::{PipelineId, StageId};

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct WorkerId(String);

impl WorkerId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for WorkerId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StageKind {
    Source,
    VideoPreset {
        preset: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        output_codec: Option<String>,
    },
    AudioRoute {
        operation: String,
        upstream: Box<StageKind>,
    },
    CodecEdge {
        operation: String,
        upstream: Box<StageKind>,
    },
    Preview {
        preset: String,
        upstream: Box<StageKind>,
    },
    HlsSegmenter {
        upstream: Box<StageKind>,
    },
    Hls,
    Recording,
}

impl StageKind {
    pub fn source() -> Self {
        Self::Source
    }

    pub fn video_preset(preset: impl Into<String>) -> Self {
        Self::VideoPreset {
            preset: preset.into(),
            output_codec: None,
        }
    }

    pub fn video_preset_with_codec(
        preset: impl Into<String>,
        output_codec: impl Into<String>,
    ) -> Self {
        Self::VideoPreset {
            preset: preset.into(),
            output_codec: Some(output_codec.into()),
        }
    }

    pub fn audio_route(operation: impl Into<String>, upstream: StageKind) -> Self {
        Self::AudioRoute {
            operation: operation.into(),
            upstream: Box::new(upstream),
        }
    }

    pub fn codec_edge(operation: impl Into<String>, upstream: StageKind) -> Self {
        Self::CodecEdge {
            operation: operation.into(),
            upstream: Box::new(upstream),
        }
    }

    pub fn preview(preset: impl Into<String>, upstream: StageKind) -> Self {
        Self::Preview {
            preset: preset.into(),
            upstream: Box::new(upstream),
        }
    }

    pub fn hls() -> Self {
        Self::Hls
    }

    pub fn hls_segmenter(upstream: StageKind) -> Self {
        Self::HlsSegmenter {
            upstream: Box::new(upstream),
        }
    }

    pub fn recording() -> Self {
        Self::Recording
    }

    pub fn graph_node_id(&self, pipeline_id: &str) -> String {
        let slug = self.to_string().replace([':', '+', ','], "_");
        format!("{pipeline_id}_{slug}_stage")
    }

    pub fn graph_label(&self) -> String {
        match self {
            Self::Source => "Source".to_string(),
            Self::Hls => "HLS Preview".to_string(),
            Self::HlsSegmenter { .. } => "fMP4 Segmenter".to_string(),
            Self::Recording => "MKV Recording".to_string(),
            Self::VideoPreset {
                preset,
                output_codec,
            } => match output_codec {
                Some(codec) => format!("Video: {preset} ({codec})"),
                None => format!("Video: {preset}"),
            },
            Self::AudioRoute { operation, .. } => format!("Audio: {operation}"),
            Self::CodecEdge { operation, .. } => match operation.as_str() {
                "hevc_to_h264" => "HEVC -> H.264".to_string(),
                other => format!("Codec edge: {other}"),
            },
            Self::Preview { preset, .. } => format!("Preview: {preset}"),
        }
    }

    pub fn graph_type(&self) -> &'static str {
        match self {
            Self::AudioRoute { .. } => "audio_filter",
            Self::CodecEdge { .. } => "codec_edge",
            Self::Source => "source",
            Self::Hls | Self::HlsSegmenter { .. } => "hls",
            Self::Recording => "recording",
            Self::VideoPreset { .. } => "transcoder",
            Self::Preview { .. } => "preview",
        }
    }

    pub fn upstream(&self) -> Option<&StageKind> {
        match self {
            Self::AudioRoute { upstream, .. }
            | Self::CodecEdge { upstream, .. }
            | Self::Preview { upstream, .. }
            | Self::HlsSegmenter { upstream } => Some(upstream),
            _ => None,
        }
    }

    pub fn is_preview(&self) -> bool {
        matches!(self, Self::Preview { .. })
    }

    pub fn is_video_preset(&self) -> bool {
        matches!(self, Self::VideoPreset { .. })
    }

    pub fn is_video_processing(&self) -> bool {
        matches!(self, Self::VideoPreset { .. } | Self::Preview { .. })
    }

    pub fn audio_operation(&self) -> Option<&str> {
        match self {
            Self::AudioRoute { operation, .. } => Some(operation.as_str()),
            _ => None,
        }
    }

    /// The video preset name for video stages, used by downstream audio and
    /// codec-edge stages to refer to their upstream in Display output.
    pub fn preset_name(&self) -> Option<&str> {
        match self {
            Self::VideoPreset { preset, .. } => Some(preset.as_str()),
            _ => None,
        }
    }

    pub fn video_output_codec(&self) -> Option<&str> {
        match self {
            Self::VideoPreset { output_codec, .. } => output_codec.as_deref(),
            _ => None,
        }
    }
}

impl fmt::Display for StageKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Source => f.write_str("source"),
            Self::Hls => f.write_str("hls"),
            Self::HlsSegmenter { upstream } if upstream.as_ref() == &StageKind::Source => {
                f.write_str("hls")
            }
            Self::HlsSegmenter { upstream } => write!(f, "hls:from:{upstream}"),
            Self::Recording => f.write_str("recording"),
            Self::VideoPreset {
                preset,
                output_codec,
            } => match output_codec {
                Some(codec) => write!(f, "video:{preset}:codec:{codec}"),
                None => write!(f, "video:{preset}"),
            },
            Self::AudioRoute {
                operation,
                upstream,
            } => write!(f, "audio:{operation}:from:{upstream}"),
            Self::CodecEdge {
                operation,
                upstream,
            } => write!(f, "{operation}:from:{upstream}"),
            Self::Preview { preset, upstream } => write!(f, "preview:{preset}:from:{upstream}"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct StageKey {
    pub pipeline: PipelineId,
    pub kind: StageKind,
}

impl StageKey {
    pub fn new(pipeline: impl Into<PipelineId>, kind: StageKind) -> Self {
        Self {
            pipeline: pipeline.into(),
            kind,
        }
    }
}

impl fmt::Display for StageKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.pipeline, self.kind)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stage_kind_display_round_trips() {
        let cases: Vec<(StageKind, &str)> = vec![
            (StageKind::source(), "source"),
            (StageKind::hls(), "hls"),
            (StageKind::recording(), "recording"),
            (StageKind::video_preset("720p"), "video:720p"),
            (
                StageKind::audio_route("atrack:0", StageKind::video_preset("720p")),
                "audio:atrack:0:from:video:720p",
            ),
            (
                StageKind::codec_edge("hevc_to_h264", StageKind::source()),
                "hevc_to_h264:from:source",
            ),
            (
                StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("720p")),
                "hevc_to_h264:from:video:720p",
            ),
        ];
        for (kind, expected) in cases {
            assert_eq!(kind.to_string(), expected, "Display for {:?}", kind);
        }
    }

    #[test]
    fn stage_key_display() {
        let key = StageKey::new("pipe", StageKind::video_preset("720p"));
        assert_eq!(key.to_string(), "pipe:video:720p");
    }

    #[test]
    fn audio_route_upstream_is_accessible() {
        let kind = StageKind::audio_route("atrack:0", StageKind::video_preset("720p"));
        assert_eq!(kind.graph_label(), "Audio: atrack:0");
        assert_eq!(*kind.upstream().unwrap(), StageKind::video_preset("720p"));
    }

    #[test]
    fn graph_node_id_sanitizes_display_separators_into_the_slug() {
        let kind = StageKind::audio_route("atrack:0", StageKind::video_preset("720p"));
        // Display renders this as "audio:atrack:0:from:video:720p"; the
        // node-id slug must replace ':' (and '+', ',') so the id is safe to
        // embed as a single graph-node token.
        assert_eq!(
            kind.graph_node_id("pipe"),
            "pipe_audio_atrack_0_from_video_720p_stage"
        );
    }

    #[test]
    fn graph_type_matches_every_stage_kind() {
        let cases: Vec<(StageKind, &str)> = vec![
            (StageKind::source(), "source"),
            (StageKind::hls(), "hls"),
            (StageKind::hls_segmenter(StageKind::source()), "hls"),
            (StageKind::recording(), "recording"),
            (StageKind::video_preset("720p"), "transcoder"),
            (
                StageKind::audio_route("atrack:0", StageKind::source()),
                "audio_filter",
            ),
            (
                StageKind::codec_edge("hevc_to_h264", StageKind::source()),
                "codec_edge",
            ),
            (StageKind::preview("720p", StageKind::source()), "preview"),
        ];
        for (kind, expected) in cases {
            assert_eq!(kind.graph_type(), expected, "graph_type for {kind:?}");
        }
    }

    #[test]
    fn is_preview_and_is_video_preset_and_is_video_processing_are_mutually_precise() {
        let preview = StageKind::preview("720p", StageKind::source());
        assert!(preview.is_preview());
        assert!(!preview.is_video_preset());
        assert!(preview.is_video_processing());

        let video_preset = StageKind::video_preset("720p");
        assert!(!video_preset.is_preview());
        assert!(video_preset.is_video_preset());
        assert!(video_preset.is_video_processing());

        let source = StageKind::source();
        assert!(!source.is_preview());
        assert!(!source.is_video_preset());
        assert!(!source.is_video_processing());
    }

    #[test]
    fn preset_name_and_video_output_codec_are_none_off_video_preset() {
        let non_video = StageKind::audio_route("atrack:0", StageKind::source());
        assert_eq!(non_video.preset_name(), None);
        assert_eq!(non_video.video_output_codec(), None);

        let bare_preset = StageKind::video_preset("720p");
        assert_eq!(bare_preset.preset_name(), Some("720p"));
        assert_eq!(bare_preset.video_output_codec(), None);

        let with_codec = StageKind::video_preset_with_codec("720p", "h264");
        assert_eq!(with_codec.preset_name(), Some("720p"));
        assert_eq!(with_codec.video_output_codec(), Some("h264"));
    }
}
