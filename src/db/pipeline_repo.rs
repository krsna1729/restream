use crate::types::Pipeline;
use sqlx::SqlitePool;

pub async fn create_pipeline(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    stream_key: &str,
    input_source: Option<&str>,
    srt_ingest_policy: Option<&str>,
) -> Result<Pipeline, sqlx::Error> {
    let exists =
        sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM pipelines WHERE stream_key = ?")
            .bind(stream_key)
            .fetch_one(pool)
            .await?;
    if exists > 0 {
        return Err(sqlx::Error::Protocol("duplicate stream key".into()));
    }

    sqlx::query(
        "INSERT INTO pipelines (id, name, stream_key, input_source, srt_ingest_policy) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(name)
    .bind(stream_key)
    .bind(input_source)
    .bind(srt_ingest_policy)
    .execute(pool)
    .await?;

    get_pipeline(pool, id)
        .await?
        .ok_or_else(|| sqlx::Error::RowNotFound)
}

pub async fn get_pipeline(pool: &SqlitePool, id: &str) -> Result<Option<Pipeline>, sqlx::Error> {
    sqlx::query_as::<_, Pipeline>(
        "SELECT id, name, stream_key, input_source, srt_ingest_policy FROM pipelines WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn get_pipeline_by_stream_key(
    pool: &SqlitePool,
    stream_key: &str,
) -> Result<Option<Pipeline>, sqlx::Error> {
    sqlx::query_as::<_, Pipeline>(
        "SELECT id, name, stream_key, input_source, srt_ingest_policy FROM pipelines WHERE stream_key = ?",
    )
    .bind(stream_key)
    .fetch_optional(pool)
    .await
}

pub async fn list_pipelines(pool: &SqlitePool) -> Result<Vec<Pipeline>, sqlx::Error> {
    sqlx::query_as::<_, Pipeline>(
        "SELECT id, name, stream_key, input_source, srt_ingest_policy FROM pipelines",
    )
    .fetch_all(pool)
    .await
}

pub async fn update_pipeline(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    stream_key: &str,
    input_source: Option<&str>,
    srt_ingest_policy: Option<&str>,
) -> Result<Option<Pipeline>, sqlx::Error> {
    let duplicate = sqlx::query_scalar::<_, i64>(
        "SELECT COUNT(*) FROM pipelines WHERE stream_key = ? AND id != ?",
    )
    .bind(stream_key)
    .bind(id)
    .fetch_one(pool)
    .await?;
    if duplicate > 0 {
        return Err(sqlx::Error::Protocol("duplicate stream key".into()));
    }

    let result = sqlx::query(
        "UPDATE pipelines SET name = ?, stream_key = ?, input_source = ?, srt_ingest_policy = ? WHERE id = ?",
    )
    .bind(name)
    .bind(stream_key)
    .bind(input_source)
    .bind(srt_ingest_policy)
    .bind(id)
    .execute(pool)
    .await?;

    if result.rows_affected() > 0 {
        get_pipeline(pool, id).await
    } else {
        Ok(None)
    }
}

pub async fn delete_pipeline(pool: &SqlitePool, id: &str) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM pipelines WHERE id = ?")
        .bind(id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
