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
    list_ingests_for_filename, list_ingests_for_stream_key, update_ingest,
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
    list_pipelines, update_pipeline,
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

/// Create a connection pool with all per-connection PRAGMAs baked in via
/// `SqliteConnectOptions`. This ensures every pooled connection gets the same
/// tuning, not just the setup connection (M4 fix).
pub async fn create_pool(url: &str) -> Result<sqlx::SqlitePool, sqlx::Error> {
    use sqlx::sqlite::SqliteConnectOptions;
    use std::str::FromStr;

    let opts = SqliteConnectOptions::from_str(url)?
        .pragma("foreign_keys", "ON")
        .pragma("synchronous", "NORMAL")
        .pragma("busy_timeout", "5000")
        .pragma("cache_size", "-16384")
        .pragma("temp_store", "MEMORY")
        .pragma("mmap_size", "134217728");

    sqlx::SqlitePool::connect_with(opts).await
}
