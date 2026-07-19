//! Runtime transcode profile cache and built-in defaults used by the
//! transcoder and API-facing settings reads.
//!
//! Profiles are looked up by name (e.g. "h264", "720p") and control all
//! encoder settings. Persistence and JSON/meta-table round-tripping live in
//! `crate::application::transcode_profiles`; this module only owns built-ins
//! plus the in-memory cache consumed on hot runtime paths.
//!
//! - `bitrate: 0` → CRF mode (constant quality, adapts to content)
//! - `width/height: 0` → passthrough (match source resolution)

use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;

use crate::domain::transcode_profile::{TranscodeProfile, TranscodeProfiles};

pub const BASELINE_TRANSCODE_PROFILE_KEY: &str = "h264";

/// Runtime cache of profiles. Loaded from DB at startup, updated when
/// the settings API patches the config. The transcoder reads from this
/// cache when initializing an encoder.
static PROFILES: std::sync::OnceLock<Arc<RwLock<TranscodeProfiles>>> = std::sync::OnceLock::new();

/// Get the global profiles cache (initializes on first call).
pub fn cache() -> &'static Arc<RwLock<TranscodeProfiles>> {
    PROFILES.get_or_init(|| Arc::new(RwLock::new(built_in_defaults())))
}

/// Return built-ins plus configured profiles, with configured profiles
/// overriding same-named built-ins.
pub fn effective_profiles(profiles: &TranscodeProfiles) -> TranscodeProfiles {
    let mut effective = built_in_defaults();
    for (name, profile) in profiles {
        effective.insert(name.clone(), profile.clone());
    }
    effective
}

/// Get the profile set currently exposed to API consumers and transcoders.
pub async fn current_effective() -> TranscodeProfiles {
    let cache = cache().read().await;
    effective_profiles(&cache)
}

/// Replace the runtime cache from a persisted/configured profile set.
pub async fn replace_runtime_profiles(profiles: &TranscodeProfiles) {
    let mut cache = cache().write().await;
    *cache = effective_profiles(profiles);
}

fn resolve_from_profiles(profiles: &TranscodeProfiles, name: &str) -> TranscodeProfile {
    profiles
        .get(name)
        .or_else(|| profiles.get(BASELINE_TRANSCODE_PROFILE_KEY))
        .cloned()
        .unwrap_or_default()
}

/// Get a profile by name. Falls back to the baseline passthrough/transcode
/// profile used for H.264-shaped outputs, then to the type default.
/// Called by transcoders when initializing an encoder.
pub async fn get(name: &str) -> TranscodeProfile {
    let cache = cache().read().await;
    resolve_from_profiles(&cache, name)
}

/// Get a profile without blocking the current thread.
///
/// Runtime media tasks call this from async contexts while constructing worker
/// arguments. If another task is updating the cache, fall back to built-ins
/// instead of blocking a Tokio worker.
pub fn try_get_cached(name: &str) -> TranscodeProfile {
    cache()
        .try_read()
        .map(|cache| resolve_from_profiles(&cache, name))
        .unwrap_or_else(|_| resolve_from_profiles(&built_in_defaults(), name))
}

pub fn get_blocking(name: &str) -> TranscodeProfile {
    let cache = cache().blocking_read();
    resolve_from_profiles(&cache, name)
}

/// Look up preset dimensions without blocking the current thread.
///
/// Called from async egress startup paths (e.g. SRT keyframe preroll
/// policy), so it must use [`try_get_cached`] rather than
/// [`get_blocking`] — `blocking_read()` on the shared `RwLock` panics
/// when invoked from a Tokio worker thread.
pub fn dimensions_for_preset(name: &str) -> Option<(u32, u32)> {
    let profile = try_get_cached(name);
    (profile.width > 0 && profile.height > 0).then_some((profile.width, profile.height))
}

pub fn baseline_profile() -> TranscodeProfile {
    get_blocking(BASELINE_TRANSCODE_PROFILE_KEY)
}

