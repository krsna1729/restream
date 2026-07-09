//! Backend selection for runtime stages.
//!
//! The engine owns stage lifecycles; this module owns the policy choice for how
//! a typed stage should run. Per-stage backend families are controlled via
//! Runtime configuration supplies the per-stage backend toggles; this module
//! owns only the policy decision for a typed stage.

use crate::domain::audio_routing::{AudioRouting, parse_audio_operation};
use crate::domain::stage::StageKind;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageBackend {
    AudioRouter,
    InternalFfmpeg,
    ExternalFfmpeg,
}

/// Per-stage-family backend policy.
///
/// Targeted controls so that each stage family can graduate independently.
/// The default is all-external (no internal FFmpeg backends enabled).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct BackendPolicy {
    pub internal_video_presets: bool,
    pub internal_hevc_to_h264: bool,
    pub internal_hls_preview: bool,
    pub internal_complex_audio: bool,
}

impl BackendPolicy {
    pub fn select_backend(&self, stage: &StageKind) -> StageBackend {
        match stage {
            StageKind::AudioRoute { operation, .. } => {
                let routing = parse_audio_operation(operation);
                if is_lightweight_audio_route(&routing) {
                    StageBackend::AudioRouter
                } else if self.internal_complex_audio {
                    StageBackend::InternalFfmpeg
                } else {
                    StageBackend::ExternalFfmpeg
                }
            }
            StageKind::VideoPreset { .. } => {
                if self.internal_video_presets {
                    StageBackend::InternalFfmpeg
                } else {
                    StageBackend::ExternalFfmpeg
                }
            }
            StageKind::CodecEdge { operation, .. }
                if operation == "hevc_to_h264" && self.internal_hevc_to_h264 =>
            {
                StageBackend::InternalFfmpeg
            }
            StageKind::Preview { .. } if self.internal_hls_preview => StageBackend::InternalFfmpeg,
            _ => StageBackend::ExternalFfmpeg,
        }
    }
}

pub fn is_lightweight_audio_route(routing: &AudioRouting) -> bool {
    matches!(
        routing,
        AudioRouting::SelectTracks { .. } | AudioRouting::Passthrough
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn default_policy() -> BackendPolicy {
        BackendPolicy::default()
    }

    #[test]
    fn selects_audio_router_for_lightweight_audio_routes() {
        let policy = default_policy();
        let stage = StageKind::audio_route("atrack:0", StageKind::source());

        assert_eq!(policy.select_backend(&stage), StageBackend::AudioRouter);
    }

    #[test]
    fn selects_external_ffmpeg_for_downmix_audio_routes() {
        let policy = default_policy();
        let stage = StageKind::audio_route("downmix:0", StageKind::source());

        assert_eq!(policy.select_backend(&stage), StageBackend::ExternalFfmpeg);
    }

    #[test]
    fn selects_external_ffmpeg_for_channel_remap_routes() {
        let policy = default_policy();
        let stage = StageKind::audio_route("remap:0:1", StageKind::source());

        assert_eq!(policy.select_backend(&stage), StageBackend::ExternalFfmpeg);
    }

    #[test]
    fn selects_external_ffmpeg_for_video_by_default() {
        let policy = default_policy();

        assert_eq!(
            policy.select_backend(&StageKind::video_preset("720p")),
            StageBackend::ExternalFfmpeg
        );
    }

    #[test]
    fn selects_internal_ffmpeg_for_video_when_enabled() {
        let policy = BackendPolicy {
            internal_video_presets: true,
            ..default_policy()
        };

        assert_eq!(
            policy.select_backend(&StageKind::video_preset("720p")),
            StageBackend::InternalFfmpeg
        );
    }

    #[test]
    fn selects_internal_ffmpeg_for_codec_edges_when_enabled() {
        let policy = BackendPolicy {
            internal_hevc_to_h264: true,
            ..default_policy()
        };

        assert_eq!(
            policy.select_backend(&StageKind::codec_edge("hevc_to_h264", StageKind::source())),
            StageBackend::InternalFfmpeg
        );
    }

    #[test]
    fn codec_edge_stays_external_when_only_video_presets_are_internal() {
        let policy = BackendPolicy {
            internal_video_presets: true,
            internal_hevc_to_h264: false,
            ..default_policy()
        };

        assert_eq!(
            policy.select_backend(&StageKind::codec_edge("hevc_to_h264", StageKind::source())),
            StageBackend::ExternalFfmpeg
        );
    }

    #[test]
    fn video_preset_stays_external_when_only_codec_edge_is_internal() {
        let policy = BackendPolicy {
            internal_video_presets: false,
            internal_hevc_to_h264: true,
            ..default_policy()
        };

        assert_eq!(
            policy.select_backend(&StageKind::video_preset("720p")),
            StageBackend::ExternalFfmpeg
        );
    }

    #[test]
    fn complex_audio_uses_external_by_default() {
        let policy = default_policy();
        let stage = StageKind::audio_route("downmix:0", StageKind::source());

        assert_eq!(policy.select_backend(&stage), StageBackend::ExternalFfmpeg);
    }

    #[test]
    fn complex_audio_uses_internal_when_enabled() {
        let policy = BackendPolicy {
            internal_complex_audio: true,
            ..default_policy()
        };
        let stage = StageKind::audio_route("downmix:0", StageKind::source());

        assert_eq!(policy.select_backend(&stage), StageBackend::InternalFfmpeg);
    }

    #[test]
    fn preview_uses_external_by_default() {
        let policy = default_policy();

        assert_eq!(
            policy.select_backend(&StageKind::preview("h264", StageKind::source())),
            StageBackend::ExternalFfmpeg
        );
    }

    #[test]
    fn preview_uses_internal_when_enabled() {
        let policy = BackendPolicy {
            internal_hls_preview: true,
            ..default_policy()
        };

        assert_eq!(
            policy.select_backend(&StageKind::preview("h264", StageKind::source())),
            StageBackend::InternalFfmpeg
        );
    }
}
