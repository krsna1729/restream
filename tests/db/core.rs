use restream::{
    db::{self, JobStatusRecord},
    domain::output_spec::OutputConfig,
    domain::state::DesiredOutputState,
};

pub(super) async fn test_pool() -> sqlx::SqlitePool {
    let pool = db::create_pool("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&pool).await.unwrap();
    pool
}

#[tokio::test]
async fn pipeline_crud() {
    let pool = test_pool().await;

    let p = db::create_pipeline(&pool, "p1", "Test Pipeline", "key01", None, None)
        .await
        .unwrap();
    assert_eq!(p.id, "p1");
    assert_eq!(p.name, "Test Pipeline");
    assert_eq!(p.stream_key, "key01");
    assert!(p.input_source.is_none());

    let fetched = db::get_pipeline(&pool, "p1").await.unwrap().unwrap();
    assert_eq!(fetched.name, "Test Pipeline");
    let by_stream_key = db::get_pipeline_by_stream_key(&pool, "key01")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(by_stream_key.id, "p1");

    let all = db::list_pipelines(&pool).await.unwrap();
    assert_eq!(all.len(), 1);

    let updated = db::update_pipeline(&pool, "p1", "Renamed", "key02", Some("file.mp4"), None)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.name, "Renamed");
    assert_eq!(updated.stream_key, "key02");
    assert_eq!(updated.input_source.as_deref(), Some("file.mp4"));

    assert!(db::delete_pipeline(&pool, "p1").await.unwrap());
    assert!(db::get_pipeline(&pool, "p1").await.unwrap().is_none());
}

#[tokio::test]
async fn update_nonexistent_pipeline_returns_none() {
    let pool = test_pool().await;
    let result = db::update_pipeline(&pool, "nope", "x", "k", None, None)
        .await
        .unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn pipeline_input_stream_keys_are_unique_in_fresh_schema() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "One", "shared-key", None, None)
        .await
        .unwrap();

    let duplicate = db::create_pipeline(&pool, "p2", "Two", "shared-key", None, None).await;

    assert!(duplicate.is_err());
}

#[tokio::test]
async fn schema_setup_records_current_migration_version_once() {
    let pool = test_pool().await;
    db::setup_database_schema(&pool).await.unwrap();

    let rows: Vec<(i64, String)> =
        sqlx::query_as("SELECT version, name FROM schema_migrations ORDER BY version")
            .fetch_all(&pool)
            .await
            .unwrap();

    assert_eq!(rows, vec![(1, "pipeline_inputs_schema_v1".to_string())]);
}