/// Built-in realtime defaults. Used when no DB config is present.
/// All settings are optimized for live streaming: lowest latency, no reordering.
pub fn built_in_defaults() -> TranscodeProfiles {
    let mut profiles = HashMap::new();

    // H.265→H.264 transcode: same resolution, CRF mode
    profiles.insert(
        "h264".to_string(),
        TranscodeProfile {
            preset: "ultrafast".into(),
            tune: "zerolatency".into(),
            crf: 23,
            gop: 60,
            bframes: 0,
            bitrate: 0,
            max_bitrate: 0,
            width: 0,
            height: 0,
        },
    );

    // 720p preset
    profiles.insert(
        "720p".to_string(),
        TranscodeProfile {
            preset: "ultrafast".into(),
            tune: "zerolatency".into(),
            crf: 23,
            gop: 60,
            bframes: 0,
            bitrate: 0,
            max_bitrate: 0,
            width: 1280,
            height: 720,
        },
    );

    // 1080p preset
    profiles.insert(
        "1080p".to_string(),
        TranscodeProfile {
            preset: "ultrafast".into(),
            tune: "zerolatency".into(),
            crf: 23,
            gop: 60,
            bframes: 0,
            bitrate: 0,
            max_bitrate: 0,
            width: 1920,
            height: 1080,
        },
    );

    profiles
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_realtime() {
        let p = TranscodeProfile::default();
        assert_eq!(p.preset, "ultrafast");
        assert_eq!(p.tune, "zerolatency");
        assert_eq!(p.bframes, 0);
        assert_eq!(p.bitrate, 0); // CRF mode
    }

    #[test]
    fn built_in_has_h264_and_720p() {
        let profiles = built_in_defaults();
        assert!(profiles.contains_key(BASELINE_TRANSCODE_PROFILE_KEY));
        assert!(profiles.contains_key("720p"));
        assert!(profiles.contains_key("1080p"));
    }

    #[test]
    fn empty_profiles_resolve_to_built_ins() {
        let profiles = TranscodeProfiles::new();
        let effective = effective_profiles(&profiles);
        assert!(effective.contains_key("h264"));
        assert!(effective.contains_key("720p"));
        assert!(effective.contains_key("1080p"));
    }

    #[test]
    fn configured_profiles_extend_and_override_built_ins() {
        let mut profiles = TranscodeProfiles::new();
        profiles.insert(
            "custom_4k".to_string(),
            TranscodeProfile {
                width: 3840,
                height: 2160,
                ..TranscodeProfile::default()
            },
        );
        profiles.insert(
            "720p".to_string(),
            TranscodeProfile {
                crf: 20,
                width: 1280,
                height: 720,
                ..TranscodeProfile::default()
            },
        );

        let effective = effective_profiles(&profiles);
        assert_eq!(effective["720p"].crf, 20);
        assert_eq!(effective["custom_4k"].width, 3840);
        assert!(effective.contains_key("h264"));
        assert!(effective.contains_key("1080p"));
    }

    #[test]
    fn serialize_deserialize_roundtrip() {
        let mut profiles = built_in_defaults();
        profiles.insert(
            "custom".to_string(),
            TranscodeProfile {
                preset: "veryfast".into(),
                tune: "film".into(),
                crf: 18,
                gop: 120,
                bframes: 2,
                bitrate: 15000000,
                max_bitrate: 20000000,
                width: 3840,
                height: 2160,
            },
        );

        let json = serde_json::to_string(&profiles).unwrap();
        let parsed: TranscodeProfiles = serde_json::from_str(&json).unwrap();

        let custom = parsed.get("custom").unwrap();
        assert_eq!(custom.preset, "veryfast");
        assert_eq!(custom.crf, 18);
        assert_eq!(custom.bitrate, 15000000);
        assert_eq!(custom.width, 3840);

        // Defaults still present
        assert!(parsed.contains_key("h264"));
    }

    #[test]
    fn partial_json_uses_defaults() {
        // Only specify preset + crf, rest should default
        let json = r#"{"test": {"preset": "slow", "crf": 18}}"#;
        let parsed: TranscodeProfiles = serde_json::from_str(json).unwrap();
        let p = parsed.get("test").unwrap();
        assert_eq!(p.preset, "slow");
        assert_eq!(p.crf, 18);
        assert_eq!(p.tune, "zerolatency"); // defaulted
        assert_eq!(p.bframes, 0); // defaulted
        assert_eq!(p.gop, 60); // defaulted
    }

    #[test]
    fn validate_all_valid_presets_pass() {
        for preset in [
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
        ] {
            let p = TranscodeProfile {
                preset: preset.into(),
                ..Default::default()
            };
            assert!(p.validate().is_ok(), "preset '{preset}' should be valid");
        }
    }

    #[test]
    fn validate_invalid_preset_rejected() {
        let p = TranscodeProfile {
            preset: "bogus".into(),
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_invalid_tune_rejected() {
        let p = TranscodeProfile {
            tune: "bogus".into(),
            ..Default::default()
        };
        assert!(p.validate().is_err());
    }

    #[test]
    fn validate_empty_tune_passes() {
        let p = TranscodeProfile {
            tune: String::new(),
            ..Default::default()
        };
        assert!(p.validate().is_ok());
    }

    #[test]
    fn validate_crf_boundaries() {
        assert!(
            TranscodeProfile {
                crf: 0,
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert!(
            TranscodeProfile {
                crf: 51,
                ..Default::default()
            }
            .validate()
            .is_ok()
        );
        assert!(
            TranscodeProfile {
                crf: -1,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            TranscodeProfile {
                crf: 52,
                ..Default::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn validate_default_passes() {
        assert!(TranscodeProfile::default().validate().is_ok());
    }

    #[test]
    fn builtin_720p_has_correct_dimensions() {
        let profiles = built_in_defaults();
        let p = &profiles["720p"];
        assert_eq!(p.width, 1280);
        assert_eq!(p.height, 720);
    }

    #[test]
    fn builtin_h264_is_passthrough() {
        let profiles = built_in_defaults();
        let p = &profiles["h264"];
        assert_eq!(p.width, 0);
        assert_eq!(p.height, 0);
        assert_eq!(p.preset, "ultrafast");
        assert_eq!(p.tune, "zerolatency");
    }

    // resolve_from_profiles is the pure three-tier fallback (exact name ->
    // baseline key -> type default) that every cache read (get,
    // try_get_cached, get_blocking) ultimately delegates to. It is tested
    // directly here, against a local TranscodeProfiles map, rather than via
    // the process-global cache functions: the cache is a single OnceLock
    // shared by every test in the binary, and `cargo test` runs tests in
    // parallel OS threads, so asserting on cache contents mutated by
    // `replace_runtime_profiles` elsewhere would be inherently racy.

    #[test]
    fn resolve_from_profiles_returns_exact_match_when_present() {
        let mut profiles = TranscodeProfiles::new();
        profiles.insert(
            "720p".to_string(),
            TranscodeProfile {
                crf: 30,
                ..TranscodeProfile::default()
            },
        );
        profiles.insert("h264".to_string(), TranscodeProfile::default());

        let resolved = resolve_from_profiles(&profiles, "720p");
        assert_eq!(resolved.crf, 30);
    }

    #[test]
    fn resolve_from_profiles_falls_back_to_baseline_when_name_missing() {
        let mut profiles = TranscodeProfiles::new();
        profiles.insert(
            BASELINE_TRANSCODE_PROFILE_KEY.to_string(),
            TranscodeProfile {
                crf: 17,
                ..TranscodeProfile::default()
            },
        );

        let resolved = resolve_from_profiles(&profiles, "no-such-preset");
        assert_eq!(
            resolved.crf, 17,
            "an unknown name must fall back to the baseline profile, not the type default"
        );
    }

    #[test]
    fn resolve_from_profiles_falls_back_to_type_default_when_baseline_also_missing() {
        // Neither the requested name nor the baseline key are present —
        // e.g. an empty or corrupted profile set. Must not panic; must
        // return TranscodeProfile::default() rather than an arbitrary entry.
        let profiles = TranscodeProfiles::new();
        let resolved = resolve_from_profiles(&profiles, "no-such-preset");
        let default = TranscodeProfile::default();
        assert_eq!(resolved.preset, default.preset);
        assert_eq!(resolved.tune, default.tune);
        assert_eq!(resolved.crf, default.crf);
        assert_eq!(resolved.width, default.width);
        assert_eq!(resolved.height, default.height);
    }

    #[test]
    fn resolve_from_profiles_empty_name_does_not_accidentally_match() {
        let mut profiles = TranscodeProfiles::new();
        profiles.insert(
            BASELINE_TRANSCODE_PROFILE_KEY.to_string(),
            TranscodeProfile {
                crf: 11,
                ..TranscodeProfile::default()
            },
        );

        let resolved = resolve_from_profiles(&profiles, "");
        assert_eq!(
            resolved.crf, 11,
            "empty string is not a real profile name and must fall back to baseline"
        );
    }
}
