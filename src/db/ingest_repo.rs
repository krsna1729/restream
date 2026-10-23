use sqlx::SqlitePool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct IngestRecord {
    pub id: String,
    pub filename: String,
    pub stream_key: String,
    #[sqlx(rename = "loop")]
    pub loop_flag: bool,
    pub start_time: String,
    pub live_optimized: bool,
    pub target_gop_seconds: u32,
    /// The ordered files of a playlist (JSON array), first = `filename`;
    /// `None` for a single file.
    pub playlist: Option<String>,
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
) -> Result<IngestRecord, sqlx::Error> {
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

pub async fn get_ingest(pool: &SqlitePool, id: &str) -> Result<Option<IngestRecord>, sqlx::Error> {
    sqlx::query_as::<_, IngestRecord>(
        "SELECT id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds, playlist FROM ingests WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn get_ingest_by_stream_key(
    pool: &SqlitePool,
    stream_key: &str,
) -> Result<Option<IngestRecord>, sqlx::Error> {
    sqlx::query_as::<_, IngestRecord>(
        "SELECT id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds, playlist FROM ingests WHERE stream_key = ? ORDER BY rowid DESC LIMIT 1",
    )
    .bind(stream_key)
    .fetch_optional(pool)
    .await
}

pub async fn list_ingests_for_stream_key(
    pool: &SqlitePool,
    stream_key: &str,
) -> Result<Vec<IngestRecord>, sqlx::Error> {
    sqlx::query_as::<_, IngestRecord>(
        "SELECT id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds, playlist FROM ingests WHERE stream_key = ? ORDER BY rowid ASC",
    )
    .bind(stream_key)
    .fetch_all(pool)
    .await
}

pub async fn list_ingests(pool: &SqlitePool) -> Result<Vec<IngestRecord>, sqlx::Error> {
    sqlx::query_as::<_, IngestRecord>(
        "SELECT id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds, playlist FROM ingests ORDER BY rowid ASC",
    )
    .fetch_all(pool)
    .await
}

pub async fn list_ingests_for_filename(
    pool: &SqlitePool,
    filename: &str,
) -> Result<Vec<IngestRecord>, sqlx::Error> {
    sqlx::query_as::<_, IngestRecord>(
        "SELECT id, filename, stream_key, loop, start_time, live_optimized, target_gop_seconds, playlist FROM ingests
         WHERE filename = ?1 OR EXISTS (SELECT 1 FROM json_each(ingests.playlist) WHERE value = ?1)",
    )
    .bind(filename)
    .fetch_all(pool)
    .await
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
) -> Result<Option<IngestRecord>, sqlx::Error> {
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

/// Updates only `filename`, leaving `stream_key`, `loop_flag`, `start_time`,
/// `live_optimized`, and `target_gop_seconds` untouched. Callers that only
/// intend to rename an ingest's backing file must use this instead of
/// `update_ingest`: that function requires the full row and overwrites every
/// column from whatever `Ingest` snapshot the caller passes in, silently
/// reverting any of those fields if they were changed by a concurrent
/// request after the snapshot was taken.
pub async fn update_ingest_filename(
    pool: &SqlitePool,
    id: &str,
    filename: &str,
) -> Result<Option<IngestRecord>, sqlx::Error> {
    let result = sqlx::query("UPDATE ingests SET filename = ? WHERE id = ?")
        .bind(filename)
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

/// Sets (or, with `None`, clears) an ingest's playlist: a JSON array of
/// filenames whose first entry is its `filename`.
pub async fn set_ingest_playlist(
    pool: &SqlitePool,
    id: &str,
    playlist: Option<&str>,
) -> Result<Option<IngestRecord>, sqlx::Error> {
    let result = sqlx::query("UPDATE ingests SET playlist = ? WHERE id = ?")
        .bind(playlist)
        .bind(id)
        .execute(pool)
        .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_ingest(pool, id).await
}

/// Renames one media file wherever an ingest uses it: its `filename` and
/// every playlist entry, in order.
pub async fn rename_ingest_file(
    pool: &SqlitePool,
    id: &str,
    from: &str,
    to: &str,
) -> Result<Option<IngestRecord>, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE ingests SET
            filename = CASE WHEN filename = ?1 THEN ?2 ELSE filename END,
            playlist = CASE WHEN playlist IS NULL THEN NULL ELSE (
                SELECT json_group_array(item) FROM (
                    SELECT CASE WHEN value = ?1 THEN ?2 ELSE value END AS item
                    FROM json_each(ingests.playlist) ORDER BY key
                )
            ) END
         WHERE id = ?3",
    )
    .bind(from)
    .bind(to)
    .bind(id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_ingest(pool, id).await
}
