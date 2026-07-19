use crate::domain::pipeline_input::{PipelineInput, PipelineInputRole};
use sqlx::{FromRow, SqlitePool};

#[derive(FromRow)]
struct PipelineInputRow {
    id: String,
    pipeline_id: String,
    label: String,
    stream_key: String,
    role: String,
    enabled: bool,
    selected: bool,
}

impl TryFrom<PipelineInputRow> for PipelineInput {
    type Error = sqlx::Error;

    fn try_from(row: PipelineInputRow) -> Result<Self, Self::Error> {
        let role = PipelineInputRole::try_from(row.role.as_str())
            .map_err(|error| sqlx::Error::Protocol(error.to_string()))?;
        Ok(Self {
            id: row.id,
            pipeline_id: row.pipeline_id,
            label: row.label,
            stream_key: row.stream_key,
            role,
            enabled: row.enabled,
            selected: row.selected,
        })
    }
}

const SELECT_FIELDS: &str = "id, pipeline_id, label, stream_key, role, enabled, selected";

fn primary_input_id(pipeline_id: &str) -> String {
    format!("input_primary_{pipeline_id}")
}

pub(crate) async fn insert_primary_pipeline_input(
    transaction: &mut sqlx::Transaction<'_, sqlx::Sqlite>,
    pipeline_id: &str,
    stream_key: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(
        "INSERT INTO pipeline_inputs
         (id, pipeline_id, label, stream_key, role, enabled, selected)
         VALUES (?, ?, 'Primary', ?, 'primary', 1, 1)",
    )
    .bind(primary_input_id(pipeline_id))
    .bind(pipeline_id)
    .bind(stream_key)
    .execute(&mut **transaction)
    .await?;
    Ok(())
}

fn convert_optional(row: Option<PipelineInputRow>) -> Result<Option<PipelineInput>, sqlx::Error> {
    row.map(PipelineInput::try_from).transpose()
}

pub async fn get_pipeline_input(
    pool: &SqlitePool,
    pipeline_id: &str,
    input_id: &str,
) -> Result<Option<PipelineInput>, sqlx::Error> {
    let query =
        format!("SELECT {SELECT_FIELDS} FROM pipeline_inputs WHERE pipeline_id = ? AND id = ?");
    convert_optional(
        sqlx::query_as::<_, PipelineInputRow>(sqlx::AssertSqlSafe(query))
            .bind(pipeline_id)
            .bind(input_id)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn get_pipeline_input_by_stream_key(
    pool: &SqlitePool,
    stream_key: &str,
) -> Result<Option<PipelineInput>, sqlx::Error> {
    let query = format!("SELECT {SELECT_FIELDS} FROM pipeline_inputs WHERE stream_key = ?");
    convert_optional(
        sqlx::query_as::<_, PipelineInputRow>(sqlx::AssertSqlSafe(query))
            .bind(stream_key)
            .fetch_optional(pool)
            .await?,
    )
}

pub async fn list_pipeline_inputs(
    pool: &SqlitePool,
    pipeline_id: &str,
) -> Result<Vec<PipelineInput>, sqlx::Error> {
    let query = format!(
        "SELECT {SELECT_FIELDS} FROM pipeline_inputs
         WHERE pipeline_id = ? ORDER BY role DESC, rowid ASC"
    );
    sqlx::query_as::<_, PipelineInputRow>(sqlx::AssertSqlSafe(query))
        .bind(pipeline_id)
        .fetch_all(pool)
        .await?
        .into_iter()
        .map(PipelineInput::try_from)
        .collect()
}

pub async fn create_pipeline_input(
    pool: &SqlitePool,
    id: &str,
    pipeline_id: &str,
    label: &str,
    stream_key: &str,
) -> Result<PipelineInput, sqlx::Error> {
    sqlx::query(
        "INSERT INTO pipeline_inputs
         (id, pipeline_id, label, stream_key, role, enabled, selected)
         VALUES (?, ?, ?, ?, 'backup', 1, 0)",
    )
    .bind(id)
    .bind(pipeline_id)
    .bind(label)
    .bind(stream_key)
    .execute(pool)
    .await?;
    get_pipeline_input(pool, pipeline_id, id)
        .await?
        .ok_or(sqlx::Error::RowNotFound)
}

pub async fn promote_pipeline_input(
    pool: &SqlitePool,
    pipeline_id: &str,
    input_id: &str,
) -> Result<Option<PipelineInput>, sqlx::Error> {
    let mut transaction = pool.begin().await?;
    let target_exists: bool = sqlx::query_scalar(
        "SELECT EXISTS(
            SELECT 1 FROM pipeline_inputs
            WHERE pipeline_id = ? AND id = ? AND enabled = 1
        )",
    )
    .bind(pipeline_id)
    .bind(input_id)
    .fetch_one(&mut *transaction)
    .await?;
    if !target_exists {
        return Ok(None);
    }
    sqlx::query(
        "UPDATE pipeline_inputs
         SET selected = CASE WHEN id = ? THEN 1 ELSE 0 END
         WHERE pipeline_id = ?",
    )
    .bind(input_id)
    .bind(pipeline_id)
    .execute(&mut *transaction)
    .await?;
    transaction.commit().await?;
    get_pipeline_input(pool, pipeline_id, input_id).await
}

pub async fn update_pipeline_input(
    pool: &SqlitePool,
    pipeline_id: &str,
    input_id: &str,
    label: &str,
    enabled: bool,
) -> Result<Option<PipelineInput>, sqlx::Error> {
    let result = sqlx::query(
        "UPDATE pipeline_inputs SET label = ?, enabled = ?
         WHERE pipeline_id = ? AND id = ?",
    )
    .bind(label)
    .bind(enabled)
    .bind(pipeline_id)
    .bind(input_id)
    .execute(pool)
    .await?;
    if result.rows_affected() == 0 {
        return Ok(None);
    }
    get_pipeline_input(pool, pipeline_id, input_id).await
}

pub async fn delete_pipeline_input(
    pool: &SqlitePool,
    pipeline_id: &str,
    input_id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM pipeline_inputs WHERE pipeline_id = ? AND id = ?")
        .bind(pipeline_id)
        .bind(input_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
