//! Transport-neutral metadata shared by media producers and consumers.
//!
//! Keep this module dependency-light: metadata describes encoded streams and
//! must not depend on engine lifecycle, protocol implementations, or storage.

/// Video stream metadata collected from the demuxer.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VideoMeta {
    pub codec: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub bw: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub level: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixel_format: Option<String>,
}

/// What an RTMP encoder says it sends (`onMetaData`): its name and its
/// configured picture, frame rate and bitrates. Declared, not measured: the
/// dashboard compares it with what arrives.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeclaredEncoder {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub encoder: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub width: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub height: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_rate: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub video_kbps: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub audio_kbps: Option<u32>,
}

/// The longest encoder name kept: it is the publisher's free text.
const ENCODER_NAME_MAX: usize = 128;

impl DeclaredEncoder {
    /// From an `onMetaData`, keeping only plausible values: a name without
    /// control characters, cut to [`ENCODER_NAME_MAX`]; positive sizes,
    /// rates and bitrates. `None` when nothing useful remains.
    pub fn from_metadata(
        encoder: Option<&str>,
        width: Option<u32>,
        height: Option<u32>,
        frame_rate: Option<f32>,
        video_kbps: Option<u32>,
        audio_kbps: Option<u32>,
    ) -> Option<Self> {
        let encoder = encoder
            .map(|name| {
                name.chars()
                    .filter(|c| !c.is_control())
                    .take(ENCODER_NAME_MAX)
                    .collect::<String>()
                    .trim()
                    .to_string()
            })
            .filter(|name| !name.is_empty());
        let positive = |value: Option<u32>| value.filter(|value| *value > 0);
        let declared = Self {
            encoder,
            width: positive(width),
            height: positive(height),
            frame_rate: frame_rate
                .map(f64::from)
                .filter(|rate| rate.is_finite() && *rate > 0.0 && *rate <= 1_000.0),
            video_kbps: positive(video_kbps),
            audio_kbps: positive(audio_kbps),
        };
        (declared != Self::default()).then_some(declared)
    }
}

/// Audio stream metadata collected from the demuxer.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AudioMeta {
    pub codec: String,
    pub sample_rate: u32,
    pub channels: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub channel_layout: Option<String>,
    pub track_index: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pid: Option<u16>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub profile: Option<String>,
}

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
