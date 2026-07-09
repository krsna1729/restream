use crate::types::{Job, JobStatus};
use sqlx::{AssertSqlSafe, FromRow, SqlitePool};

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

impl TryFrom<JobRow> for Job {
    type Error = sqlx::Error;

    fn try_from(row: JobRow) -> Result<Self, Self::Error> {
        let status = JobStatus::try_from(row.status.as_str())
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

async fn fetch_job_optional(
    pool: &SqlitePool,
    query: &str,
    binds: &[&str],
) -> Result<Option<Job>, sqlx::Error> {
    let mut sql = sqlx::query_as::<_, JobRow>(AssertSqlSafe(query.to_string()));
    for bind in binds {
        sql = sql.bind(*bind);
    }
    sql.fetch_optional(pool)
        .await?
        .map(Job::try_from)
        .transpose()
}

async fn fetch_job_all(
    pool: &SqlitePool,
    query: &str,
    binds: &[&str],
) -> Result<Vec<Job>, sqlx::Error> {
    let mut sql = sqlx::query_as::<_, JobRow>(AssertSqlSafe(query.to_string()));
    for bind in binds {
        sql = sql.bind(*bind);
    }
    sql.fetch_all(pool)
        .await?
        .into_iter()
        .map(Job::try_from)
        .collect()
}

pub async fn create_job(
    pool: &SqlitePool,
    id: &str,
    pipeline_id: &str,
    output_id: &str,
    pid: Option<i64>,
    status: JobStatus,
    started_at: &str,
) -> Result<Job, sqlx::Error> {
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

pub async fn get_job(pool: &SqlitePool, id: &str) -> Result<Option<Job>, sqlx::Error> {
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
) -> Result<Option<Job>, sqlx::Error> {
    fetch_job_optional(
        pool,
        "SELECT id, pipeline_id, output_id, pid, status, started_at, ended_at, exit_code, exit_signal
         FROM jobs WHERE pipeline_id = ? AND output_id = ? AND status = ? LIMIT 1",
        &[pipeline_id, output_id, JobStatus::Running.as_str()],
    )
    .await
}

pub async fn update_job(
    pool: &SqlitePool,
    id: &str,
    pid: Option<i64>,
    status: Option<JobStatus>,
    ended_at: Option<&str>,
    exit_code: Option<i64>,
    exit_signal: Option<&str>,
) -> Result<Option<Job>, sqlx::Error> {
    sqlx::query(
        "UPDATE jobs SET pid = COALESCE(?, pid), status = COALESCE(?, status), ended_at = COALESCE(?, ended_at),
                         exit_code = COALESCE(?, exit_code), exit_signal = COALESCE(?, exit_signal) WHERE id = ?",
    )
    .bind(pid)
    .bind(status.map(JobStatus::as_str))
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
) -> Result<Vec<Job>, sqlx::Error> {
    fetch_job_all(
        pool,
        "SELECT id, pipeline_id, output_id, pid, status, started_at, ended_at, exit_code, exit_signal
         FROM jobs WHERE pipeline_id = ? AND output_id = ? ORDER BY started_at DESC",
        &[pipeline_id, output_id],
    )
    .await
}

pub async fn list_jobs(pool: &SqlitePool) -> Result<Vec<Job>, sqlx::Error> {
    fetch_job_all(
        pool,
        "SELECT id, pipeline_id, output_id, pid, status, started_at, ended_at, exit_code, exit_signal
         FROM jobs ORDER BY started_at DESC, id DESC",
        &[],
    )
    .await
}

pub async fn cleanup_old_jobs(pool: &SqlitePool) -> Result<(u64, u64), sqlx::Error> {
    let mut tx = pool.begin().await?;

    let result = sqlx::query(
        "DELETE FROM jobs
         WHERE (status IN (?, ?) AND ended_at IS NOT NULL AND datetime(ended_at) < datetime('now', '-7 days'))
            OR datetime(COALESCE(ended_at, started_at)) < datetime('now', '-30 days')",
    )
    .bind(JobStatus::Stopped.as_str())
    .bind(JobStatus::Failed.as_str())
    .execute(&mut *tx)
    .await?;

    tx.commit().await?;
    Ok((result.rows_affected(), 0))
}

pub async fn reset_running_jobs(pool: &SqlitePool, now_ts: &str) -> Result<(), sqlx::Error> {
    sqlx::query(
        "UPDATE jobs SET status = ?, ended_at = ?, exit_code = NULL, exit_signal = 'SIGKILL'
         WHERE status = ?",
    )
    .bind(JobStatus::Stopped.as_str())
    .bind(now_ts)
    .bind(JobStatus::Running.as_str())
    .execute(pool)
    .await?;
    Ok(())
}
