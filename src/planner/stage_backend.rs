//! Backend selection for runtime stages.
//!
//! Owns the backend decision for a typed stage.

use crate::domain::audio_routing::{AudioRouting, parse_audio_operation};
use crate::domain::stage::StageKind;
use crate::domain::state::StageBackendKind;

/// The backend that runs a typed stage. FFmpeg work always runs as an
/// external `ffmpeg` process; lightweight audio routes and the service
/// stages have their own runtimes.
pub fn select_backend(stage: &StageKind) -> StageBackendKind {
    match stage {
        StageKind::AudioRoute { operation, .. }
            if is_lightweight_audio_route(&parse_audio_operation(operation)) =>
        {
            StageBackendKind::AudioRouter
        }
        StageKind::Hls | StageKind::HlsSegmenter { .. } => StageBackendKind::HlsSegmenter,
        StageKind::Recording => StageBackendKind::Recording,
        _ => StageBackendKind::ExternalFfmpeg,
    }
}

fn is_lightweight_audio_route(routing: &AudioRouting) -> bool {
    matches!(
        routing,
        AudioRouting::SelectTracks { .. } | AudioRouting::Passthrough
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selects_audio_router_for_lightweight_audio_routes() {
        let stage = StageKind::audio_route("atrack:0", StageKind::source());

        assert_eq!(select_backend(&stage), StageBackendKind::AudioRouter);
    }

    #[test]
    fn selects_external_ffmpeg_for_downmix_audio_routes() {
        let stage = StageKind::audio_route("downmix:0", StageKind::source());

        assert_eq!(select_backend(&stage), StageBackendKind::ExternalFfmpeg);
    }

    #[test]
    fn selects_external_ffmpeg_for_channel_remap_routes() {
        let stage = StageKind::audio_route("remap:0:1", StageKind::source());

        assert_eq!(select_backend(&stage), StageBackendKind::ExternalFfmpeg);
    }

    #[test]
    fn selects_external_ffmpeg_for_video_by_default() {
        assert_eq!(
            select_backend(&StageKind::video_preset("720p")),
            StageBackendKind::ExternalFfmpeg
        );
    }

    #[test]
    fn complex_audio_uses_external_by_default() {
        let stage = StageKind::audio_route("downmix:0", StageKind::source());

        assert_eq!(select_backend(&stage), StageBackendKind::ExternalFfmpeg);
    }

    #[test]
    fn preview_uses_external_by_default() {
        assert_eq!(
            select_backend(&StageKind::preview("h264", StageKind::source())),
            StageBackendKind::ExternalFfmpeg
        );
    }

    #[test]
    fn terminal_service_stages_use_named_backends() {
        assert_eq!(
            select_backend(&StageKind::hls()),
            StageBackendKind::HlsSegmenter
        );
        assert_eq!(
            select_backend(&StageKind::recording()),
            StageBackendKind::Recording
        );
    }
}
