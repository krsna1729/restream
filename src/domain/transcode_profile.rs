//! Domain model for named transcode profile settings.

use std::collections::HashMap;

/// Encoder settings for a single transcode profile.
#[derive(Debug, Clone, serde::Deserialize, serde::Serialize)]
pub struct TranscodeProfile {
    /// x264 preset: ultrafast, superfast, veryfast, faster, fast, medium, slow, slower.
    #[serde(default = "default_preset")]
    pub preset: String,

    /// x264 tune: zerolatency, fastdecode, animation, film, etc.
    #[serde(default = "default_tune")]
    pub tune: String,

    /// CRF (constant quality) value. Used when bitrate == 0.
    /// Range 0-51, lower = higher quality. 23 is x264 default.
    #[serde(default = "default_crf")]
    pub crf: i32,

    /// GOP size (keyframe interval in frames).
    #[serde(default = "default_gop")]
    pub gop: u32,

    /// Max B-frames. 0 for realtime (no reordering, lowest latency).
    #[serde(default = "default_bframes")]
    pub bframes: usize,

    /// Target bitrate in bps. 0 = use CRF mode.
    #[serde(default)]
    pub bitrate: i64,

    /// Max bitrate in bps (for VBV). 0 = no VBV limit.
    #[serde(default, rename = "maxBitrate")]
    pub max_bitrate: i64,

    /// Output width. 0 = match source.
    #[serde(default)]
    pub width: u32,

    /// Output height. 0 = match source.
    #[serde(default)]
    pub height: u32,
}

fn default_preset() -> String {
    "ultrafast".to_string()
}
fn default_tune() -> String {
    "zerolatency".to_string()
}
fn default_crf() -> i32 {
    23
}
fn default_gop() -> u32 {
    60
}
fn default_bframes() -> usize {
    0
}

impl Default for TranscodeProfile {
    fn default() -> Self {
        Self {
            preset: default_preset(),
            tune: default_tune(),
            crf: default_crf(),
            gop: default_gop(),
            bframes: default_bframes(),
            bitrate: 0,
            max_bitrate: 0,
            width: 0,
            height: 0,
        }
    }
}

impl TranscodeProfile {
    pub fn validate(&self) -> Result<(), &'static str> {
        let valid_presets = [
            "ultrafast",
            "superfast",
            "veryfast",
            "faster",
            "fast",
            "medium",
            "slow",
            "slower",
            "veryslow",
            "placebo",
        ];
        if !valid_presets.contains(&self.preset.as_str()) {
            return Err(
                "preset must be one of: ultrafast, superfast, veryfast, faster, fast, medium, slow, slower, veryslow, placebo",
            );
        }
        let valid_tunes = [
            "",
            "film",
            "animation",
            "grain",
            "stillimage",
            "psnr",
            "ssim",
            "fastdecode",
            "zerolatency",
        ];
        if !valid_tunes.contains(&self.tune.as_str()) {
            return Err(
                "tune must be one of: film, animation, grain, stillimage, psnr, ssim, fastdecode, zerolatency, or empty",
            );
        }
        if !(0..=51).contains(&self.crf) {
            return Err("crf must be between 0 and 51");
        }
        Ok(())
    }
}