#[tokio::test]
async fn output_crud() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();

    let o = db::create_output(
        &pool,
        "o1",
        "p1",
        "YouTube",
        "rtmp://yt/live",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();
    assert_eq!(o.id, "o1");
    assert_eq!(o.desired_state, DesiredOutputState::Stopped);

    let fetched = db::get_output(&pool, "p1", "o1").await.unwrap().unwrap();
    assert_eq!(fetched.name, "YouTube");

    let all = db::list_outputs_for_pipeline(&pool, "p1").await.unwrap();
    assert_eq!(all.len(), 1);

    let updated = db::update_output(
        &pool,
        "p1",
        "o1",
        "Twitch",
        "rtmp://tw/live",
        None,
        &OutputConfig::preset("720p"),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(updated.name, "Twitch");
    assert_eq!(updated.config.stage_encoding_label(), "720p");

    let started = db::set_output_desired_state(&pool, "p1", "o1", DesiredOutputState::Running)
        .await
        .unwrap();
    assert_eq!(started.desired_state, DesiredOutputState::Running);

    assert!(db::delete_output(&pool, "p1", "o1").await.unwrap());
    assert!(db::get_output(&pool, "p1", "o1").await.unwrap().is_none());
}

#[tokio::test]
async fn schema_constraints_reject_invalid_runtime_invariants() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();

    let invalid_pipeline_bool = sqlx::query(
        "INSERT INTO pipelines (id, name, input_ever_seen_live)
         VALUES ('p-bool', 'Bad', 2);",
    )
    .execute(&pool)
    .await;
    assert!(invalid_pipeline_bool.is_err());

    let empty_srt_policy = sqlx::query(
        "INSERT INTO pipelines (id, name, srt_ingest_policy)
         VALUES ('p-empty-policy', 'Empty policy', '');",
    )
    .execute(&pool)
    .await;
    assert!(empty_srt_policy.is_ok());

    let legacy_srt_policy = sqlx::query(
        "INSERT INTO pipelines (id, name, srt_ingest_policy)
         VALUES ('p-legacy-policy', 'Legacy policy', 'source');",
    )
    .execute(&pool)
    .await;
    assert!(legacy_srt_policy.is_ok());

    let invalid_srt_policy = sqlx::query(
        "INSERT INTO pipelines (id, name, srt_ingest_policy)
         VALUES ('p-bad-policy', 'Bad policy', 'not-json');",
    )
    .execute(&pool)
    .await;
    assert!(invalid_srt_policy.is_err());

    let invalid_output_state = sqlx::query(
        "INSERT INTO outputs (id, pipeline_id, name, url, desired_state, config)
         VALUES ('o-bad-state', 'p1', 'Bad', 'rtmp://example/live/key', 'paused',
                 '{\"video\":{\"mode\":\"source\"},\"audio\":{\"mode\":\"all\"}}');",
    )
    .execute(&pool)
    .await;
    assert!(invalid_output_state.is_err());

    let invalid_output_config = sqlx::query(
        "INSERT INTO outputs (id, pipeline_id, name, url, desired_state, config)
         VALUES ('o-bad-config', 'p1', 'Bad', 'rtmp://example/live/key', 'running', 'not-json');",
    )
    .execute(&pool)
    .await;
    assert!(invalid_output_config.is_err());

    let invalid_job_status = sqlx::query(
        "INSERT INTO jobs (id, pipeline_id, output_id, status)
         VALUES ('j-bad', 'p1', 'o1', 'retrying');",
    )
    .execute(&pool)
    .await;
    assert!(invalid_job_status.is_err());

    let invalid_ingest_bool = sqlx::query(
        "INSERT INTO ingests (id, filename, stream_key, loop, live_optimized, target_gop_seconds)
         VALUES ('i-bad-bool', 'clip.mp4', 'key-file', 2, 0, 2);",
    )
    .execute(&pool)
    .await;
    assert!(invalid_ingest_bool.is_err());

    let invalid_ingest_gop = sqlx::query(
        "INSERT INTO ingests (id, filename, stream_key, loop, live_optimized, target_gop_seconds)
         VALUES ('i-bad-gop', 'clip.mp4', 'key-file-2', 0, 0, 0);",
    )
    .execute(&pool)
    .await;
    assert!(invalid_ingest_gop.is_err());

    let invalid_recording_status = sqlx::query(
        "INSERT INTO recordings (recording_id, pipeline_id, started_at, status)
         VALUES ('r-bad', 'p1', '2026-01-01T00:00:00Z', 'done');",
    )
    .execute(&pool)
    .await;
    assert!(invalid_recording_status.is_err());
}

#[tokio::test]
async fn cascade_delete_removes_outputs() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "o1",
        "p1",
        "Out",
        "rtmp://x",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    db::delete_pipeline(&pool, "p1").await.unwrap();
    let outputs = db::list_outputs(&pool).await.unwrap();
    assert!(outputs.is_empty());
}

