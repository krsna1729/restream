use crate::application::models::Ingest;
use sqlx::SqlitePool;

#[derive(sqlx::FromRow)]
struct IngestRow {
    id: String,
    filename: String,
    stream_key: String,
    #[sqlx(rename = "loop")]
    loop_flag: bool,
    start_time: String,
    live_optimized: bool,
    target_gop_seconds: u32,
}

impl From<IngestRow> for Ingest {
    fn from(row: IngestRow) -> Self {
        Self {
            id: row.id,
            filename: row.filename,
            stream_key: row.stream_key,
            loop_flag: row.loop_flag,
            start_time: row.start_time,
            live_optimized: row.live_optimized,
            target_gop_seconds: row.target_gop_seconds,
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn create_ingest(
    pool: &SqlitePool,
    id: &str,
    filename: &str,
    stream_key: &str,
    loop_flag: bool,
    start_time: &str,
    live_optimized: bool,
    target_gop_seconds: u32,
) -> Result<Ingest, sqlx::Error> {
    sqlx::query(
        "INSERT INTO ingests (id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(filename)
    .bind(stream_key)
    .bind(if loop_flag { 1 } else { 0 })
    .bind(start_time)
    .bind(if live_optimized { 1 } else { 0 })
    .bind(i64::from(target_gop_seconds))
    .execute(pool)
    .await?;

    get_ingest(pool, id)
        .await?
        .ok_or_else(|| sqlx::Error::RowNotFound)
}

pub async fn get_ingest(pool: &SqlitePool, id: &str) -> Result<Option<Ingest>, sqlx::Error> {
    sqlx::query_as::<_, IngestRow>(
        "SELECT id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds FROM ingests WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map(|row| row.map(Into::into))
}

pub async fn get_ingest_by_stream_key(
    pool: &SqlitePool,
    stream_key: &str,
) -> Result<Option<Ingest>, sqlx::Error> {
    sqlx::query_as::<_, IngestRow>(
        "SELECT id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds FROM ingests WHERE stream_key = ? ORDER BY rowid DESC LIMIT 1",
    )
    .bind(stream_key)
    .fetch_optional(pool)
    .await
    .map(|row| row.map(Into::into))
}

pub async fn list_ingests_for_stream_key(
    pool: &SqlitePool,
    stream_key: &str,
) -> Result<Vec<Ingest>, sqlx::Error> {
    sqlx::query_as::<_, IngestRow>(
        "SELECT id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds FROM ingests WHERE stream_key = ? ORDER BY rowid ASC",
    )
    .bind(stream_key)
    .fetch_all(pool)
    .await
    .map(|rows| rows.into_iter().map(Into::into).collect())
}

pub async fn list_ingests(pool: &SqlitePool) -> Result<Vec<Ingest>, sqlx::Error> {
    sqlx::query_as::<_, IngestRow>(
        "SELECT id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds FROM ingests ORDER BY rowid ASC",
    )
    .fetch_all(pool)
    .await
    .map(|rows| rows.into_iter().map(Into::into).collect())
}

pub async fn list_ingests_for_filename(
    pool: &SqlitePool,
    filename: &str,
) -> Result<Vec<Ingest>, sqlx::Error> {
    sqlx::query_as::<_, IngestRow>(
        "SELECT id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds FROM ingests WHERE filename = ?",
    )
    .bind(filename)
    .fetch_all(pool)
    .await
    .map(|rows| rows.into_iter().map(Into::into).collect())
}

#[allow(clippy::too_many_arguments)]
pub async fn update_ingest(
    pool: &SqlitePool,
    id: &str,
    filename: &str,
    stream_key: &str,
    loop_flag: bool,
    start_time: &str,
    live_optimized: bool,
    target_gop_seconds: u32,
) -> Result<Option<Ingest>, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE ingests SET filename = ?, stream_key = ?, loop = ?, start_time = ?, live_optimized = ?, target_gop_seconds = ? WHERE id = ?",
    )
    .bind(filename)
    .bind(stream_key)
    .bind(if loop_flag { 1 } else { 0 })
    .bind(start_time)
    .bind(if live_optimized { 1 } else { 0 })
    .bind(i64::from(target_gop_seconds))
    .bind(id)
    .execute(pool)
    .await?;

    if result.rows_affected() > 0 {
        get_ingest(pool, id).await
    } else {
        Ok(None)
    }
}

pub async fn delete_ingest(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM ingests WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
