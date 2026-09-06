use sqlx::{AssertSqlSafe, FromRow, SqlitePool};
use std::str::FromStr;

#[derive(FromRow)]
struct JobRow {
    id: String,
    pipeline_id: String,
    output_id: String,
    pid: Option<i64>,
    status: String,
    started_at: String,
    ended_at: Option<String>,
    exit_code: Option<i64>,
    exit_signal: Option<String>,
}

#[derive(Debug, Clone)]
pub struct JobRecord {
    pub id: String,
    pub pipeline_id: String,
    pub output_id: String,
    pub pid: Option<i64>,
    pub status: JobStatusRecord,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub exit_code: Option<i64>,
    pub exit_signal: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobStatusRecord {
    Running,
    Stopped,
    Failed,
}

impl JobStatusRecord {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Stopped => "stopped",
            Self::Failed => "failed",
        }
    }
}

impl FromStr for JobStatusRecord {
    type Err = &'static str;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        match value {
            "running" => Ok(Self::Running),
            "stopped" => Ok(Self::Stopped),
            "failed" => Ok(Self::Failed),
            _ => Err("unknown job status"),
        }
    }
}

impl TryFrom<JobRow> for JobRecord {
    type Error = sqlx::Error;

    fn try_from(row: JobRow) -> Result<Self, Self::Error> {
        let status = JobStatusRecord::from_str(&row.status)
            .map_err(|err| sqlx::Error::Protocol(format!("parse job status: {err}")))?;
        Ok(Self {
            id: row.id,
            pipeline_id: row.pipeline_id,
            output_id: row.output_id,
            pid: row.pid,
            status,
            started_at: row.started_at,
            ended_at: row.ended_at,
            exit_code: row.exit_code,
            exit_signal: row.exit_signal,
        })
    }
}

impl JobRecord {
    pub const fn status_typed(&self) -> Option<JobStatusRecord> {
        Some(self.status)
    }
}

async fn fetch_job_optional(
    pool: &SqlitePool,
    query: &str,
    binds: &[&str],
) -> Result<Option<JobRecord>, sqlx::Error> {
    let mut sql = sqlx::query_as::<_, JobRow>(AssertSqlSafe(query.to_string()));
    for bind in binds {
        sql = sql.bind(*bind);
    }
    sql.fetch_optional(pool)
        .await?
        .map(JobRecord::try_from)
        .transpose()
}

async fn fetch_job_all(
    pool: &SqlitePool,
    query: &str,
    binds: &[&str],
) -> Result<Vec<JobRecord>, sqlx::Error> {
    let mut sql = sqlx::query_as::<_, JobRow>(AssertSqlSafe(query.to_string()));
    for bind in binds {
        sql = sql.bind(*bind);
    }
    sql.fetch_all(pool)
        .await?
        .into_iter()
        .map(JobRecord::try_from)
        .collect()
}

pub async fn create_job<S>(
    pool: &SqlitePool,
    id: &str,
    pipeline_id: &str,
    output_id: &str,
    pid: Option<i64>,
    status: S,
    started_at: &str,
) -> Result<JobRecord, sqlx::Error>
where
    S: Into<JobStatusRecord>,
{
    let status = status.into();
    sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, output_id, pid, status, started_at, ended_at, exit_code, exit_signal)
         VALUES (?, ?, ?, ?, ?, ?, NULL, NULL, NULL)
         ON CONFLICT(pipeline_id, output_id) DO UPDATE SET
             id = excluded.id,
             pid = excluded.pid,
             status = excluded.status,
             started_at = excluded.started_at,
             ended_at = NULL,
             exit_code = NULL,
             exit_signal = NULL",
    )
    .bind(id)
    .bind(pipeline_id)
    .bind(output_id)
    .bind(pid)
    .bind(status.as_str())
    .bind(started_at)
    .execute(pool)
    .await?;

    get_job(pool, id)
        .await?
        .ok_or_else(|| sqlx::Error::RowNotFound)
}

