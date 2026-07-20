use crate::domain::audio_routing::is_audio_operation;

use super::video::VideoSelector;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputEncodingSpec {
    video: VideoSelector,
    audio_operation: Option<String>,
}

impl OutputEncodingSpec {
    pub fn parse(encoding: &str) -> Self {
        let mut parts = encoding.splitn(2, '+');
        let first_part = parts.next().unwrap_or("source");
        let second_part = parts.next().filter(|value| !value.is_empty());
        let (video_part, audio_operation) = if is_audio_operation(first_part) {
            ("source", Some(first_part.to_string()))
        } else {
            (first_part, second_part.map(str::to_string))
        };

        let video = match video_part {
            "" | "source" => VideoSelector::Source,
            "custom" => VideoSelector::Custom,
            preset => VideoSelector::Preset(preset.to_string()),
        };

        Self {
            video,
            audio_operation,
        }
    }

    pub fn video(&self) -> &VideoSelector {
        &self.video
    }

    pub fn audio_operation(&self) -> Option<&str> {
        self.audio_operation.as_deref()
    }

    pub fn is_custom_output(&self) -> bool {
        self.video.is_custom()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StagePresetSpec {
    video: VideoSelector,
    audio_operation: Option<String>,
}

impl StagePresetSpec {
    pub fn parse(preset: &str) -> Self {
        if let Some(video) = preset.strip_prefix("video:") {
            return Self {
                video: match video {
                    "" | "source" => VideoSelector::Source,
                    "custom" => VideoSelector::Custom,
                    name => VideoSelector::Preset(name.to_string()),
                },
                audio_operation: None,
            };
        }

        if let Some(rest) = preset.strip_prefix("audio:") {
            let operation = rest.rsplit_once(":from:").map(|(op, _)| op).unwrap_or(rest);
            return Self {
                video: VideoSelector::Source,
                audio_operation: Some(operation.to_string()),
            };
        }

        let output = OutputEncodingSpec::parse(preset);
        Self {
            video: output.video,
            audio_operation: output.audio_operation,
        }
    }

    pub fn video(&self) -> &VideoSelector {
        &self.video
    }

    pub fn video_encoding(&self) -> &str {
        self.video.as_encoding_str()
    }

    pub fn audio_operation(&self) -> Option<&str> {
        self.audio_operation.as_deref()
    }
}
