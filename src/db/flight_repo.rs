//! The flight recorder's persisted history: one JSON sample per pipeline
//! per persisted tick, keyed by (pipeline, time).

use sqlx::{Row, SqlitePool};

/// Writes one tick's samples in one transaction.
pub async fn insert_batch(
    pool: &SqlitePool,
    rows: &[(String, i64, String)],
) -> Result<(), sqlx::Error> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut tx = pool.begin().await?;
    for (pipeline_id, at_ms, sample) in rows {
        sqlx::query(
            "INSERT OR REPLACE INTO flight_samples (pipeline_id, at_ms, sample) VALUES (?, ?, ?)",
        )
        .bind(pipeline_id)
        .bind(at_ms)
        .bind(sample)
        .execute(&mut *tx)
        .await?;
    }
    tx.commit().await
}

/// One pipeline's samples in `[since_ms, until_ms)`, oldest first.
pub async fn window(
    pool: &SqlitePool,
    pipeline_id: &str,
    since_ms: i64,
    until_ms: i64,
) -> Result<Vec<(i64, String)>, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT at_ms, sample FROM flight_samples
         WHERE pipeline_id = ? AND at_ms >= ? AND at_ms < ? ORDER BY at_ms",
    )
    .bind(pipeline_id)
    .bind(since_ms)
    .bind(until_ms)
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(|row| (row.get(0), row.get(1))).collect())
}

/// The oldest kept sample of a pipeline, if any.
pub async fn oldest(pool: &SqlitePool, pipeline_id: &str) -> Result<Option<i64>, sqlx::Error> {
    sqlx::query("SELECT MIN(at_ms) FROM flight_samples WHERE pipeline_id = ?")
        .bind(pipeline_id)
        .fetch_one(pool)
        .await
        .map(|row| row.get(0))
}

/// Drops every sample older than `before_ms`.
pub async fn prune(pool: &SqlitePool, before_ms: i64) -> Result<u64, sqlx::Error> {
    sqlx::query("DELETE FROM flight_samples WHERE at_ms < ?")
        .bind(before_ms)
        .execute(pool)
        .await
        .map(|result| result.rows_affected())
}

/// Drops a deleted pipeline's history.
pub async fn forget(pool: &SqlitePool, pipeline_id: &str) -> Result<(), sqlx::Error> {
    sqlx::query("DELETE FROM flight_samples WHERE pipeline_id = ?")
        .bind(pipeline_id)
        .execute(pool)
        .await
        .map(|_| ())
}