pub async fn get_job(pool: &SqlitePool, id: &str) -> Result<Option<JobRecord>, sqlx::Error> {
    fetch_job_optional(
        pool,
        "SELECT id, pipeline_id, output_id, pid, status, started_at, ended_at, exit_code, exit_signal FROM jobs WHERE id = ?",
        &[id],
    )
    .await
}

pub async fn get_running_job_for(
    pool: &SqlitePool,
    pipeline_id: &str,
    output_id: &str,
) -> Result<Option<JobRecord>, sqlx::Error> {
    fetch_job_optional(
        pool,
        "SELECT id, pipeline_id, output_id, pid, status, started_at, ended_at, exit_code, exit_signal
         FROM jobs WHERE pipeline_id = ? AND output_id = ? AND status = ? LIMIT 1",
        &[pipeline_id, output_id, JobStatusRecord::Running.as_str()],
    )
    .await
}

pub async fn update_job<S>(
    pool: &SqlitePool,
    id: &str,
    pid: Option<i64>,
    status: Option<S>,
    ended_at: Option<&str>,
    exit_code: Option<i64>,
    exit_signal: Option<&str>,
) -> Result<Option<JobRecord>, sqlx::Error>
where
    S: Into<JobStatusRecord>,
{
    sqlx::query(
        "UPDATE jobs SET pid = COALESCE(?, pid), status = COALESCE(?, status), ended_at = COALESCE(?, ended_at),
                         exit_code = COALESCE(?, exit_code), exit_signal = COALESCE(?, exit_signal) WHERE id = ?",
    )
    .bind(pid)
    .bind(status.map(Into::into).map(JobStatusRecord::as_str))
    .bind(ended_at)
    .bind(exit_code)
    .bind(exit_signal)
    .bind(id)
    .execute(pool)
    .await?;

    get_job(pool, id).await
}

pub async fn list_jobs_for_output(
    pool: &SqlitePool,
    pipeline_id: &str,
    output_id: &str,
) -> Result<Vec<JobRecord>, sqlx::Error> {
    fetch_job_all(
        pool,
        "SELECT id, pipeline_id, output_id, pid, status, started_at, ended_at, exit_code, exit_signal
         FROM jobs WHERE pipeline_id = ? AND output_id = ? ORDER BY started_at DESC",
        &[pipeline_id, output_id],
    )
    .await
}

pub async fn list_jobs(pool: &SqlitePool) -> Result<Vec<JobRecord>, sqlx::Error> {
    fetch_job_all(
        pool,
        "SELECT id, pipeline_id, output_id, pid, status, started_at, ended_at, exit_code, exit_signal
         FROM jobs ORDER BY started_at DESC, id DESC",
        &[],
    )
    .await
}

pub async fn cleanup_old_jobs(pool: &SqlitePool) -> Result<(u64, u64), sqlx::Error> {
    super::with_busy_retry(|| async {
        let mut tx = pool.begin().await?;

        let result = sqlx::query(
            "DELETE FROM jobs
         WHERE (status IN (?, ?) AND ended_at IS NOT NULL AND datetime(ended_at) < datetime('now', '-7 days'))
            OR datetime(COALESCE(ended_at, started_at)) < datetime('now', '-30 days')",
        )
        .bind(JobStatusRecord::Stopped.as_str())
        .bind(JobStatusRecord::Failed.as_str())
        .execute(&mut *tx)
        .await?;

        tx.commit().await?;
        Ok((result.rows_affected(), 0))
    })
    .await
}

pub async fn reset_running_jobs(pool: &SqlitePool, now_ts: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE jobs SET status = ?, ended_at = ?, exit_code = NULL, exit_signal = 'SIGKILL'
         WHERE status = ?",
    )
    .bind(JobStatusRecord::Stopped.as_str())
    .bind(now_ts)
    .bind(JobStatusRecord::Running.as_str())
    .execute(pool)
    .await?;
    Ok(())
}
