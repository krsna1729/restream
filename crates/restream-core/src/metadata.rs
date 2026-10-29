//! Transport-neutral metadata shared by media producers and consumers.

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

pub const ENCODER_NAME_MAX: usize = 128;

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
