use super::*;
use crate::db::{create_pool, setup_database_schema};

/// A file database (an in-memory one is per connection, and the rebuild
/// runs on its own connection).
async fn file_pool(tag: &str) -> (SqlitePool, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!(
        "restream-schema-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    let url = format!("sqlite://{}", dir.join("db.sqlite").display());
    (create_pool(&url).await.unwrap(), dir)
}

async fn exec(pool: &SqlitePool, sql: &str) {
    sqlx::query(AssertSqlSafe(sql.to_string()))
        .execute(pool)
        .await
        .unwrap();
}

async fn column_names(pool: &SqlitePool, table: &str) -> Vec<String> {
    let mut conn = pool.acquire().await.unwrap();
    columns(&mut conn, table).await.unwrap()
}

async fn count(pool: &SqlitePool, sql: &str) -> i64 {
    sqlx::query_scalar(AssertSqlSafe(sql.to_string()))
        .fetch_one(pool)
        .await
        .unwrap()
}

/// The tables a schema-v2 database had, as the previous release created
/// them, holding a little of everything the rebuild must carry or shed.
async fn v2_database(pool: &SqlitePool) {
    for sql in [
        "CREATE TABLE schema_migrations (version INTEGER PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL DEFAULT (datetime('now')))",
        "INSERT INTO schema_migrations (version, name) VALUES (2, 'pipeline_inputs_hls_pull_v2')",
        "CREATE TABLE pipelines (id TEXT PRIMARY KEY, name TEXT NOT NULL, encoding TEXT, input_ever_seen_live INTEGER NOT NULL DEFAULT 0, input_source TEXT, srt_ingest_policy TEXT)",
        "CREATE TABLE outputs (id TEXT PRIMARY KEY, pipeline_id TEXT NOT NULL, name TEXT NOT NULL, url TEXT NOT NULL, monitoring_url TEXT, desired_state TEXT NOT NULL DEFAULT 'running', config TEXT NOT NULL DEFAULT '{}', retries_exhausted INTEGER NOT NULL DEFAULT 0, FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE)",
        "CREATE TABLE pipeline_inputs (id TEXT PRIMARY KEY, pipeline_id TEXT NOT NULL, label TEXT NOT NULL, stream_key TEXT NOT NULL, role TEXT NOT NULL, enabled INTEGER NOT NULL DEFAULT 1, selected INTEGER NOT NULL DEFAULT 0, source_kind TEXT NOT NULL DEFAULT 'keyed' CHECK(source_kind IN ('keyed', 'hls_pull')), source_url TEXT, variant_rule TEXT NOT NULL DEFAULT 'highest_bandwidth', source_auth TEXT, push_auth TEXT, FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE)",
        "CREATE TABLE jobs (id TEXT PRIMARY KEY, pipeline_id TEXT NOT NULL, output_id TEXT NOT NULL, pid INTEGER, status TEXT NOT NULL, started_at TEXT, ended_at TEXT, exit_code INTEGER, exit_signal TEXT, FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE, FOREIGN KEY(output_id) REFERENCES outputs(id) ON DELETE CASCADE)",
        "CREATE TABLE schedules (id TEXT PRIMARY KEY, pipeline_id TEXT NOT NULL, name TEXT NOT NULL, action TEXT NOT NULL, output_ids TEXT NOT NULL DEFAULT '[]', at INTEGER NOT NULL, repeat TEXT NOT NULL, enabled INTEGER NOT NULL, last_run INTEGER, last_result TEXT, created_at INTEGER NOT NULL)",
        "CREATE TABLE share_links (id TEXT PRIMARY KEY, pipeline_id TEXT NOT NULL, label TEXT NOT NULL, token TEXT NOT NULL UNIQUE, created_at INTEGER NOT NULL, expires_at INTEGER, views INTEGER NOT NULL DEFAULT 0)",
        "INSERT INTO pipelines (id, name, encoding, input_ever_seen_live) VALUES ('p1', 'Main', 'h264', 1)",
        "INSERT INTO outputs (id, pipeline_id, name, url, retries_exhausted) VALUES ('o1', 'p1', 'YouTube', 'rtmp://a/b', 1)",
        "INSERT INTO pipeline_inputs (id, pipeline_id, label, stream_key, role, selected, source_kind, source_url) VALUES ('in-pull', 'p1', 'Pull', 'sk-pull', 'primary', 1, 'hls_pull', 'http://origin/master.m3u8')",
        "INSERT INTO jobs (id, pipeline_id, output_id, pid, status, started_at, exit_signal) VALUES ('j1', 'p1', 'o1', 42, 'running', '2026-10-01T00:00:00Z', 'SIGKILL')",
        "INSERT INTO schedules (id, pipeline_id, name, action, at, repeat, enabled, created_at) VALUES ('s1', 'p1', 'Kept', 'start_outputs', 1, 'none', 1, 1)",
        // A schedule of a pipeline deleted before schedules had a foreign key.
        "INSERT INTO schedules (id, pipeline_id, name, action, at, repeat, enabled, created_at) VALUES ('s-orphan', 'gone', 'Orphan', 'start_outputs', 1, 'none', 1, 1)",
        "INSERT INTO share_links (id, pipeline_id, label, token, created_at) VALUES ('l1', 'p1', 'Lobby', 'tok', 1)",
    ] {
        exec(pool, sql).await;
    }
}

#[tokio::test]
async fn a_v2_database_is_rebuilt_onto_the_canonical_tables_keeping_its_rows() {
    let (pool, dir) = file_pool("v2").await;
    v2_database(&pool).await;
    setup_database_schema(&pool).await.unwrap();

    // Dead columns are gone, every live one is carried.
    let pipeline_columns = column_names(&pool, "pipelines").await;
    assert!(!pipeline_columns.contains(&"encoding".to_string()));
    assert!(!pipeline_columns.contains(&"input_ever_seen_live".to_string()));
    let job_columns = column_names(&pool, "jobs").await;
    for dead in ["pid", "exit_code", "exit_signal"] {
        assert!(!job_columns.contains(&dead.to_string()), "{dead}");
    }
    assert_eq!(
        count(
            &pool,
            "SELECT retries_exhausted FROM outputs WHERE id = 'o1'"
        )
        .await,
        1
    );
    assert_eq!(
        count(&pool, "SELECT COUNT(*) FROM jobs WHERE status = 'running'").await,
        1
    );
    assert_eq!(
        count(
            &pool,
            "SELECT COUNT(*) FROM pipeline_inputs WHERE source_kind = 'hls_pull' AND selected = 1"
        )
        .await,
        1
    );
    // Tables the old database lacked are created; the orphan is removed.
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM recordings").await, 0);
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM schedules").await, 1);
    // The widened CHECK, the rebuilt unique index and the new foreign keys.
    exec(&pool, "INSERT INTO pipeline_inputs (id, pipeline_id, label, stream_key, role, source_kind, source_url) VALUES ('in-call', 'p1', 'Call', 'sk-call', 'backup', 'srt_call', 'srt://e:9000')").await;
    assert!(
        sqlx::query("INSERT INTO pipeline_inputs (id, pipeline_id, label, stream_key, role) VALUES ('dup', 'p1', 'Dup', 'sk-call', 'backup')")
            .execute(&pool)
            .await
            .is_err(),
        "stream keys stay unique"
    );
    exec(&pool, "DELETE FROM pipelines WHERE id = 'p1'").await;
    for table in [
        "outputs",
        "jobs",
        "pipeline_inputs",
        "schedules",
        "share_links",
    ] {
        assert_eq!(
            count(&pool, &format!("SELECT COUNT(*) FROM {table}")).await,
            0,
            "{table} follows its pipeline"
        );
    }
    assert_eq!(
        count(&pool, "SELECT MAX(version) FROM schema_migrations").await,
        CURRENT_SCHEMA_VERSION
    );
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

/// A current database is not rebuilt again: a row the rebuild would drop
/// (an orphan) survives a second setup, because no rebuild runs.
#[tokio::test]
async fn a_current_database_is_left_as_is() {
    let (pool, dir) = file_pool("current").await;
    setup_database_schema(&pool).await.unwrap();
    exec(&pool, "PRAGMA foreign_keys = OFF").await;
    let mut conn = pool.acquire().await.unwrap();
    sqlx::query("PRAGMA foreign_keys = OFF")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("INSERT INTO schedules (id, pipeline_id, name, action, at, repeat, enabled, created_at) VALUES ('s', 'gone', 'S', 'start_outputs', 1, 'none', 1, 1)")
        .execute(&mut *conn)
        .await
        .unwrap();
    sqlx::query("PRAGMA foreign_keys = ON")
        .execute(&mut *conn)
        .await
        .unwrap();
    drop(conn);
    setup_database_schema(&pool).await.unwrap();
    assert_eq!(count(&pool, "SELECT COUNT(*) FROM schedules").await, 1);
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_database_from_a_newer_build_is_refused() {
    let (pool, dir) = file_pool("newer").await;
    setup_database_schema(&pool).await.unwrap();
    exec(
        &pool,
        "INSERT INTO schema_migrations (version, name) VALUES (99, 'future')",
    )
    .await;
    let error = setup_database_schema(&pool).await.unwrap_err().to_string();
    assert!(error.contains("schema version 99"), "{error}");
    pool.close().await;
    let _ = std::fs::remove_dir_all(&dir);
}
