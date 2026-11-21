//! Persistence for ingest security configuration.
//! This file owns the metadata key and JSON round-tripping between the meta
//! table and the domain config; validation semantics live in
//! `restream_domain::domain::ingest_security`, while runtime enforcement lives in
//! `restream_engine::security`.

use crate::db::DeadlineDb;
use restream_domain::domain::ingest_security::IngestSecurityConfig;

pub const INGEST_SECURITY_CONFIG_META_KEY: &str = "ingest_security_config";

pub async fn load_ingest_security_config(db: &DeadlineDb) -> IngestSecurityConfig {
    let mut config = db
        .read(|pool| crate::db::get_meta(pool, INGEST_SECURITY_CONFIG_META_KEY))
        .await
        .ok()
        .flatten()
        .and_then(|raw| serde_json::from_str::<IngestSecurityConfig>(&raw).ok())
        .unwrap_or_default();
    config.normalize();
    config
}

pub async fn save_ingest_security_config(
    db: &DeadlineDb,
    config: &IngestSecurityConfig,
) -> Result<(), String> {
    let mut config = config.clone();
    config.normalize();
    let raw = serde_json::to_string(&config).map_err(|error| error.to_string())?;
    db.write(move |pool| async move {
        crate::db::set_meta(&pool, INGEST_SECURITY_CONFIG_META_KEY, &raw).await
    })
    .await
    .map(|_| ())
    .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::infrastructure::bootstrap::test_support::memory_pool;

    async fn db_with(value: Option<&str>) -> (DeadlineDb, sqlx::SqlitePool) {
        let pool = memory_pool().await;
        if let Some(value) = value {
            crate::db::set_meta(&pool, INGEST_SECURITY_CONFIG_META_KEY, value)
                .await
                .unwrap();
        }
        (DeadlineDb::api(pool.clone()), pool)
    }

    async fn persisted(pool: &sqlx::SqlitePool) -> IngestSecurityConfig {
        let raw = crate::db::get_meta(pool, INGEST_SECURITY_CONFIG_META_KEY)
            .await
            .unwrap()
            .unwrap();
        serde_json::from_str(&raw).unwrap()
    }

    #[tokio::test]
    async fn load_ingest_security_config_reads_valid_json() {
        let raw = serde_json::json!({
            "failureLimit": 3,
            "failureWindowMs": 10_000,
            "banMs": 30_000,
            "trackedIpLimit": 42
        })
        .to_string();
        let (db, _pool) = db_with(Some(&raw)).await;

        let config = load_ingest_security_config(&db).await;

        assert_eq!(config.failure_limit, 3);
        assert_eq!(config.failure_window_ms, 10_000);
        assert_eq!(config.ban_ms, 30_000);
        assert_eq!(config.tracked_ip_limit, 42);
    }

    #[tokio::test]
    async fn load_ingest_security_config_falls_back_to_default_on_error() {
        let (db, pool) = db_with(Some("{\"failureLimit\":\"bad\"}")).await;
        pool.close().await;

        let config = load_ingest_security_config(&db).await;
        let default = IngestSecurityConfig::default();

        assert_eq!(config.failure_limit, default.failure_limit);
        assert_eq!(config.failure_window_ms, default.failure_window_ms);
        assert_eq!(config.ban_ms, default.ban_ms);
        assert_eq!(config.tracked_ip_limit, default.tracked_ip_limit);
    }

    #[tokio::test]
    async fn load_ingest_security_config_normalizes_persisted_values() {
        let raw = serde_json::json!({
            "failureLimit": 0,
            "failureWindowMs": -10,
            "banMs": 0,
            "trackedIpLimit": -3
        })
        .to_string();
        let (db, _pool) = db_with(Some(&raw)).await;

        let config = load_ingest_security_config(&db).await;

        assert_eq!(config.failure_limit, 1);
        assert_eq!(config.failure_window_ms, 1);
        assert_eq!(config.ban_ms, 1);
        assert_eq!(config.tracked_ip_limit, 1);
    }

    #[tokio::test]
    async fn save_ingest_security_config_serializes_to_meta_store() {
        let (db, pool) = db_with(None).await;
        let config = IngestSecurityConfig {
            failure_limit: 3,
            failure_window_ms: 10_000,
            ban_ms: 30_000,
            tracked_ip_limit: 42,
        };

        save_ingest_security_config(&db, &config).await.unwrap();

        let roundtrip = persisted(&pool).await;
        assert_eq!(roundtrip.failure_limit, config.failure_limit);
        assert_eq!(roundtrip.failure_window_ms, config.failure_window_ms);
        assert_eq!(roundtrip.ban_ms, config.ban_ms);
        assert_eq!(roundtrip.tracked_ip_limit, config.tracked_ip_limit);
    }

    #[tokio::test]
    async fn save_ingest_security_config_persists_normalized_values() {
        let (db, pool) = db_with(None).await;
        let config = IngestSecurityConfig {
            failure_limit: 0,
            failure_window_ms: -10,
            ban_ms: 0,
            tracked_ip_limit: -3,
        };

        save_ingest_security_config(&db, &config).await.unwrap();

        let roundtrip = persisted(&pool).await;
        assert_eq!(roundtrip.failure_limit, 1);
        assert_eq!(roundtrip.failure_window_ms, 1);
        assert_eq!(roundtrip.ban_ms, 1);
        assert_eq!(roundtrip.tracked_ip_limit, 1);
    }
}
