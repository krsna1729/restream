//! Schema versions. A new database is created at [`CURRENT_SCHEMA_VERSION`].
//! A database from an earlier version is rebuilt onto the canonical table
//! definitions (`schema::TABLES`) in one transaction: each table is
//! recreated and every column the new definition still has is copied, so
//! dropped columns go, new ones take their defaults, and widened CHECKs,
//! new foreign keys and indexes apply to every database the same way.

use sqlx::{Acquire, AssertSqlSafe, Row, SqlitePool};

pub(crate) const CURRENT_SCHEMA_VERSION: i64 = 3;
const CURRENT_SCHEMA_NAME: &str = "canonical_tables_v3";

/// Bring an existing database to the current version; a new one is left for
/// the schema setup to create.
pub(crate) async fn upgrade(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "CREATE TABLE IF NOT EXISTS schema_migrations (
            version INTEGER PRIMARY KEY,
            name TEXT NOT NULL,
            applied_at TEXT NOT NULL DEFAULT (datetime('now'))
        )",
    )
    .execute(pool)
    .await?;
    let version: Option<i64> = sqlx::query_scalar("SELECT MAX(version) FROM schema_migrations")
        .fetch_one(pool)
        .await?;
    if let Some(version) = version.filter(|version| *version > CURRENT_SCHEMA_VERSION) {
        return Err(sqlx::Error::Configuration(
            format!(
                "the database is at schema version {version}, newer than this build's \
                 {CURRENT_SCHEMA_VERSION}; run the release that wrote it"
            )
            .into(),
        ));
    }
    if version == Some(CURRENT_SCHEMA_VERSION) {
        return Ok(());
    }
    let existing: Vec<String> =
        sqlx::query_scalar("SELECT name FROM sqlite_master WHERE type = 'table'")
            .fetch_all(pool)
            .await?;
    let tables: Vec<(&str, &str)> = super::schema::TABLES
        .iter()
        .copied()
        .filter(|(name, _)| existing.iter().any(|table| table == name))
        .collect();
    if tables.is_empty() {
        return Ok(());
    }
    rebuild(pool, &tables).await
}

/// Record that the database matches the current definitions.
pub(crate) async fn record_current(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO schema_migrations (version, name) VALUES (?, ?)
         ON CONFLICT(version) DO UPDATE SET name = excluded.name",
    )
    .bind(CURRENT_SCHEMA_VERSION)
    .bind(CURRENT_SCHEMA_NAME)
    .execute(pool)
    .await?;
    Ok(())
}

/// Recreate `tables` (parents first) on their canonical definitions. Foreign
/// keys are off for the swap, or dropping a parent would cascade into its
/// children; rows a new foreign key refuses (a schedule of a pipeline deleted
/// before schedules had one) are removed before the commit.
async fn rebuild(pool: &SqlitePool, tables: &[(&str, &str)]) -> Result<(), sqlx::Error> {
    let mut conn = pool.acquire().await?;
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await?;
    let result = async {
        let mut tx = conn.begin().await?;
        for (name, create) in tables {
            let staged = format!("{name}__rebuild");
            let prefix = format!("CREATE TABLE IF NOT EXISTS {name} (");
            let ddl = create.replacen(&prefix, &format!("CREATE TABLE {staged} ("), 1);
            assert_ne!(
                &ddl, create,
                "canonical definition of {name} starts with {prefix}"
            );
            sqlx::query(AssertSqlSafe(ddl)).execute(&mut *tx).await?;
            let kept = columns(&mut tx, name).await?;
            let shared: Vec<String> = columns(&mut tx, &staged)
                .await?
                .into_iter()
                .filter(|column| kept.contains(column))
                .map(|column| format!("\"{column}\""))
                .collect();
            let shared = shared.join(", ");
            let copy = format!("INSERT INTO {staged} ({shared}) SELECT {shared} FROM {name}");
            sqlx::query(AssertSqlSafe(copy)).execute(&mut *tx).await?;
            sqlx::query(AssertSqlSafe(format!("DROP TABLE {name}")))
                .execute(&mut *tx)
                .await?;
            sqlx::query(AssertSqlSafe(format!(
                "ALTER TABLE {staged} RENAME TO {name}"
            )))
            .execute(&mut *tx)
            .await?;
        }
        let orphans = sqlx::query("PRAGMA foreign_key_check")
            .fetch_all(&mut *tx)
            .await?;
        for orphan in orphans {
            let table: String = orphan.get(0);
            let rowid: i64 = orphan.get(1);
            sqlx::query(AssertSqlSafe(format!(
                "DELETE FROM \"{table}\" WHERE rowid = ?"
            )))
            .bind(rowid)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await
    }
    .await;
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await?;
    result
}

async fn columns(
    conn: &mut sqlx::SqliteConnection,
    table: &str,
) -> Result<Vec<String>, sqlx::Error> {
    let rows = sqlx::query(AssertSqlSafe(format!("PRAGMA table_info({table})")))
        .fetch_all(conn)
        .await?;
    Ok(rows
        .iter()
        .map(|row| row.get::<String, _>("name"))
        .collect())
}

#[cfg(test)]
#[path = "migrations_tests.rs"]
mod tests;
