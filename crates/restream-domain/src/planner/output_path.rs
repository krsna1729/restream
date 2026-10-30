//! Application-layer output path planning that interprets output encoding and
//! target protocol choices into stage-aware routing decisions.

#[cfg(test)]
use crate::domain::output_spec::OutputVideoConfig;
use crate::domain::output_spec::{
    EgressProtocol, OutputConfig, OutputConfigError, ProtocolCapabilities, ResolvedOutputVideo,
    VideoCodecKind,
};
use crate::domain::stage::{PipelineId, StageKey, StageKind};

use super::encoding_stage_plan::EncodingStagePlan;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct OutputPath {
    stage_plan: EncodingStagePlan,
    protocol: EgressProtocol,
    config: OutputConfig,
}

impl OutputPath {
    pub(super) fn resolve(
        pipeline_id: impl Into<PipelineId>,
        config: OutputConfig,
        url: &str,
    ) -> Self {
        Self {
            stage_plan: EncodingStagePlan::from_output_config(pipeline_id, &config),
            protocol: EgressProtocol::from_url(url),
            config,
        }
    }

    #[cfg(test)]
    fn is_rtmp(&self) -> bool {
        self.protocol.is_rtmp()
    }

    fn video_stage(&self, ingest_video_codec: Option<&str>) -> Option<StageKey> {
        let input_codec = normalized_input_codec(ingest_video_codec);
        if let Ok(resolved) = self.resolved_config(input_codec)
            && let ResolvedOutputVideo::Preset { preset, codec } = resolved.video
        {
            return Some(StageKey::new(
                self.stage_plan.pipeline().clone(),
                match codec.as_stage_codec() {
                    Some(codec) => StageKind::video_preset_with_codec(preset, codec),
                    None => StageKind::video_preset(preset),
                },
            ));
        }
        self.stage_plan.video_stage()
    }

    fn codec_edge_upstream_stage_kind(&self, ingest_video_codec: Option<&str>) -> StageKind {
        self.video_stage(ingest_video_codec)
            .map(|stage| stage.kind)
            .unwrap_or_else(|| self.stage_plan.video_terminal_kind().clone())
    }

    #[cfg(test)]
    fn audio_stage(&self) -> Option<StageKey> {
        self.stage_plan.audio_stage()
    }

    #[cfg(test)]
    fn codec_edge_candidate_stage(&self) -> Option<StageKey> {
        self.codec_edge_may_be_needed_without_input().then(|| {
            StageKey::new(
                self.stage_plan.pipeline().clone(),
                StageKind::codec_edge("hevc_to_h264", self.codec_edge_upstream_stage_kind(None)),
            )
        })
    }

    #[cfg(test)]
    fn needs_rtmp_h264_conv(&self, ingest_video_codec: Option<&str>) -> bool {
        self.needs_h264_codec_edge(ingest_video_codec)
    }

