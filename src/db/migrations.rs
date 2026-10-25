use sqlx::{AssertSqlSafe, Row, SqlitePool};

pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 3;
pub(crate) const CURRENT_SCHEMA_MIGRATION_NAME: &str = "pipeline_inputs_srt_call_v3";

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

/// Rebuild `pipeline_inputs` when its stored definition predates the
/// `srt_call` source kind. Rows, ids and every column carry over; nothing
/// references this table, and its indexes and trigger are recreated by the
/// schema setup that runs after this. A database already carrying the kind
/// is left untouched.
pub(crate) async fn widen_pipeline_input_source_kinds(
    pool: &SqlitePool,
) -> Result<(), sqlx::Error> {
    let stored: Option<String> = sqlx::query_scalar(
        "SELECT sql FROM sqlite_master WHERE type = 'table' AND name = 'pipeline_inputs'",
    )
    .fetch_optional(pool)
    .await?;
    if stored.is_none_or(|sql| sql.contains("'srt_call'")) {
        return Ok(());
    }
    const COLUMNS: &str = "id, pipeline_id, label, stream_key, role, enabled, selected, \
                           source_kind, source_url, variant_rule, source_auth, push_auth";
    let mut tx = pool.begin().await?;
    sqlx::query(
        "CREATE TABLE pipeline_inputs_v3 (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            label TEXT NOT NULL CHECK(length(trim(label)) > 0),
            stream_key TEXT NOT NULL CHECK(length(stream_key) > 0),
            role TEXT NOT NULL CHECK(role IN ('primary', 'backup')),
            enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
            selected INTEGER NOT NULL DEFAULT 0 CHECK(selected IN (0, 1)),
            source_kind TEXT NOT NULL DEFAULT 'keyed' CHECK(source_kind IN ('keyed', 'hls_pull', 'srt_call')),
            source_url TEXT,
            variant_rule TEXT NOT NULL DEFAULT 'highest_bandwidth' CHECK(variant_rule IN ('highest_bandwidth', 'lowest_bandwidth')),
            source_auth TEXT CHECK(source_auth IS NULL OR (json_valid(source_auth) AND json_extract(source_auth, '$.kind') IN ('none', 'bearer', 'cookies', 'basic', 'headers'))),
            push_auth TEXT CHECK(push_auth IS NULL OR (json_valid(push_auth) AND json_extract(push_auth, '$.kind') IN ('none', 'basic'))),
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        );",
    )
    .execute(&mut *tx)
    .await?;
    let copy =
        format!("INSERT INTO pipeline_inputs_v3 ({COLUMNS}) SELECT {COLUMNS} FROM pipeline_inputs");
    sqlx::query(AssertSqlSafe(copy)).execute(&mut *tx).await?;
    sqlx::query("DROP TABLE pipeline_inputs")
        .execute(&mut *tx)
        .await?;
    sqlx::query("ALTER TABLE pipeline_inputs_v3 RENAME TO pipeline_inputs")
        .execute(&mut *tx)
        .await?;
    tx.commit().await
}