#[tokio::test]
async fn job_lifecycle() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "o1",
        "p1",
        "Out",
        "rtmp://x",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    let job = db::create_job(
        &pool,
        "j1",
        "p1",
        "o1",
        Some(1234),
        JobStatusRecord::Running,
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();
    assert_eq!(job.status_typed(), Some(JobStatusRecord::Running));
    assert_eq!(job.pid, Some(1234));

    let running = db::get_running_job_for(&pool, "p1", "o1").await.unwrap();
    assert!(running.is_some());

    let updated = db::update_job(
        &pool,
        "j1",
        None,
        Some(JobStatusRecord::Stopped),
        Some("2024-01-01T00:01:00Z"),
        Some(0),
        None,
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(updated.status_typed(), Some(JobStatusRecord::Stopped));
    assert_eq!(updated.exit_code, Some(0));

    let no_running = db::get_running_job_for(&pool, "p1", "o1").await.unwrap();
    assert!(no_running.is_none());
}

#[tokio::test]
async fn job_upsert_on_conflict() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "o1",
        "p1",
        "Out",
        "rtmp://x",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    db::create_job(
        &pool,
        "j1",
        "p1",
        "o1",
        Some(100),
        JobStatusRecord::Running,
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();
    let replaced = db::create_job(
        &pool,
        "j2",
        "p1",
        "o1",
        Some(200),
        JobStatusRecord::Running,
        "2024-01-01T01:00:00Z",
    )
    .await
    .unwrap();
    assert_eq!(replaced.id, "j2");
    assert_eq!(replaced.pid, Some(200));

    let all = db::list_jobs(&pool).await.unwrap();
    assert_eq!(all.len(), 1);
}

#[tokio::test]
async fn stale_job_update_cannot_clobber_replacement_attempt() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "o1",
        "p1",
        "Out",
        "rtmp://x",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    db::create_job(
        &pool,
        "j1",
        "p1",
        "o1",
        Some(100),
        JobStatusRecord::Running,
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();

    let replacement = db::create_job(
        &pool,
        "j2",
        "p1",
        "o1",
        Some(200),
        JobStatusRecord::Running,
        "2024-01-01T01:00:00Z",
    )
    .await
    .unwrap();
    assert_eq!(replacement.id, "j2");

    let stale_cleanup = db::update_job(
        &pool,
        "j1",
        None,
        Some(JobStatusRecord::Failed),
        Some("2024-01-01T00:05:00Z"),
        Some(1),
        None,
    )
    .await
    .unwrap();
    assert!(stale_cleanup.is_none());

    let running = db::get_running_job_for(&pool, "p1", "o1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(running.id, "j2");
    assert_eq!(running.status_typed(), Some(JobStatusRecord::Running));
    assert_eq!(running.pid, Some(200));
    assert!(running.ended_at.is_none());
    assert!(running.exit_code.is_none());
}

#[tokio::test]
async fn multiple_stale_job_updates_cannot_clobber_newest_attempt() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "o1",
        "p1",
        "Out",
        "rtmp://x",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    db::create_job(
        &pool,
        "j1",
        "p1",
        "o1",
        Some(100),
        JobStatusRecord::Running,
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();
    db::create_job(
        &pool,
        "j2",
        "p1",
        "o1",
        Some(200),
        JobStatusRecord::Running,
        "2024-01-01T00:10:00Z",
    )
    .await
    .unwrap();
    let newest = db::create_job(
        &pool,
        "j3",
        "p1",
        "o1",
        Some(300),
        JobStatusRecord::Running,
        "2024-01-01T00:20:00Z",
    )
    .await
    .unwrap();
    assert_eq!(newest.id, "j3");

    let stale_j1 = db::update_job(
        &pool,
        "j1",
        None,
        Some(JobStatusRecord::Failed),
        Some("2024-01-01T00:05:00Z"),
        Some(1),
        None,
    )
    .await
    .unwrap();
    let stale_j2 = db::update_job(
        &pool,
        "j2",
        None,
        Some(JobStatusRecord::Failed),
        Some("2024-01-01T00:15:00Z"),
        Some(1),
        None,
    )
    .await
    .unwrap();
    assert!(stale_j1.is_none());
    assert!(stale_j2.is_none());

    let running = db::get_running_job_for(&pool, "p1", "o1")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(running.id, "j3");
    assert_eq!(running.status_typed(), Some(JobStatusRecord::Running));
    assert_eq!(running.pid, Some(300));
    assert_eq!(running.started_at, "2024-01-01T00:20:00Z");
    assert!(running.ended_at.is_none());
    assert!(running.exit_code.is_none());
}

#[tokio::test]
async fn ingest_crud() {
    let pool = test_pool().await;

    let i = db::create_ingest(
        &pool,
        "i1",
        "video.mp4",
        "key01",
        true,
        "00:00:05",
        false,
        2,
    )
    .await
    .unwrap();
    assert_eq!(i.filename, "video.mp4");
    assert!(i.loop_flag);
    assert!(!i.live_optimized);
    assert_eq!(i.target_gop_seconds, 2);

    let all = db::list_ingests(&pool).await.unwrap();
    assert_eq!(all.len(), 1);

    let updated = db::update_ingest(&pool, "i1", "other.mp4", "key02", false, "", true, 4)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(updated.filename, "other.mp4");
    assert!(!updated.loop_flag);
    assert!(updated.live_optimized);
    assert_eq!(updated.target_gop_seconds, 4);

    assert!(db::delete_ingest(&pool, "i1").await.unwrap());
    assert!(db::list_ingests(&pool).await.unwrap().is_empty());
}

#[tokio::test]
async fn file_ingest_stream_key_uniqueness_is_enforced_immediately() {
    let pool = test_pool().await;
    db::create_ingest(&pool, "first", "first.mp4", "same-key", false, "", false, 2)
        .await
        .unwrap();
    let duplicate = db::create_ingest(
        &pool,
        "another",
        "other.mp4",
        "same-key",
        false,
        "",
        false,
        2,
    )
    .await
    .expect_err("stream key should be unique after schema setup");
    assert!(
        duplicate
            .to_string()
            .contains("idx_ingests_stream_key_unique")
            || duplicate
                .to_string()
                .contains("UNIQUE constraint failed: ingests.stream_key"),
        "unexpected duplicate-ingest error: {duplicate}"
    );
}

#[tokio::test]
async fn meta_operations() {
    let pool = test_pool().await;

    assert!(db::get_meta(&pool, "foo").await.unwrap().is_none());

    db::set_meta(&pool, "foo", "bar").await.unwrap();
    assert_eq!(db::get_meta(&pool, "foo").await.unwrap().unwrap(), "bar");

    db::set_meta(&pool, "foo", "baz").await.unwrap();
    assert_eq!(db::get_meta(&pool, "foo").await.unwrap().unwrap(), "baz");
}

#[tokio::test]
async fn session_operations() {
    let pool = test_pool().await;

    db::create_session(&pool, "tok1", 1000).await.unwrap();
    db::create_session(&pool, "tok2", 2000).await.unwrap();

    let sessions = db::list_sessions(&pool).await.unwrap();
    assert_eq!(sessions.len(), 2);

    db::delete_session(&pool, "tok1").await.unwrap();
    let sessions = db::list_sessions(&pool).await.unwrap();
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0], "tok2");
}

