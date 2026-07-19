use sqlx::{AssertSqlSafe, Row, SqlitePool};

pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 1;
pub(crate) const CURRENT_SCHEMA_MIGRATION_NAME: &str = "pipeline_inputs_schema_v1";

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
