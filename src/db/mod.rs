//! SQLite persistence layer — raw `sqlx` prepared statements against `data.db`.
//! Schema is created via `CREATE TABLE IF NOT EXISTS` at startup, with targeted
//! in-place column backfills for contract migrations. WAL mode is enabled for
//! concurrent reader/writer access.

pub mod ingest_repo;
pub mod job_repo;
pub mod log_repo;
pub(crate) mod meta_repo;
pub(crate) mod migrations;
pub mod output_repo;
pub mod pipeline_repo;
mod schema;
pub(crate) mod session_repo;

pub use ingest_repo::*;
pub use job_repo::*;
pub use log_repo::*;
pub use output_repo::*;
pub use pipeline_repo::*;
pub use schema::*;
pub use session_repo::*;

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