    #[cfg(test)]
    fn ingest_codec_override(&self, ingest_video_codec: Option<&str>) -> Option<&'static str> {
        ingest_video_codec
            .map(VideoCodecKind::from_codec_name)
            .is_some_and(VideoCodecKind::is_hevc)
            .then_some("hevc")
    }

    fn codec_edge_stage(&self, ingest_video_codec: Option<&str>) -> Option<StageKey> {
        self.needs_h264_codec_edge(ingest_video_codec).then(|| {
            StageKey::new(
                self.stage_plan.pipeline().clone(),
                StageKind::codec_edge(
                    "hevc_to_h264",
                    self.codec_edge_upstream_stage_kind(ingest_video_codec),
                ),
            )
        })
    }

    fn routed_audio_stage(&self, ingest_video_codec: Option<&str>) -> Option<StageKey> {
        if let Some(codec_edge) = self.codec_edge_stage(ingest_video_codec) {
            return self.stage_plan.audio_stage_from_upstream(codec_edge.kind);
        }
        if let Some(video_stage) = self.video_stage(ingest_video_codec) {
            return self.stage_plan.audio_stage_from_upstream(video_stage.kind);
        }
        self.stage_plan.audio_stage()
    }

    fn terminal_stage_kind(&self, ingest_video_codec: Option<&str>) -> StageKind {
        self.routed_audio_stage(ingest_video_codec)
            .or_else(|| self.codec_edge_stage(ingest_video_codec))
            .or_else(|| self.video_stage(ingest_video_codec))
            .map(|stage| stage.kind)
            .unwrap_or_else(|| self.stage_plan.terminal_kind().clone())
    }

    pub(super) fn terminal_stage_key(&self, ingest_video_codec: Option<&str>) -> StageKey {
        StageKey::new(
            self.stage_plan.pipeline().clone(),
            self.terminal_stage_kind(ingest_video_codec),
        )
    }

    pub(super) fn needed_stage_keys(&self, ingest_video_codec: Option<&str>) -> Vec<StageKey> {
        let mut stages = Vec::new();
        if let Some(stage) = self.video_stage(ingest_video_codec) {
            stages.push(stage);
        }
        if let Some(stage) = self.codec_edge_stage(ingest_video_codec) {
            stages.push(stage);
        }
        if let Some(stage) = self.routed_audio_stage(ingest_video_codec) {
            stages.push(stage);
        }
        stages
    }

    fn capabilities(&self) -> ProtocolCapabilities {
        ProtocolCapabilities {
            protocol: self.protocol,
            rtmp_mode: self.protocol.is_rtmp().then(|| self.config.rtmp_mode()),
        }
    }

    fn resolved_config(
        &self,
        input_codec: VideoCodecKind,
    ) -> Result<crate::domain::output_spec::ResolvedOutputConfig, OutputConfigError> {
        self.config
            .resolve_for_input_codec(self.capabilities(), input_codec)
    }

    #[cfg(test)]
    fn codec_edge_may_be_needed_without_input(&self) -> bool {
        matches!(self.config.video, OutputVideoConfig::Source { .. })
            && self
                .resolved_config(VideoCodecKind::Hevc)
                .is_ok_and(|resolved| {
                    matches!(
                        resolved.video,
                        ResolvedOutputVideo::Source {
                            codec: VideoCodecKind::H264
                        }
                    )
                })
    }

    fn needs_h264_codec_edge(&self, ingest_video_codec: Option<&str>) -> bool {
        let input_codec = normalized_input_codec(ingest_video_codec);
        input_codec.is_hevc()
            && self.resolved_config(input_codec).is_ok_and(|resolved| {
                matches!(
                    resolved.video,
                    ResolvedOutputVideo::Source {
                        codec: VideoCodecKind::H264
                    }
                )
            })
    }
}

