use sqlx::{AssertSqlSafe, Row, SqlitePool};

pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 1;
pub(crate) const CURRENT_SCHEMA_MIGRATION_NAME: &str = "bootstrap_schema_v1";

pub(crate) async fn ensure_schema_migrations_table(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        );",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn record_schema_migration(
    pool: &SqlitePool,
    version: i64,
    name: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO schema_migrations (version, name)
         VALUES (?, ?)
         ON CONFLICT(version) DO UPDATE SET name = excluded.name;",
    )
    .bind(version)
    .bind(name)
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn ensure_column_exists(
    pool: &SqlitePool,
    table: &str,
    column: &str,
    column_type: &str,
) -> Result<(), sqlx::Error> {
    if !table_has_column(pool, table, column).await? {
        let alter = format!("ALTER TABLE {table} ADD COLUMN {column} {column_type}");
        sqlx::query(AssertSqlSafe(alter)).execute(pool).await?;
    }
    Ok(())
}

async fn table_has_column(
    pool: &SqlitePool,
    table: &str,
    column: &str,
) -> Result<bool, sqlx::Error> {
    let pragma = format!("PRAGMA table_info({table})");
    let rows = sqlx::query(AssertSqlSafe(pragma)).fetch_all(pool).await?;
    Ok(rows
        .iter()
        .any(|row| row.get::<String, _>("name") == column))
}

pub(crate) async fn backfill_output_configs(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    if !table_has_column(pool, "outputs", "encoding").await? {
        return Ok(());
    }
    let default_config =
        super::output_repo::serialize_config(&super::output_repo::default_config())?;
    let rows = sqlx::query(
        "SELECT id, COALESCE(config, '') AS config, COALESCE(encoding, '') AS encoding FROM outputs",
    )
    .fetch_all(pool)
    .await?;
    for row in rows {
        let id: String = row.get("id");
        let config_raw: String = row.get("config");
        let encoding_raw: String = row.get("encoding");
        let next_config = if !config_raw.trim().is_empty() {
            if super::output_repo::deserialize_config(&config_raw).is_ok() {
                continue;
            }
            super::output_repo::parse_encoding(&encoding_raw)
        } else if !encoding_raw.trim().is_empty() {
            super::output_repo::parse_encoding(&encoding_raw)
        } else {
            super::output_repo::default_config()
        };
        let next_config_json = super::output_repo::serialize_config(&next_config)?;
        sqlx::query("UPDATE outputs SET config = ? WHERE id = ?")
            .bind(if next_config_json.is_empty() {
                default_config.as_str()
            } else {
                next_config_json.as_str()
            })
            .bind(id)
            .execute(pool)
            .await?;
    }
    Ok(())
}

pub(crate) async fn prune_duplicate_ingests_by_stream_key(
    pool: &SqlitePool,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "DELETE FROM ingests
         WHERE rowid NOT IN (
             SELECT MAX(rowid)
             FROM ingests
             GROUP BY stream_key
         );",
    )
    .execute(pool)
    .await?;
    Ok(())
}

pub(crate) async fn ensure_no_duplicate_pipeline_stream_keys(
    pool: &SqlitePool,
) -> Result<(), sqlx::Error> {
    let duplicates = sqlx::query(
        "SELECT stream_key, GROUP_CONCAT(id, ',') AS pipeline_ids, COUNT(*) AS count
         FROM pipelines
         GROUP BY stream_key
         HAVING COUNT(*) > 1
         ORDER BY stream_key;",
    )
    .fetch_all(pool)
    .await?;
    if duplicates.is_empty() {
        return Ok(());
    }

    let report = duplicates
        .iter()
        .map(|row| {
            let stream_key: String = row.get("stream_key");
            let pipeline_ids: String = row.get("pipeline_ids");
            let count: i64 = row.get("count");
            format!("{stream_key} ({count} pipelines: {pipeline_ids})")
        })
        .collect::<Vec<_>>()
        .join("; ");
    Err(sqlx::Error::Protocol(format!(
        "duplicate pipeline stream keys must be resolved before migration: {report}"
    )))
}
