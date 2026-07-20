use sqlx::SqlitePool;

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PipelineRecord {
    pub id: String,
    pub name: String,
    pub stream_key: String,
    pub input_source: Option<String>,
    pub srt_ingest_policy: Option<String>,
}

pub async fn create_pipeline(
    pool: &SqlitePool,
    id: &str,
    name: &str,
    stream_key: &str,
    input_source: Option<&str>,
    srt_ingest_policy: Option<&str>,
) -> Result<PipelineRecord, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    sqlx::query(
        "INSERT INTO pipelines (id, name, input_source, srt_ingest_policy) VALUES (?, ?, ?, ?)",
    )
    .bind(id)
    .bind(name)
    .bind(input_source)
    .bind(srt_ingest_policy)
    .execute(&mut *transaction)
    .await?;
    super::pipeline_input_repo::insert_primary_pipeline_input(&mut transaction, id, stream_key)
        .await?;
    transaction.commit().await?;

    get_pipeline(pool, id)
        .await?
        .ok_or_else(|| sqlx::Error::RowNotFound)
}

pub async fn get_pipeline(
    pool: &SqlitePool,
    id: &str,
) -> Result<Option<PipelineRecord>, sqlx::Error> {
    sqlx::query_as::<_, PipelineRecord>(
        "SELECT p.id, p.name, i.stream_key, p.input_source, p.srt_ingest_policy
         FROM pipelines p
         JOIN pipeline_inputs i ON i.pipeline_id = p.id AND i.selected = 1
         WHERE p.id = ?",
    )
    .bind(id)
    .fetch_optional(pool)
    .await
}

pub async fn get_pipeline_by_stream_key(
    pool: &SqlitePool,
    stream_key: &str,
) -> Result<Option<PipelineRecord>, sqlx::Error> {
    sqlx::query_as::<_, PipelineRecord>(
        "SELECT p.id, p.name, i.stream_key, p.input_source, p.srt_ingest_policy
         FROM pipelines p
         JOIN pipeline_inputs i ON i.pipeline_id = p.id
         WHERE i.stream_key = ? AND i.enabled = 1",
    )
    .bind(stream_key)
    .fetch_optional(pool)
    .await
}

pub async fn list_pipelines(pool: &SqlitePool) -> Result<Vec<PipelineRecord>, sqlx::Error> {
    sqlx::query_as::<_, PipelineRecord>(
        "SELECT p.id, p.name, i.stream_key, p.input_source, p.srt_ingest_policy
         FROM pipelines p
         JOIN pipeline_inputs i ON i.pipeline_id = p.id AND i.selected = 1",
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
) -> Result<Option<PipelineRecord>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let result = sqlx::query(
        "UPDATE pipelines SET name = ?, input_source = ?, srt_ingest_policy = ? WHERE id = ?",
    )
    .bind(name)
    .bind(input_source)
    .bind(srt_ingest_policy)
    .bind(id)
    .execute(&mut *transaction)
    .await?;

    if result.rows_affected() > 0 {
        sqlx::query(
            "UPDATE pipeline_inputs SET stream_key = ?
             WHERE pipeline_id = ? AND selected = 1",
        )
        .bind(stream_key)
        .bind(id)
        .execute(&mut *transaction)
        .await?;
        transaction.commit().await?;
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
