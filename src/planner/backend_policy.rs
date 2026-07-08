//! Backend selection for runtime stages.
//!
//! The engine owns stage lifecycles; this module owns the policy choice for how
//! a typed stage should run.

use crate::domain::audio_routing::{AudioRouting, parse_audio_operation};
use crate::domain::stage::StageKind;
use tracing::warn;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageBackend {
    AudioRouter,
    InternalFfmpeg,
    ExternalFfmpeg,
}

/// Per-stage-family backend policy.
///
/// Replace the old `RESTREAM_USE_INTERNAL_TRANSCODER` global flag with
/// targeted controls so that each stage family can graduate independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackendPolicy {
    pub internal_video_presets: bool,
    pub internal_hevc_to_h264: bool,
    pub internal_hls_preview: bool,
    pub internal_complex_audio: bool,
}

fn env_bool(name: &str) -> Option<bool> {
    std::env::var(name).ok().map(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

impl BackendPolicy {
    pub fn from_env() -> Self {
        // Check the old global flag first for backward compatibility.
        let global_internal = env_bool("RESTREAM_USE_INTERNAL_TRANSCODER");

        if global_internal == Some(true) {
            warn!(
                "RESTREAM_USE_INTERNAL_TRANSCODER=1 is deprecated, \
                 use per-stage controls: RESTREAM_INTERNAL_VIDEO_PRESETS, \
                 RESTREAM_INTERNAL_HEVC_TO_H264, RESTREAM_INTERNAL_HLS_PREVIEW, \
                 RESTREAM_INTERNAL_AUDIO_COMPLEX"
            );
        }

        Self {
            internal_video_presets: env_bool("RESTREAM_INTERNAL_VIDEO_PRESETS")
                .or(global_internal)
                .unwrap_or(false),
            internal_hevc_to_h264: env_bool("RESTREAM_INTERNAL_HEVC_TO_H264")
                .or(global_internal)
                .unwrap_or(false),
            internal_hls_preview: env_bool("RESTREAM_INTERNAL_HLS_PREVIEW")
                .or(global_internal)
                .unwrap_or(false),
            internal_complex_audio: env_bool("RESTREAM_INTERNAL_AUDIO_COMPLEX")
                .or(global_internal)
                .unwrap_or(false),
        }
    }

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
        BackendPolicy {
            internal_video_presets: false,
            internal_hevc_to_h264: false,
            internal_hls_preview: false,
            internal_complex_audio: false,
        }
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
}
