//! Transport-neutral metadata shared by media producers and consumers.
//!
//! Keep this module dependency-light: metadata describes encoded streams and
//! must not depend on engine lifecycle, protocol implementations, or storage.

pub use restream_core::metadata::*;
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_encoder_declaration_keeps_plausible_values_only() {
        let declared = DeclaredEncoder::from_metadata(
            Some("  obs-output module (libobs version 30.2.3)\u{0}\n "),
            Some(1920),
            Some(1080),
            Some(59.94),
            Some(6000),
            Some(160),
        )
        .expect("declared");
        assert_eq!(
            declared.encoder.as_deref(),
            Some("obs-output module (libobs version 30.2.3)")
        );
        assert_eq!((declared.width, declared.height), (Some(1920), Some(1080)));
        assert!((declared.frame_rate.unwrap() - 59.94).abs() < 0.001);
        assert_eq!(
            (declared.video_kbps, declared.audio_kbps),
            (Some(6000), Some(160))
        );

        let zeros =
            DeclaredEncoder::from_metadata(None, Some(0), Some(0), Some(f32::NAN), Some(0), None);
        assert_eq!(zeros, None, "nothing useful");
        let long = "x".repeat(500);
        let cut =
            DeclaredEncoder::from_metadata(Some(&long), None, None, Some(5_000.0), None, None)
                .expect("a name");
        assert_eq!(cut.encoder.unwrap().len(), ENCODER_NAME_MAX);
        assert_eq!(cut.frame_rate, None, "5000 fps is not a frame rate");
    }

    #[test]
    fn a_declaration_serializes_without_absent_fields() {
        let declared = DeclaredEncoder::from_metadata(
            Some("Lavf60.16.100"),
            None,
            None,
            None,
            Some(2500),
            None,
        )
        .unwrap();
        assert_eq!(
            serde_json::to_value(&declared).unwrap(),
            serde_json::json!({ "encoder": "Lavf60.16.100", "videoKbps": 2500 })
        );
    }
}
