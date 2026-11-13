//! SQLite persistence layer — raw `sqlx` prepared statements against `data.db`.
//! Schema is created via `CREATE TABLE IF NOT EXISTS` at startup, with targeted
//! in-place column backfills for contract migrations. WAL mode is enabled for
//! concurrent reader/writer access.

mod deadline;
pub(crate) mod flight_repo;
mod ingest_repo;
mod job_repo;
pub mod log_repo;
pub(crate) mod meta_repo;
pub(crate) mod migrations;
pub(crate) mod notify_repo;
mod output_repo;
pub mod pipeline_input_repo;
mod pipeline_repo;
pub mod recording_repo;
pub(crate) mod schedule_repo;
mod schema;
pub(crate) mod session_repo;
pub(crate) mod share_repo;
pub(crate) mod team_repo;

pub use deadline::{DeadlineDb, DeadlineError, SHARED_LOOP_DEADLINE};
pub use ingest_repo::{
    IngestRecord, create_ingest, delete_ingest, get_ingest, get_ingest_by_stream_key, list_ingests,
    list_ingests_for_filename, list_ingests_for_stream_key, rename_ingest_file,
    set_ingest_playlist, update_ingest, update_ingest_filename,
};
pub use job_repo::{
    JobRecord, JobStatusRecord, cleanup_old_jobs, create_job, get_job, get_running_job_for,
    list_jobs, reset_running_jobs, update_job,
};
pub use log_repo::{
    APP_LOG_RETENTION_BATCH, append_app_log_batch, append_app_log_batch_returning, list_app_logs,
    prune_app_logs,
};
pub use output_repo::{
    OutputRecord, create_output, delete_output, get_output, list_outputs,
    list_outputs_for_pipeline, mark_output_retries_exhausted, set_output_desired_state,
    update_output,
};
pub use pipeline_input_repo::{
    UndecodableInputRow, create_pipeline_input, delete_pipeline_input, get_pipeline_input,
    get_pipeline_input_by_stream_key, list_all_pipeline_inputs, list_pipeline_input_ids,
    list_pipeline_inputs, list_pipeline_inputs_for_admission, list_pull_inputs,
    list_srt_call_inputs, promote_pipeline_input, update_pipeline_input,
};
pub use pipeline_repo::{
    PipelineRecord, create_pipeline, delete_pipeline, get_pipeline, get_pipeline_by_stream_key,
    list_pipelines, update_pipeline, update_pipeline_input_source,
};
pub use recording_repo::{
    RecordingRow, create_recording, finalize_recording, get_recording, list_recordings,
    update_recording_status,
};
pub use schema::setup_database_schema;
pub use session_repo::{
    create_session, delete_session, delete_sessions_except, get_session_created_at, list_sessions,
    prune_expired_sessions,
};

pub use meta_repo::get_ingest_host;
pub use meta_repo::get_meta;
pub use meta_repo::list_meta_with_prefixes;
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
    sanitize_preexisting_wal(url);
    sqlx::SqlitePool::connect_with(connect_options(url)?).await
}

/// Connections only the shared control loops use (reconciler,
/// housekeeping, egress start-up, scheduler, catalog reload), each through
/// a [`DeadlineDb`]: API handlers holding every connection of the main pool
/// cannot take the capacity these loops need. Opened after [`create_pool`]
/// on the same file.
pub async fn create_lifecycle_pool(url: &str) -> Result<sqlx::SqlitePool, sqlx::Error> {
    sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(LIFECYCLE_POOL_CONNECTIONS)
        .connect_with(connect_options(url)?)
        .await
}

/// One connection per loop that awaits the database (reconciler output
/// pass, housekeeping, scheduler, catalog reload), plus two for the writes
/// that run as their own tasks (egress job rows, retries-exhausted marks).
const LIFECYCLE_POOL_CONNECTIONS: u32 = 6;

