use crate::domain::output_spec::{
    EgressProtocol, OutputUrlScheme, RecirculationTarget, VideoCodecKind,
};

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
        EgressProtocol::from_url("sink://local/test"),
        EgressProtocol::Sink
    );
    assert_eq!(
        EgressProtocol::from_url("pipeline://target/input"),
        EgressProtocol::Pipeline
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
        OutputUrlScheme::from_url("sink://local/test"),
        OutputUrlScheme::Sink
    );
    assert_eq!(
        OutputUrlScheme::from_url("pipeline://target/input"),
        OutputUrlScheme::Pipeline
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
fn output_url_scheme_supports_runnable_outputs() {
    assert!(OutputUrlScheme::Rtmp.is_supported_output());
    assert!(OutputUrlScheme::Rtmps.is_supported_output());
    assert!(OutputUrlScheme::Srt.is_supported_output());
    assert!(OutputUrlScheme::Hls.is_supported_output());
    assert!(OutputUrlScheme::Sink.is_supported_output());
    assert!(OutputUrlScheme::Pipeline.is_supported_output());
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
            OutputUrlScheme::Sink,
            false,
            false,
            false,
            EgressProtocol::Sink,
        ),
        (
            OutputUrlScheme::Pipeline,
            false,
            false,
            false,
            EgressProtocol::Pipeline,
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
    assert!(!EgressProtocol::Sink.is_rtmp());
    assert!(!EgressProtocol::Pipeline.is_rtmp());
    assert!(!EgressProtocol::Unknown.is_rtmp());

    assert_eq!(EgressProtocol::Rtmp.as_str(), "rtmp");
    assert_eq!(EgressProtocol::Srt.as_str(), "srt");
    assert_eq!(EgressProtocol::Hls.as_str(), "hls");
    assert_eq!(EgressProtocol::Sink.as_str(), "sink");
    assert_eq!(EgressProtocol::Pipeline.as_str(), "pipeline");
    assert_eq!(EgressProtocol::Unknown.as_str(), "unknown");
}

#[test]
fn video_codec_kind_is_hevc_is_true_only_for_hevc() {
    assert!(!VideoCodecKind::H264.is_hevc());
    assert!(VideoCodecKind::Hevc.is_hevc());
    assert!(!VideoCodecKind::Unknown.is_hevc());
}

#[test]
fn recirculation_target_parses_pipeline_and_input_identity() {
    let target = RecirculationTarget::parse("pipeline://pipe-b/input-secondary").unwrap();

    assert_eq!(target.pipeline_id(), "pipe-b");
    assert_eq!(target.input_id(), "input-secondary");
}

#[test]
fn recirculation_target_rejects_missing_input_identity() {
    let error = RecirculationTarget::parse("pipeline://pipe-b").unwrap_err();

    assert_eq!(
        error.message(),
        "recirculation URL must include a target input"
    );
}