fn normalized_input_codec(ingest_video_codec: Option<&str>) -> VideoCodecKind {
    ingest_video_codec
        .map(VideoCodecKind::from_codec_name)
        .unwrap_or(VideoCodecKind::Unknown)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::audio_routing::AudioRouting;
    use crate::domain::output_spec::RtmpOutputMode;

    fn preset_with_audio(preset: &str, audio: AudioRouting) -> OutputConfig {
        OutputConfig::preset(preset).with_audio(audio)
    }

    #[test]
    fn legacy_rtmp_hevc_preset_resolves_to_h264_video_stage() {
        let path = OutputPath::resolve(
            "pipe",
            preset_with_audio("720p", AudioRouting::SelectTracks { tracks: vec![0] }),
            "rtmp://example/live",
        );

        assert!(!path.needs_rtmp_h264_conv(Some("hevc")));
        assert_eq!(path.ingest_codec_override(Some("h265")), Some("hevc"));
        assert_eq!(
            path.terminal_stage_kind(Some("hevc")),
            StageKind::audio_route(
                "atrack:0",
                StageKind::video_preset_with_codec("720p", "h264"),
            )
        );
    }

    #[test]
    fn non_rtmp_outputs_do_not_add_codec_edge_for_hevc_ingest() {
        let path = OutputPath::resolve(
            "pipe",
            preset_with_audio("720p", AudioRouting::SelectTracks { tracks: vec![0] }),
            "srt://example:9000",
        );

        assert!(!path.needs_rtmp_h264_conv(Some("hevc")));
        assert!(path.codec_edge_stage(Some("hevc")).is_none());
        assert_eq!(
            path.terminal_stage_kind(Some("hevc")),
            StageKind::audio_route(
                "atrack:0",
                StageKind::video_preset_with_codec("720p", "hevc")
            ),
        );
    }

    #[test]
    fn enhanced_rtmp_hevc_output_skips_codec_edge_to_terminal_stage() {
        let path = OutputPath::resolve(
            "pipe",
            preset_with_audio("720p", AudioRouting::SelectTracks { tracks: vec![0] })
                .with_rtmp_mode(RtmpOutputMode::Enhanced),
            "rtmp://example/live",
        );

        assert!(!path.needs_rtmp_h264_conv(Some("hevc")));
        assert!(path.codec_edge_stage(Some("hevc")).is_none());
        assert!(path.codec_edge_candidate_stage().is_none());
        assert_eq!(
            path.terminal_stage_kind(Some("hevc")),
            StageKind::audio_route(
                "atrack:0",
                StageKind::video_preset_with_codec("720p", "hevc")
            ),
        );
    }

    #[test]
    fn candidate_codec_edge_is_available_for_rtmp_planning_without_ingest_codec() {
        let path = OutputPath::resolve("pipe", OutputConfig::source(), "rtmps://example/live");

        assert!(path.is_rtmp());
        assert_eq!(
            path.codec_edge_candidate_stage().unwrap().kind,
            StageKind::codec_edge("hevc_to_h264", StageKind::source())
        );
    }

    #[test]
    fn source_atrack_creates_audio_stage_without_video_stage() {
        let path = OutputPath::resolve(
            "pipe",
            OutputConfig::source().with_audio(AudioRouting::SelectTracks { tracks: vec![0] }),
            "rtmp://example/live",
        );

        assert!(path.video_stage(None).is_none());
        assert_eq!(
            path.audio_stage().unwrap().kind,
            StageKind::audio_route("atrack:0", StageKind::source())
        );
        assert_eq!(
            path.terminal_stage_kind(None),
            StageKind::audio_route("atrack:0", StageKind::source())
        );
    }

    #[test]
    fn needed_stage_keys_include_resolved_video_and_audio() {
        let path = OutputPath::resolve(
            "pipe",
            preset_with_audio(
                "720p",
                AudioRouting::Remap {
                    track: 0,
                    left: 0,
                    right: 1,
                },
            ),
            "rtmp://example/live",
        );
        let stages = path.needed_stage_keys(Some("hevc"));

        assert_eq!(stages.len(), 2);
        assert_eq!(
            stages[0].kind,
            StageKind::video_preset_with_codec("720p", "h264")
        );
        assert_eq!(
            stages[1].kind,
            StageKind::audio_route(
                "remap:0:1",
                StageKind::video_preset_with_codec("720p", "h264"),
            )
        );
    }

    #[test]
    fn duplicate_outputs_share_planned_stage_keys() {
        use std::collections::HashSet;

        let matrix = [
            (
                OutputConfig::source(),
                "rtmp://example/live/a",
                Some("hevc"),
            ),
            (
                OutputConfig::source(),
                "rtmp://example/live/b",
                Some("hevc"),
            ),
            (
                preset_with_audio("720p", AudioRouting::SelectTracks { tracks: vec![0] }),
                "rtmp://example/live/c",
                Some("hevc"),
            ),
            (
                preset_with_audio("720p", AudioRouting::SelectTracks { tracks: vec![0] }),
                "rtmp://example/live/d",
                Some("hevc"),
            ),
        ];
        let unique: HashSet<_> = matrix
            .iter()
            .flat_map(|(config, url, codec)| {
                OutputPath::resolve("pipe", config.clone(), url).needed_stage_keys(*codec)
            })
            .collect();

        assert!(unique.contains(&StageKey::new(
            "pipe",
            StageKind::codec_edge("hevc_to_h264", StageKind::source())
        )));
        assert!(unique.contains(&StageKey::new(
            "pipe",
            StageKind::video_preset_with_codec("720p", "h264")
        )));
        assert!(unique.contains(&StageKey::new(
            "pipe",
            StageKind::audio_route(
                "atrack:0",
                StageKind::video_preset_with_codec("720p", "h264")
            )
        )));
        assert_eq!(
            unique.len(),
            3,
            "duplicate outputs must reuse stage keys instead of planning per-output stages"
        );
    }
}
