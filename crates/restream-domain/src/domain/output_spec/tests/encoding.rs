use crate::domain::output_spec::{OutputEncodingSpec, StagePresetSpec, VideoSelector};

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
