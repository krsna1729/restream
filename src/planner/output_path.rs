//! Application-layer output path planning that interprets output encoding and
//! target protocol choices into stage-aware routing decisions.

use crate::domain::output_spec::{EgressProtocol, OutputConfig, VideoCodecKind};
use crate::domain::stage::{EncodingStagePlan, PipelineId, StageKey, StageKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputPath {
    stage_plan: EncodingStagePlan,
    protocol: EgressProtocol,
    config: OutputConfig,
}

impl OutputPath {
    pub fn resolve(pipeline_id: impl Into<PipelineId>, config: OutputConfig, url: &str) -> Self {
        Self {
            stage_plan: EncodingStagePlan::from_output_config(pipeline_id, &config),
            protocol: EgressProtocol::from_url(url),
            config,
        }
    }

    pub fn is_rtmp(&self) -> bool {
        self.protocol.is_rtmp()
    }

    pub fn video_stage(&self, ingest_video_codec: Option<&str>) -> Option<StageKey> {
        if ingest_video_codec
            .map(VideoCodecKind::from_codec_name)
            .is_some_and(VideoCodecKind::is_hevc)
            && let StageKind::VideoPreset { preset, .. } = self.stage_plan.video_terminal_kind()
        {
            return Some(StageKey::new(
                self.stage_plan.pipeline().clone(),
                StageKind::video_preset_with_codec(preset.clone(), "hevc"),
            ));
        }
        self.stage_plan.video_stage()
    }

    fn codec_edge_upstream_stage_kind(&self, ingest_video_codec: Option<&str>) -> StageKind {
        if ingest_video_codec
            .map(VideoCodecKind::from_codec_name)
            .is_some_and(VideoCodecKind::is_hevc)
            && let StageKind::VideoPreset { preset, .. } = self.stage_plan.video_terminal_kind()
        {
            return StageKind::video_preset_with_codec(preset.clone(), "hevc");
        }
        self.stage_plan.video_terminal_kind().clone()
    }

    pub fn audio_stage(&self) -> Option<StageKey> {
        self.stage_plan.audio_stage()
    }

    pub fn codec_edge_candidate_stage(&self) -> Option<StageKey> {
        // Legacy RTMP cannot carry HEVC in our current contract. Key the expensive
        // HEVC->H.264 edge by video shape only, then apply selected-audio
        // routing after it. That trades a few cheap audio-route stages for
        // sharing one codec edge across atrack:0/atrack:1 outputs.
        (self.protocol.is_rtmp() && self.config.rtmp_mode().is_legacy()).then(|| {
            StageKey::new(
                self.stage_plan.pipeline().clone(),
                StageKind::codec_edge("hevc_to_h264", self.codec_edge_upstream_stage_kind(None)),
            )
        })
    }

    pub fn needs_rtmp_h264_conv(&self, ingest_video_codec: Option<&str>) -> bool {
        self.protocol.is_rtmp()
            && self.config.rtmp_mode().is_legacy()
            && ingest_video_codec
                .map(VideoCodecKind::from_codec_name)
                .is_some_and(VideoCodecKind::is_hevc)
    }

    pub fn ingest_codec_override(&self, ingest_video_codec: Option<&str>) -> Option<&'static str> {
        ingest_video_codec
            .map(VideoCodecKind::from_codec_name)
            .is_some_and(VideoCodecKind::is_hevc)
            .then_some("hevc")
    }

    pub fn codec_edge_stage(&self, ingest_video_codec: Option<&str>) -> Option<StageKey> {
        self.needs_rtmp_h264_conv(ingest_video_codec).then(|| {
            StageKey::new(
                self.stage_plan.pipeline().clone(),
                StageKind::codec_edge(
                    "hevc_to_h264",
                    self.codec_edge_upstream_stage_kind(ingest_video_codec),
                ),
            )
        })
    }

    pub fn codec_edge_upstream_kind(&self, ingest_video_codec: Option<&str>) -> StageKind {
        if self.needs_rtmp_h264_conv(ingest_video_codec) {
            self.codec_edge_upstream_stage_kind(ingest_video_codec)
        } else {
            self.stage_plan.terminal_kind().clone()
        }
    }

    pub fn routed_audio_stage(&self, ingest_video_codec: Option<&str>) -> Option<StageKey> {
        if let Some(codec_edge) = self.codec_edge_stage(ingest_video_codec) {
            return self.stage_plan.audio_stage_from_upstream(codec_edge.kind);
        }
        if let Some(video_stage) = self.video_stage(ingest_video_codec) {
            return self.stage_plan.audio_stage_from_upstream(video_stage.kind);
        }
        self.stage_plan.audio_stage()
    }

    pub fn terminal_stage_kind(&self, ingest_video_codec: Option<&str>) -> StageKind {
        self.routed_audio_stage(ingest_video_codec)
            .or_else(|| self.codec_edge_stage(ingest_video_codec))
            .or_else(|| self.video_stage(ingest_video_codec))
            .map(|stage| stage.kind)
            .unwrap_or_else(|| self.stage_plan.terminal_kind().clone())
    }

    pub fn terminal_stage_key(&self, ingest_video_codec: Option<&str>) -> StageKey {
        StageKey::new(
            self.stage_plan.pipeline().clone(),
            self.terminal_stage_kind(ingest_video_codec),
        )
    }

    pub fn needed_stage_keys(&self, ingest_video_codec: Option<&str>) -> Vec<StageKey> {
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
    fn rtmp_hevc_output_adds_codec_edge_to_terminal_stage() {
        let path = OutputPath::resolve(
            "pipe",
            preset_with_audio("720p", AudioRouting::SelectTracks { tracks: vec![0] }),
            "rtmp://example/live",
        );

        assert!(path.needs_rtmp_h264_conv(Some("hevc")));
        assert_eq!(path.ingest_codec_override(Some("h265")), Some("hevc"));
        assert_eq!(
            path.terminal_stage_kind(Some("hevc")),
            StageKind::audio_route(
                "atrack:0",
                StageKind::codec_edge(
                    "hevc_to_h264",
                    StageKind::video_preset_with_codec("720p", "hevc")
                ),
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
    fn needed_stage_keys_include_video_audio_and_optional_codec_edge() {
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

        assert_eq!(stages.len(), 3);
        assert_eq!(
            stages[0].kind,
            StageKind::video_preset_with_codec("720p", "hevc")
        );
        assert_eq!(
            stages[1].kind,
            StageKind::codec_edge(
                "hevc_to_h264",
                StageKind::video_preset_with_codec("720p", "hevc")
            )
        );
        assert_eq!(
            stages[2].kind,
            StageKind::audio_route(
                "remap:0:1",
                StageKind::codec_edge(
                    "hevc_to_h264",
                    StageKind::video_preset_with_codec("720p", "hevc")
                ),
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
            StageKind::video_preset_with_codec("720p", "hevc")
        )));
        assert!(unique.contains(&StageKey::new(
            "pipe",
            StageKind::codec_edge(
                "hevc_to_h264",
                StageKind::video_preset_with_codec("720p", "hevc")
            )
        )));
        assert!(unique.contains(&StageKey::new(
            "pipe",
            StageKind::audio_route(
                "atrack:0",
                StageKind::codec_edge(
                    "hevc_to_h264",
                    StageKind::video_preset_with_codec("720p", "hevc")
                )
            )
        )));
        assert_eq!(
            unique.len(),
            4,
            "duplicate outputs must reuse stage keys instead of planning per-output stages"
        );
    }
}
