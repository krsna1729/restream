//! Scheduled events.

use sqlx::{Row, SqlitePool};

#[derive(Debug, Clone)]
pub struct ScheduleRecord {
    pub id: String,
    pub pipeline_id: String,
    pub name: String,
    pub action: String,
    /// JSON array of output ids.
    pub output_ids: String,
    pub at_ms: i64,
    pub repeat: String,
    pub enabled: bool,
    pub last_run_ms: Option<i64>,
    pub last_result: Option<String>,
    pub created_at_ms: i64,
}

const COLUMNS: &str = "id, pipeline_id, name, action, output_ids, at, repeat, enabled, last_run, last_result, created_at";

fn record(row: &sqlx::sqlite::SqliteRow) -> ScheduleRecord {
    ScheduleRecord {
        id: row.get(0),
        pipeline_id: row.get(1),
        name: row.get(2),
        action: row.get(3),
        output_ids: row.get(4),
        at_ms: row.get(5),
        repeat: row.get(6),
        enabled: row.get::<i64, _>(7) != 0,
        last_run_ms: row.get(8),
        last_result: row.get(9),
        created_at_ms: row.get(10),
    }
}

pub async fn list(pool: &SqlitePool) -> Result<Vec<ScheduleRecord>, sqlx::Error> {
    let rows = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM schedules ORDER BY at, id"
    )))
    .fetch_all(pool)
    .await?;
    Ok(rows.iter().map(record).collect())
}

pub async fn get(pool: &SqlitePool, id: &str) -> Result<Option<ScheduleRecord>, sqlx::Error> {
    let row = sqlx::query(sqlx::AssertSqlSafe(format!(
        "SELECT {COLUMNS} FROM schedules WHERE id = ?"
    )))
    .bind(id)
    .fetch_optional(pool)
    .await?;
    Ok(row.as_ref().map(record))
}

pub async fn save(pool: &SqlitePool, record: &ScheduleRecord) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO schedules
           (id, pipeline_id, name, action, output_ids, at, repeat, enabled, last_run, last_result, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(id) DO UPDATE SET pipeline_id = excluded.pipeline_id, name = excluded.name,
           action = excluded.action, output_ids = excluded.output_ids, at = excluded.at,
           repeat = excluded.repeat, enabled = excluded.enabled, last_run = excluded.last_run,
           last_result = excluded.last_result",
    )
    .bind(&record.id)
    .bind(&record.pipeline_id)
    .bind(&record.name)
    .bind(&record.action)
    .bind(&record.output_ids)
    .bind(record.at_ms)
    .bind(&record.repeat)
    .bind(i64::from(record.enabled))
    .bind(record.last_run_ms)
    .bind(&record.last_result)
    .bind(record.created_at_ms)
    .execute(pool)
    .await?;
    Ok(())
}

pub async fn delete(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM schedules WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() == 1)
}

pub async fn record_run(
    pool: &SqlitePool,
    id: &str,
    occurrence_ms: i64,
    result: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query("UPDATE schedules SET last_run = ?, last_result = ? WHERE id = ?")
        .bind(occurrence_ms)
        .bind(result)
        .bind(id)
        .execute(pool)
        .await?;
    Ok(())
}
