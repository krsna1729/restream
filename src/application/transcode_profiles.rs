//! Application-layer persistence for transcode profiles.
//! This file owns the meta-table key, JSON round-tripping, and syncing the
//! persisted profile set into the runtime cache used by the media layer.

use crate::db::DeadlineDb;
use restream_domain::domain::transcode_profile::TranscodeProfiles;
use tracing::{info, warn};

pub const TRANSCODE_PROFILES_META_KEY: &str = "transcode_profiles";

pub fn default_transcode_profiles() -> TranscodeProfiles {
    restream_engine::profiles::built_in_defaults()
}

#[expect(
    clippy::cognitive_complexity,
    reason = "predates the complexity limit; split it before growing it"
)]
pub async fn load_transcode_profiles(db: &DeadlineDb) {
    let stored = db
        .read(|pool| crate::db::get_meta(pool, TRANSCODE_PROFILES_META_KEY))
        .await;
    let profiles = match stored {
        Ok(Some(json_str)) => match serde_json::from_str::<TranscodeProfiles>(&json_str) {
            Ok(p) if !p.is_empty() => {
                info!(count = p.len(), "loaded profiles from meta store");
                p
            }
            Ok(_) => {
                warn!("meta store has empty profiles, using defaults");
                default_transcode_profiles()
            }
            Err(error) => {
                warn!(err = %error, "failed to parse profiles, using defaults");
                default_transcode_profiles()
            }
        },
        Ok(None) => {
            info!("no persisted profiles found, using built-in defaults");
            default_transcode_profiles()
        }
        Err(error) => {
            warn!(err = %error, "failed to load profiles, using defaults");
            default_transcode_profiles()
        }
    };

    restream_engine::profiles::replace_runtime_profiles(&profiles).await;
}

pub async fn save_transcode_profiles(
    db: &DeadlineDb,
    profiles: &TranscodeProfiles,
) -> Result<(), String> {
    let json = serde_json::to_string(profiles).map_err(|error| error.to_string())?;
    db.write(move |pool| async move {
        crate::db::set_meta(&pool, TRANSCODE_PROFILES_META_KEY, &json).await
    })
    .await
    .map_err(|error| error.to_string())?;
    restream_engine::profiles::replace_runtime_profiles(profiles).await;
    info!(
        count = profiles.len(),
        "updated profiles in meta store and runtime cache"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::bootstrap::test_support::memory_pool;
    use restream_domain::domain::transcode_profile::{TranscodeProfile, TranscodeProfiles};

    fn custom_profiles() -> TranscodeProfiles {
        TranscodeProfiles::from([(
            "4k60".to_string(),
            TranscodeProfile {
                preset: "ultrafast".to_string(),
                tune: "zerolatency".to_string(),
                crf: 23,
                gop: 60,
                bframes: 0,
                bitrate: 20_000_000,
                max_bitrate: 24_000_000,
                width: 3840,
                height: 2160,
            },
        )])
    }

    #[tokio::test]
    async fn save_transcode_profiles_updates_runtime_cache_with_built_ins() {
        let db = DeadlineDb::api(memory_pool().await);

        save_transcode_profiles(&db, &custom_profiles())
            .await
            .unwrap();

        let runtime = restream_engine::profiles::current_effective().await;
        assert!(runtime.contains_key("h264"));
        assert!(runtime.contains_key("720p"));
        assert_eq!(runtime["4k60"].width, 3840);
    }

    #[tokio::test]
    async fn load_transcode_profiles_populates_runtime_cache_with_built_ins() {
        let pool = memory_pool().await;
        let raw = serde_json::to_string(&custom_profiles()).unwrap();
        crate::db::set_meta(&pool, TRANSCODE_PROFILES_META_KEY, &raw)
            .await
            .unwrap();

        load_transcode_profiles(&DeadlineDb::api(pool)).await;

        let runtime = restream_engine::profiles::current_effective().await;
        assert!(runtime.contains_key("h264"));
        assert!(runtime.contains_key("1080p"));
        assert_eq!(runtime["4k60"].height, 2160);
    }
}