/// All profiles, keyed by name.
pub type TranscodeProfiles = HashMap<String, TranscodeProfile>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_profile_validates() {
        assert!(TranscodeProfile::default().validate().is_ok());
    }

    #[test]
    fn all_documented_presets_validate() {
        let presets = [
            "ultrafast",
            "superfast",
            "veryfast",
            "faster",
            "fast",
            "medium",
            "slow",
            "slower",
            "veryslow",
            "placebo",
        ];
        for preset in presets {
            let profile = TranscodeProfile {
                preset: preset.to_string(),
                ..TranscodeProfile::default()
            };
            assert!(
                profile.validate().is_ok(),
                "preset {preset} should validate"
            );
        }
    }

    #[test]
    fn unknown_preset_is_rejected() {
        let profile = TranscodeProfile {
            preset: "ultraslow".to_string(),
            ..TranscodeProfile::default()
        };
        assert!(profile.validate().is_err());
    }

    #[test]
    fn preset_matching_is_case_sensitive() {
        let profile = TranscodeProfile {
            preset: "Ultrafast".to_string(),
            ..TranscodeProfile::default()
        };
        assert!(
            profile.validate().is_err(),
            "whitelist match must be exact-case, not case-insensitive"
        );
    }

    #[test]
    fn empty_tune_is_valid() {
        let profile = TranscodeProfile {
            tune: String::new(),
            ..TranscodeProfile::default()
        };
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn all_documented_tunes_validate() {
        let tunes = [
            "",
            "film",
            "animation",
            "grain",
            "stillimage",
            "psnr",
            "ssim",
            "fastdecode",
            "zerolatency",
        ];
        for tune in tunes {
            let profile = TranscodeProfile {
                tune: tune.to_string(),
                ..TranscodeProfile::default()
            };
            assert!(profile.validate().is_ok(), "tune {tune:?} should validate");
        }
    }

    #[test]
    fn unknown_tune_is_rejected() {
        let profile = TranscodeProfile {
            tune: "nonexistent".to_string(),
            ..TranscodeProfile::default()
        };
        assert!(profile.validate().is_err());
    }

    #[test]
    fn crf_boundaries_are_inclusive() {
        for crf in [0, 51] {
            let profile = TranscodeProfile {
                crf,
                ..TranscodeProfile::default()
            };
            assert!(
                profile.validate().is_ok(),
                "crf {crf} is within [0, 51] and must validate"
            );
        }
    }

    #[test]
    fn crf_outside_boundaries_is_rejected() {
        for crf in [-1, 52, i32::MIN, i32::MAX] {
            let profile = TranscodeProfile {
                crf,
                ..TranscodeProfile::default()
            };
            assert!(
                profile.validate().is_err(),
                "crf {crf} is outside [0, 51] and must be rejected"
            );
        }
    }

    #[test]
    fn validate_does_not_bound_bitrate_gop_or_dimensions() {
        // `validate()` only checks preset/tune/crf. Negative bitrate, zero gop,
        // and extreme dimensions are accepted -- pinning the current contract
        // (callers rely on 0 as a "use source/no limit" sentinel) rather than
        // asserting these values are sensible.
        let profile = TranscodeProfile {
            bitrate: -1,
            max_bitrate: -1,
            gop: 0,
            bframes: usize::MAX,
            width: u32::MAX,
            height: u32::MAX,
            ..TranscodeProfile::default()
        };
        assert!(profile.validate().is_ok());
    }

    #[test]
    fn deserialize_fills_missing_fields_with_defaults() {
        let profile: TranscodeProfile =
            serde_json::from_str("{}").expect("empty object deserializes");
        assert_eq!(profile.preset, "ultrafast");
        assert_eq!(profile.tune, "zerolatency");
        assert_eq!(profile.crf, 23);
        assert_eq!(profile.gop, 60);
        assert_eq!(profile.bframes, 0);
        assert_eq!(profile.bitrate, 0);
        assert_eq!(profile.max_bitrate, 0);
        assert_eq!(profile.width, 0);
        assert_eq!(profile.height, 0);
    }

    #[test]
    fn deserialize_accepts_negative_bitrate_without_validation() {
        let profile: TranscodeProfile =
            serde_json::from_str(r#"{"bitrate": -5000}"#).expect("negative bitrate deserializes");
        assert_eq!(profile.bitrate, -5000);
    }

    #[test]
    fn deserialize_ignores_unknown_fields() {
        let profile: TranscodeProfile =
            serde_json::from_str(r#"{"preset": "fast", "bogusField": 123}"#)
                .expect("unknown fields are silently ignored, not rejected");
        assert_eq!(profile.preset, "fast");
    }
}