fn connect_options(url: &str) -> Result<sqlx::sqlite::SqliteConnectOptions, sqlx::Error> {
    use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqliteSynchronous};
    use std::str::FromStr;

    Ok(SqliteConnectOptions::from_str(url)?
        .create_if_missing(true)
        .foreign_keys(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .busy_timeout(SQLITE_BUSY_TIMEOUT)
        .pragma("cache_size", "-16384")
        .pragma("temp_store", "MEMORY")
        .pragma("mmap_size", "134217728"))
}

pub(crate) fn is_database_locked(err: &sqlx::Error) -> bool {
    match err {
        sqlx::Error::Database(db_err) => {
            db_err.code().as_deref() == Some("5") || db_err.message().contains("database is locked")
        }
        _ => false,
    }
}

/// Defense-in-depth against SQLite WAL page-size mismatch (CVE-2026-119375).
/// If a pre-existing sibling `-wal` file has a page size differing from the
/// database header, remove it before SQLite's pager opens it.
fn sanitize_preexisting_wal(url: &str) {
    let path_str = url
        .strip_prefix("sqlite://")
        .or_else(|| url.strip_prefix("sqlite:"));
    let Some(raw_path) = path_str else { return };
    if raw_path.starts_with(':') || raw_path.contains("mode=memory") {
        return;
    }
    let file_path = raw_path.split('?').next().unwrap_or(raw_path);
    if file_path.is_empty() {
        return;
    }
    let db_path = std::path::Path::new(file_path);
    let mut wal_os = db_path.as_os_str().to_os_string();
    wal_os.push("-wal");
    let wal_path = std::path::PathBuf::from(wal_os);
    if !wal_path.is_file() {
        return;
    }
    let Ok(db_bytes) = std::fs::read(db_path) else {
        return;
    };
    let Ok(wal_bytes) = std::fs::read(&wal_path) else {
        return;
    };
    if db_bytes.len() < 18 || wal_bytes.len() < 12 {
        return;
    }
    let mut db_page_size = u16::from_be_bytes([db_bytes[16], db_bytes[17]]) as u32;
    if db_page_size == 1 {
        db_page_size = 65536;
    }
    let wal_page_size =
        u32::from_be_bytes([wal_bytes[8], wal_bytes[9], wal_bytes[10], wal_bytes[11]]);
    if db_page_size != 0 && wal_page_size != 0 && db_page_size != wal_page_size {
        tracing::warn!(
            db = %db_path.display(),
            db_page_size,
            wal_page_size,
            "Removing mismatched SQLite WAL file before connection (CVE-2026-119375 mitigation)"
        );
        let _ = std::fs::remove_file(&wal_path);
        let mut shm_os = db_path.as_os_str().to_os_string();
        shm_os.push("-shm");
        let _ = std::fs::remove_file(std::path::PathBuf::from(shm_os));
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

    #[test]
    fn mismatched_preexisting_wal_is_removed_before_opening() {
        let dir = std::env::temp_dir().join(format!(
            "restream-wal-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let db_file = dir.join("test.db");
        let wal_file = dir.join("test.db-wal");
        // Write a minimal SQLite header with page size 4096 (bytes 16..18 = 0x1000).
        let mut db_header = vec![0u8; 100];
        db_header[..16].copy_from_slice(b"SQLite format 3\0");
        db_header[16..18].copy_from_slice(&4096u16.to_be_bytes());
        std::fs::write(&db_file, db_header).unwrap();

        // Write a minimal WAL header with mismatched page size 1024 (bytes 8..12 = 0x00000400).
        let mut wal_header = vec![0u8; 32];
        wal_header[..4].copy_from_slice(&0x377f0682u32.to_be_bytes());
        wal_header[8..12].copy_from_slice(&1024u32.to_be_bytes());
        std::fs::write(&wal_file, wal_header).unwrap();

        assert!(wal_file.is_file());
        let url = format!("sqlite://{}", db_file.display());
        sanitize_preexisting_wal(&url);
        assert!(
            !wal_file.exists(),
            "mismatched WAL must be unlinked before SQLite pager can access it"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
