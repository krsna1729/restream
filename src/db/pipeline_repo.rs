use crate::application::models::Pipeline;
use sqlx::SqlitePool;

#[derive(sqlx::FromRow)]
struct PipelineRow {
    id: String,
    name: String,
    stream_key: String,
    input_source: Option<String>,
    srt_ingest_policy: Option<String>,
}

impl From<PipelineRow> for Pipeline {
    fn from(row: PipelineRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            stream_key: row.stream_key,
            input_source: row.input_source,
            srt_ingest_policy: row.srt_ingest_policy,
        }
    }
}

pub async fn create_pipeline(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    stream_key: &str,
    input_source: Option<&str>,
    srt_ingest_policy: Option<&str>,
) -> Result<Pipeline, sqlx::Error> {
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
    sqlx::query_as::<_, PipelineRow>(
        "SELECT id, name, stream_key, input_source, srt_ingest_policy FROM pipelines WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
    .map(|row| row.map(Into::into))
}

pub async fn get_pipeline_by_stream_key(
    pool: &SqlitePool,
    stream_key: &str,
) -> Result<Option<Pipeline>, sqlx::Error> {
    sqlx::query_as::<_, PipelineRow>(
        "SELECT id, name, stream_key, input_source, srt_ingest_policy FROM pipelines WHERE stream_key = ?",
    )
    .bind(stream_key)
    .fetch_optional(pool)
    .await
    .map(|row| row.map(Into::into))
}

pub async fn list_pipelines(pool: &SqlitePool) -> Result<Vec<Pipeline>, sqlx::Error> {
    sqlx::query_as::<_, PipelineRow>(
        "SELECT id, name, stream_key, input_source, srt_ingest_policy FROM pipelines",
    )
    .fetch_all(pool)
    .await
    .map(|rows| rows.into_iter().map(Into::into).collect())
}

pub async fn update_pipeline(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    stream_key: &str,
    input_source: Option<&str>,
    srt_ingest_policy: Option<&str>,
) -> Result<Option<Pipeline>, sqlx::Error> {
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
