use crate::domain::audio_routing::AudioRouting;
use crate::domain::output_spec::{
    EgressProtocol, OutputConfig, OutputConfigError, OutputProtocolConfig, OutputVideoCodec,
    OutputVideoConfig, ProtocolCapabilities, ResolvedOutputVideo, RtmpOutputMode, VideoCodecKind,
};

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
fn output_video_config_is_custom_and_encoding_label() {
    let source = OutputVideoConfig::Source {
        codec: OutputVideoCodec::Auto,
    };
    assert_eq!(source.encoding_label(), "source");
    assert!(!source.is_custom());

    assert_eq!(OutputVideoConfig::Custom.encoding_label(), "custom");
    assert!(OutputVideoConfig::Custom.is_custom());

    let preset = OutputVideoConfig::Preset {
        preset: "480p".to_string(),
        codec: OutputVideoCodec::Auto,
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
fn output_config_source_passthrough_matches_same_format_path_only() {
    assert!(OutputConfig::default().is_source_passthrough());
    assert!(!OutputConfig::preset("720p").is_source_passthrough());
    assert!(
        !OutputConfig::source()
            .with_video_codec(OutputVideoCodec::H264)
            .is_source_passthrough()
    );
    assert!(
        !OutputConfig::source()
            .with_audio(AudioRouting::SelectTracks { tracks: vec![0] })
            .is_source_passthrough()
    );
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
    assert_eq!(config.video.codec(), OutputVideoCodec::Auto);
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
fn output_video_config_serializes_explicit_h265_codec() {
    let config = OutputConfig::preset("720p").with_video_codec(OutputVideoCodec::Hevc);
    let value = serde_json::to_value(&config).unwrap();

    assert_eq!(
        value["video"],
        serde_json::json!({"mode": "preset", "preset": "720p", "codec": "h265"})
    );
}

#[test]
fn capability_resolution_keeps_enhanced_rtmp_hevc_auto() {
    let config = OutputConfig::preset("720p").with_rtmp_mode(RtmpOutputMode::Enhanced);
    let capabilities = ProtocolCapabilities {
        protocol: EgressProtocol::Rtmp,
        rtmp_mode: Some(RtmpOutputMode::Enhanced),
    };

    let resolved = config
        .resolve_for_input_codec(capabilities, VideoCodecKind::Hevc)
        .unwrap();

    assert_eq!(
        resolved.video,
        ResolvedOutputVideo::Preset {
            preset: "720p".to_string(),
            codec: VideoCodecKind::Hevc
        }
    );
}

#[test]
fn capability_resolution_downgrades_legacy_rtmp_auto_to_h264() {
    let capabilities = ProtocolCapabilities {
        protocol: EgressProtocol::Rtmp,
        rtmp_mode: Some(RtmpOutputMode::Legacy),
    };

    let resolved = OutputConfig::preset("720p")
        .resolve_for_input_codec(capabilities, VideoCodecKind::Hevc)
        .unwrap();

    assert_eq!(
        resolved.video,
        ResolvedOutputVideo::Preset {
            preset: "720p".to_string(),
            codec: VideoCodecKind::H264
        }
    );
}

#[test]
fn capability_validation_rejects_explicit_h265_for_legacy_rtmp() {
    let capabilities = ProtocolCapabilities {
        protocol: EgressProtocol::Rtmp,
        rtmp_mode: Some(RtmpOutputMode::Legacy),
    };
    let config = OutputConfig::preset("720p").with_video_codec(OutputVideoCodec::Hevc);

    let error = config.validate_capabilities(capabilities).unwrap_err();

    assert_eq!(error, OutputConfigError::UnsupportedCodecForProtocol);
}

#[test]
fn sink_capabilities_accept_source_codecs() {
    let capabilities = ProtocolCapabilities {
        protocol: EgressProtocol::Sink,
        rtmp_mode: None,
    };

    assert!(
        OutputConfig::source()
            .with_video_codec(OutputVideoCodec::H264)
            .validate_capabilities(capabilities)
            .is_ok()
    );
    assert!(
        OutputConfig::source()
            .with_video_codec(OutputVideoCodec::Hevc)
            .validate_capabilities(capabilities)
            .is_ok()
    );
}
