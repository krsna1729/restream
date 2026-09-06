//! SQLite persistence layer — raw `sqlx` prepared statements against `data.db`.
//! Schema is created via `CREATE TABLE IF NOT EXISTS` at startup, with targeted
//! in-place column backfills for contract migrations. WAL mode is enabled for
//! concurrent reader/writer access.

mod ingest_repo;
mod job_repo;
pub mod log_repo;
pub(crate) mod meta_repo;
pub(crate) mod migrations;
mod output_repo;
pub mod pipeline_input_repo;
mod pipeline_repo;
pub mod recording_repo;
mod schema;
pub(crate) mod session_repo;

pub use ingest_repo::{
    IngestRecord, create_ingest, delete_ingest, get_ingest, get_ingest_by_stream_key, list_ingests,
    list_ingests_for_filename, list_ingests_for_stream_key, update_ingest, update_ingest_filename,
};
pub use job_repo::{
    JobRecord, JobStatusRecord, cleanup_old_jobs, create_job, get_job, get_running_job_for,
    list_jobs, list_jobs_for_output, reset_running_jobs, update_job,
};
pub use log_repo::{
    append_app_log_batch, append_app_log_batch_returning, delete_app_logs_older_than, list_app_logs,
};
pub use output_repo::{
    OutputRecord, create_output, delete_output, get_output, list_outputs,
    list_outputs_for_pipeline, set_output_desired_state, update_output,
};
pub use pipeline_input_repo::{
    create_pipeline_input, delete_pipeline_input, get_pipeline_input,
    get_pipeline_input_by_stream_key, list_pipeline_inputs, promote_pipeline_input,
    update_pipeline_input,
};
pub use pipeline_repo::{
    PipelineRecord, create_pipeline, delete_pipeline, get_pipeline, get_pipeline_by_stream_key,
    list_pipelines, update_pipeline, update_pipeline_input_source,
};
pub use recording_repo::{
    RecordingRow, create_recording, delete_recording, finalize_recording, get_recording,
    list_recordings, list_recordings_by_status, list_recordings_for_pipeline,
    update_recording_status,
};
pub use schema::setup_database_schema;
pub use session_repo::{
    create_session, delete_session, delete_sessions_except, get_session_created_at, list_sessions,
    prune_expired_sessions,
};

pub use meta_repo::get_ingest_host;
pub use meta_repo::get_meta;
pub use meta_repo::set_ingest_host;
pub use meta_repo::set_meta;

use std::future::Future;
use std::time::Duration;

/// How long each pooled connection waits on `SQLITE_BUSY` before failing.
///
/// `create_pool` already set `PRAGMA busy_timeout = 5000` (sqlx's default).
/// Hosted `--no-netns` concurrency CI still returned code 5 after that wait
/// when the API, reconciler, and app-log drain shared one file-backed DB
/// on a starved runner. 30s is the same connect-option, not a new mechanism.
pub const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

const BUSY_RETRY_ATTEMPTS: u32 = 8;
const BUSY_RETRY_INITIAL_DELAY: Duration = Duration::from_millis(5);
const BUSY_RETRY_MAX_DELAY: Duration = Duration::from_millis(100);

/// Create a connection pool with WAL, busy-wait, and the rest of the
/// per-connection PRAGMAs baked in via `SqliteConnectOptions`.
///
/// Production `run_app` is the only non-test pool opener and already used
/// this function. The remaining gap was WAL: schema setup ran
/// `PRAGMA journal_mode = WAL` on one checkout, while sqlx does not set a
/// journal mode on connect. Typed `journal_mode(Wal)` makes WAL stick for
/// every pooled connection, including the first.
pub async fn create_pool(url: &str) -> Result<sqlx::SqlitePool, sqlx::Error> {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
    use std::str::FromStr;

    let opts = SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .pragma("cache_size", "-16384")
        .pragma("temp_store", "MEMORY")
        .pragma("mmap_size", "134217728");

    sqlx::SqlitePool::connect_with(opts).await
}

pub(crate) fn is_database_locked(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => {
            db_err.code().as_deref() == Some("5") || db_err.message().contains("database is locked")
        }
        _ => false,
    }
}

/// Retry a DB operation that lost the SQLITE_BUSY race after the
/// connection-level busy timeout (typically a deferred-transaction
/// upgrade deadlock, which SQLite returns immediately).
pub(crate) async fn with_busy_retry<T, F, Fut>(mut op: F) -> Result<T, sqlx::Error>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, sqlx::Error>>,
{
    let mut delay = BUSY_RETRY_INITIAL_DELAY;
    for attempt in 0..BUSY_RETRY_ATTEMPTS {
        match op().await {
            Err(err) if is_database_locked(&err) && attempt + 1 < BUSY_RETRY_ATTEMPTS => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(BUSY_RETRY_MAX_DELAY);
            }
            other => return other,
        }
    }
    unreachable!("busy retry loop always returns on the last attempt")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_database_errors_are_not_treated_as_locked() {
        assert!(!is_database_locked(&sqlx::Error::RowNotFound));
        assert!(!is_database_locked(&sqlx::Error::Protocol(
            "unrelated failure".into()
        )));
    }

    #[tokio::test]
    async fn busy_retry_returns_first_success_without_extra_attempts() {
        let mut calls = 0;
        let result = with_busy_retry(|| {
            calls += 1;
            async { Ok::<_, sqlx::Error>(7) }
        })
        .await
        .unwrap();
        assert_eq!(result, 7);
        assert_eq!(calls, 1);
    }
}