#[tokio::test]
async fn reset_running_jobs() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "o1",
        "p1",
        "Out",
        "rtmp://x",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();
    db::create_job(
        &pool,
        "j1",
        "p1",
        "o1",
        Some(999),
        JobStatusRecord::Running,
        "2024-01-01T00:00:00Z",
    )
    .await
    .unwrap();

    db::reset_running_jobs(&pool, "2024-01-01T00:05:00Z")
        .await
        .unwrap();

    let job = db::get_job(&pool, "j1").await.unwrap().unwrap();
    assert_eq!(job.status_typed(), Some(JobStatusRecord::Stopped));
    assert_eq!(job.exit_signal.as_deref(), Some("SIGKILL"));
}

#[tokio::test]
async fn cleanup_old_jobs_removes_only_old_terminal_jobs() {
    let pool = test_pool().await;
    db::create_pipeline(&pool, "p1", "P", "key01", None, None)
        .await
        .unwrap();
    db::create_output(
        &pool,
        "o1",
        "p1",
        "Out",
        "rtmp://x",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();
    db::create_output(
        &pool,
        "o2",
        "p1",
        "Out 2",
        "rtmp://y",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();
    db::create_output(
        &pool,
        "o3",
        "p1",
        "Out 3",
        "rtmp://z",
        None,
        DesiredOutputState::Stopped,
        &OutputConfig::default(),
    )
    .await
    .unwrap();

    db::create_job(
        &pool,
        "old-stopped",
        "p1",
        "o1",
        None,
        JobStatusRecord::Stopped,
        "2000-01-01T00:00:00Z",
    )
    .await
    .unwrap();
    db::create_job(
        &pool,
        "old-failed",
        "p1",
        "o2",
        None,
        JobStatusRecord::Failed,
        "2000-01-02T00:00:00Z",
    )
    .await
    .unwrap();
    db::create_job(
        &pool,
        "keep-running",
        "p1",
        "o3",
        None,
        JobStatusRecord::Running,
        "2999-01-01T00:00:00Z",
    )
    .await
    .unwrap();

    let (deleted_jobs, deleted_logs) = db::cleanup_old_jobs(&pool).await.unwrap();

    assert_eq!(deleted_jobs, 2);
    assert_eq!(deleted_logs, 0);
    assert!(db::get_job(&pool, "old-stopped").await.unwrap().is_none());
    assert!(db::get_job(&pool, "old-failed").await.unwrap().is_none());

    let running = db::get_job(&pool, "keep-running").await.unwrap().unwrap();
    assert_eq!(running.status_typed(), Some(JobStatusRecord::Running));
}

// ── Regression tests for Round 10 audit fixes ────────────────────────────────

// M1: list_sessions must return Err (not Ok([])) when the DB fails. The
// reconciler's session-prune logic skips retain() on Err — if this returned
// Ok([]) instead, every active session would be wiped from memory.
#[tokio::test]
async fn list_sessions_returns_err_not_empty_on_db_failure() {
    let pool = db::create_pool("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&pool).await.unwrap();

    // Insert a live session so Ok([]) vs Err is distinguishable.
    let _ = sqlx::query("INSERT INTO sessions (token, created_at) VALUES ('tok1', 0)")
        .execute(&pool)
        .await
        .unwrap();

    // Close the pool to simulate a DB failure.
    pool.close().await;

    let result = db::list_sessions(&pool).await;
    assert!(
        result.is_err(),
        "closed pool must return Err, not Ok([]) — \
         Ok([]) would wipe all active sessions from memory"
    );
}

// M4: Per-connection PRAGMAs — every pooled connection must have busy_timeout
// set so SQLITE_BUSY retries rather than failing immediately. Verify via the
// PRAGMA value read back from the pool (not just the setup connection).
#[tokio::test]
async fn pool_connections_have_busy_timeout_set() {
    let pool = db::create_pool("sqlite::memory:").await.unwrap();
    db::setup_database_schema(&pool).await.unwrap();

    // Acquire two distinct connections and check both have busy_timeout.
    let conn1 = sqlx::query_scalar::<_, i64>("PRAGMA busy_timeout")
        .fetch_one(&pool)
        .await
        .unwrap();
    let conn2 = sqlx::query_scalar::<_, i64>("PRAGMA busy_timeout")
        .fetch_one(&pool)
        .await
        .unwrap();

    assert_eq!(
        conn1, 5000,
        "busy_timeout must be 5000ms on every connection"
    );
    assert_eq!(
        conn2, 5000,
        "busy_timeout must be 5000ms on every connection"
    );
}

// M5: NULL legacy encoding in DB must not cause a decode failure. A row with
// encoding=NULL falls back to the `config` column's default (source).
