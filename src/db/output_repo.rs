use crate::application::models::Output;
use crate::domain::output_spec::OutputConfig;
use crate::domain::state::DesiredOutputState;
use sqlx::{AssertSqlSafe, FromRow, SqlitePool};

pub(crate) fn serialize_config(config: &OutputConfig) -> Result<String, sqlx::Error> {
    serde_json::to_string(config)
        .map_err(|err| sqlx::Error::Protocol(format!("serialize output config: {err}")))
}

pub(crate) fn deserialize_config(raw: &str) -> Result<OutputConfig, sqlx::Error> {
    serde_json::from_str(raw)
        .map_err(|err| sqlx::Error::Protocol(format!("parse output config json: {err}")))
}

#[derive(FromRow)]
struct OutputRow {
    id: String,
    pipeline_id: String,
    name: String,
    url: String,
    monitoring_url: Option<String>,
    desired_state: String,
    config: String,
}

impl TryFrom<OutputRow> for Output {
    type Error = sqlx::Error;

    fn try_from(row: OutputRow) -> Result<Self, Self::Error> {
        Ok(Self {
            id: row.id,
            pipeline_id: row.pipeline_id,
            name: row.name,
            url: row.url,
            monitoring_url: row.monitoring_url,
            desired_state: DesiredOutputState::from(row.desired_state),
            config: deserialize_config(&row.config)?,
        })
    }
}

async fn fetch_output_optional(
    pool: &SqlitePool,
    query: &str,
    binds: &[&str],
) -> Result<Option<Output>, sqlx::Error> {
    let mut sql = sqlx::query_as::<_, OutputRow>(AssertSqlSafe(query.to_string()));
    for bind in binds {
        sql = sql.bind(*bind);
    }
    sql.fetch_optional(pool)
        .await?
        .map(Output::try_from)
        .transpose()
}

async fn fetch_output_all(
    pool: &SqlitePool,
    query: &str,
    binds: &[&str],
) -> Result<Vec<Output>, sqlx::Error> {
    let mut sql = sqlx::query_as::<_, OutputRow>(AssertSqlSafe(query.to_string()));
    for bind in binds {
        sql = sql.bind(*bind);
    }
    sql.fetch_all(pool)
        .await?
        .into_iter()
        .map(Output::try_from)
        .collect()
}

#[allow(clippy::too_many_arguments)]
pub async fn create_output(
    pool: &SqlitePool,
    id: &str,
    pipeline_id: &str,
    name: &str,
    url: &str,
    monitoring_url: Option<&str>,
    desired_state: DesiredOutputState,
    config: &OutputConfig,
) -> Result<Output, sqlx::Error> {
    let config_json = serialize_config(config)?;
    sqlx::query(
        "INSERT INTO outputs (id, pipeline_id, name, url, monitoring_url, desired_state, config) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(id)
    .bind(pipeline_id)
    .bind(name)
    .bind(url)
    .bind(monitoring_url)
    .bind(desired_state.as_str())
    .bind(config_json)
    .execute(pool)
    .await?;

    get_output(pool, pipeline_id, id)
        .await?
        .ok_or_else(|| sqlx::Error::RowNotFound)
}

pub async fn get_output(
    pool: &SqlitePool,
    pipeline_id: &str,
    id: &str,
) -> Result<Option<Output>, sqlx::Error> {
    fetch_output_optional(
        pool,
        "SELECT id, pipeline_id, name, url, monitoring_url, desired_state, config \
         FROM outputs WHERE id = ? AND pipeline_id = ?",
        &[id, pipeline_id],
    )
    .await
}

pub async fn list_outputs(pool: &SqlitePool) -> Result<Vec<Output>, sqlx::Error> {
    fetch_output_all(
        pool,
        "SELECT id, pipeline_id, name, url, monitoring_url, desired_state, config FROM outputs",
        &[],
    )
    .await
}

pub async fn list_outputs_for_pipeline(
    pool: &SqlitePool,
    pipeline_id: &str,
) -> Result<Vec<Output>, sqlx::Error> {
    fetch_output_all(
        pool,
        "SELECT id, pipeline_id, name, url, monitoring_url, desired_state, config \
         FROM outputs WHERE pipeline_id = ? ORDER BY rowid ASC",
        &[pipeline_id],
    )
    .await
}

pub async fn update_output(
    pool: &SqlitePool,
    pipeline_id: &str,
    id: &str,
    name: &str,
    url: &str,
    monitoring_url: Option<&str>,
    config: &OutputConfig,
) -> Result<Option<Output>, sqlx::Error> {
    let config_json = serialize_config(config)?;
    let result = sqlx::query(
        "UPDATE outputs SET name = ?, url = ?, monitoring_url = ?, config = ? WHERE id = ? AND pipeline_id = ?",
    )
    .bind(name)
    .bind(url)
    .bind(monitoring_url)
    .bind(config_json)
    .bind(id)
    .bind(pipeline_id)
    .execute(pool)
    .await?;

    if result.rows_affected() > 0 {
        get_output(pool, pipeline_id, id).await
    } else {
        Ok(None)
    }
}

pub async fn set_output_desired_state(
    pool: &SqlitePool,
    pipeline_id: &str,
    id: &str,
    desired_state: DesiredOutputState,
) -> Result<Output, sqlx::Error> {
    sqlx::query("UPDATE outputs SET desired_state = ? WHERE id = ? AND pipeline_id = ?")
        .bind(desired_state.as_str())
        .bind(id)
        .bind(pipeline_id)
        .execute(pool)
        .await?;

    get_output(pool, pipeline_id, id)
        .await?
        .ok_or_else(|| sqlx::Error::RowNotFound)
}

pub async fn delete_output(
    pool: &SqlitePool,
    pipeline_id: &str,
    id: &str,
) -> Result<bool, sqlx::Error> {
    let result = sqlx::query("DELETE FROM outputs WHERE id = ? AND pipeline_id = ?")
        .bind(id)
        .bind(pipeline_id)
        .execute(pool)
        .await?;
    Ok(result.rows_affected() > 0)
}
