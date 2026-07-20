use crate::domain::output_spec::{OutputConfig, OutputVideoCodec, OutputVideoConfig};
#[cfg(test)]
use crate::domain::output_spec::{OutputEncodingSpec, VideoSelector};
use crate::domain::stage::{PipelineId, StageKey, StageKind};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EncodingStagePlan {
    pipeline: PipelineId,
    source: StageKind,
    video_stage: Option<StageKind>,
    audio_stage: Option<StageKind>,
}

impl EncodingStagePlan {
    #[cfg(test)]
    fn from_spec(pipeline_id: impl Into<PipelineId>, encoding: &OutputEncodingSpec) -> Self {
        let pipeline = pipeline_id.into();
        let source = StageKind::source();
        let video_stage = match encoding.video() {
            VideoSelector::Preset(preset) => Some(StageKind::video_preset(preset.clone())),
            VideoSelector::Source | VideoSelector::Custom => None,
        };
        let upstream = video_stage.clone().unwrap_or_else(|| source.clone());
        let audio_stage = encoding
            .audio_operation()
            .map(|operation| StageKind::audio_route(operation, upstream));

        Self {
            pipeline,
            source,
            video_stage,
            audio_stage,
        }
    }

    pub(super) fn from_output_config(
        pipeline_id: impl Into<PipelineId>,
        config: &OutputConfig,
    ) -> Self {
        let pipeline = pipeline_id.into();
        let source = StageKind::source();
        let video_stage = match &config.video {
            OutputVideoConfig::Preset { preset, codec } => Some(match codec {
                OutputVideoCodec::Auto => StageKind::video_preset(preset),
                OutputVideoCodec::H264 => StageKind::video_preset_with_codec(preset, "h264"),
                OutputVideoCodec::Hevc => StageKind::video_preset_with_codec(preset, "hevc"),
            }),
            OutputVideoConfig::Source { .. } | OutputVideoConfig::Custom => None,
        };
        let upstream = video_stage.clone().unwrap_or_else(|| source.clone());
        let audio_stage = config
            .audio
            .operation_string()
            .map(|operation| StageKind::audio_route(operation, upstream));

        Self {
            pipeline,
            source,
            video_stage,
            audio_stage,
        }
    }

    #[cfg(test)]
    pub(crate) fn from_encoding(pipeline_id: impl Into<PipelineId>, encoding: &str) -> Self {
        Self::from_spec(pipeline_id, &OutputEncodingSpec::parse(encoding))
    }

    pub(super) fn pipeline(&self) -> &PipelineId {
        &self.pipeline
    }

    pub(crate) fn video_stage(&self) -> Option<StageKey> {
        self.video_stage
            .clone()
            .map(|kind| StageKey::new(self.pipeline.clone(), kind))
    }

    pub(crate) fn audio_stage(&self) -> Option<StageKey> {
        self.audio_stage
            .clone()
            .map(|kind| StageKey::new(self.pipeline.clone(), kind))
    }

    pub(super) fn audio_stage_from_upstream(&self, upstream: StageKind) -> Option<StageKey> {
        self.audio_stage
            .as_ref()
            .and_then(StageKind::audio_operation)
            .map(|operation| {
                StageKey::new(
                    self.pipeline.clone(),
                    StageKind::audio_route(operation, upstream),
                )
            })
    }

    pub(super) fn video_terminal_kind(&self) -> &StageKind {
        self.video_stage.as_ref().unwrap_or(&self.source)
    }

    pub(super) fn terminal_kind(&self) -> &StageKind {
        self.audio_stage
            .as_ref()
            .or(self.video_stage.as_ref())
            .unwrap_or(&self.source)
    }

    #[cfg(test)]
    fn codec_edge_stage(&self, operation: &str) -> StageKey {
        StageKey::new(
            self.pipeline.clone(),
            StageKind::codec_edge(operation, self.terminal_kind().clone()),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encoding_plan_produces_typed_video_and_audio_stages() {
        let plan = EncodingStagePlan::from_encoding("pipe", "720p+atrack:0");

        let video = plan.video_stage().unwrap();
        assert_eq!(video.kind, StageKind::video_preset("720p"));
        assert_eq!(video.pipeline.as_str(), "pipe");

        let audio = plan.audio_stage().unwrap();
        assert_eq!(
            audio.kind,
            StageKind::audio_route("atrack:0", StageKind::video_preset("720p"))
        );

        assert_eq!(
            *plan.terminal_kind(),
            StageKind::audio_route("atrack:0", StageKind::video_preset("720p"))
        );
    }

    #[test]
    fn encoding_plan_handles_passthrough_audio_route() {
        let plan = EncodingStagePlan::from_encoding("pipe", "source+remap:0:1");

        assert!(plan.video_stage().is_none());
        let audio = plan.audio_stage().unwrap();
        assert_eq!(
            audio.kind,
            StageKind::audio_route("remap:0:1", StageKind::source())
        );
        assert_eq!(
            *plan.terminal_kind(),
            StageKind::audio_route("remap:0:1", StageKind::source())
        );
    }

    #[test]
    fn encoding_plan_treats_plain_atrack_as_source_audio_route() {
        let plan = EncodingStagePlan::from_encoding("pipe", "atrack:0");

        assert!(plan.video_stage().is_none());
        assert_eq!(
            plan.audio_stage().unwrap().kind,
            StageKind::audio_route("atrack:0", StageKind::source())
        );
        assert_eq!(
            *plan.terminal_kind(),
            StageKind::audio_route("atrack:0", StageKind::source())
        );
    }

    #[test]
    fn codec_edge_uses_terminal_kind() {
        let plan = EncodingStagePlan::from_encoding("pipe", "720p");

        let edge = plan.codec_edge_stage("hevc_to_h264");
        assert_eq!(
            edge.kind,
            StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("720p"))
        );
    }

    #[test]
    fn video_terminal_kind_falls_back_to_source_without_a_video_stage() {
        let plan = EncodingStagePlan::from_encoding("pipe", "atrack:0");
        assert_eq!(*plan.video_terminal_kind(), StageKind::source());

        let plan_with_video = EncodingStagePlan::from_encoding("pipe", "720p+atrack:0");
        assert_eq!(
            *plan_with_video.video_terminal_kind(),
            StageKind::video_preset("720p")
        );
    }

    #[test]
    fn audio_stage_from_upstream_is_none_without_an_audio_route() {
        let plan = EncodingStagePlan::from_encoding("pipe", "720p");
        assert!(
            plan.audio_stage_from_upstream(StageKind::video_preset("720p"))
                .is_none()
        );
    }

    #[test]
    fn audio_stage_from_upstream_rewrites_the_upstream_kind() {
        let plan = EncodingStagePlan::from_encoding("pipe", "720p+atrack:0");
        let rewritten = plan
            .audio_stage_from_upstream(StageKind::codec_edge(
                "hevc_to_h264",
                StageKind::video_preset("720p"),
            ))
            .expect("plan has an audio route");
        assert_eq!(
            rewritten.kind,
            StageKind::audio_route(
                "atrack:0",
                StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("720p"))
            )
        );
    }

    #[test]
    fn encoding_stage_plan_pipeline_accessor_reports_the_constructing_pipeline() {
        let plan = EncodingStagePlan::from_encoding("pipe-7", "720p");
        assert_eq!(plan.pipeline().as_str(), "pipe-7");
    }
}
