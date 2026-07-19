use sqlx::SqlitePool;

pub async fn setup_database_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    sqlx::query("PRAGMA journal_mode = WAL;")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA journal_size_limit = 67108864;")
        .execute(pool)
        .await?;
    super::migrations::ensure_schema_migrations_table(pool).await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS pipelines (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            encoding TEXT,
            input_ever_seen_live INTEGER NOT NULL DEFAULT 0 CHECK(input_ever_seen_live IN (0, 1)),
            input_source TEXT,
            srt_ingest_policy TEXT CHECK(srt_ingest_policy IS NULL OR srt_ingest_policy IN ('', 'source', 'encrypted') OR json_valid(srt_ingest_policy))
        );",
    )
    .execute(pool)
    .await?;
    super::migrations::ensure_column_exists(pool, "pipelines", "srt_ingest_policy", "TEXT").await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS outputs (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            monitoring_url TEXT,
            desired_state TEXT NOT NULL DEFAULT 'running' CHECK(desired_state IN ('running', 'stopped', 'failed')),
            config TEXT NOT NULL DEFAULT '{\"video\":{\"mode\":\"source\"},\"audio\":{\"mode\":\"all\"}}' CHECK(json_valid(config)),
            encoding TEXT,
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        );",
    )
    .execute(pool)
    .await?;
    super::migrations::ensure_column_exists(pool, "outputs", "monitoring_url", "TEXT").await?;
    super::migrations::ensure_column_exists(
        pool,
        "outputs",
        "config",
        "TEXT NOT NULL DEFAULT '{\"video\":{\"mode\":\"source\"},\"audio\":{\"mode\":\"all\"}}'",
    )
    .await?;
    super::migrations::backfill_output_configs(pool).await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_outputs_pipeline ON outputs(pipeline_id);")
        .execute(pool)
        .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS pipeline_inputs (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            label TEXT NOT NULL CHECK(length(trim(label)) > 0),
            stream_key TEXT NOT NULL CHECK(length(stream_key) > 0),
            role TEXT NOT NULL CHECK(role IN ('primary', 'backup')),
            enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
            selected INTEGER NOT NULL DEFAULT 0 CHECK(selected IN (0, 1)),
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        );",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_pipeline_inputs_stream_key_unique
         ON pipeline_inputs(stream_key);",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_pipeline_inputs_one_primary
         ON pipeline_inputs(pipeline_id) WHERE role = 'primary';",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_pipeline_inputs_one_selected
         ON pipeline_inputs(pipeline_id) WHERE selected = 1;",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_pipeline_inputs_pipeline
         ON pipeline_inputs(pipeline_id);",
    )
    .execute(pool)
    .await?;
    sqlx::query(
        "CREATE TRIGGER IF NOT EXISTS trg_pipeline_inputs_limit
         BEFORE INSERT ON pipeline_inputs
         WHEN (
             SELECT COUNT(*) FROM pipeline_inputs
             WHERE pipeline_id = NEW.pipeline_id
         ) >= 4
         BEGIN
             SELECT RAISE(ABORT, 'pipeline input limit exceeded');
         END;",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS jobs (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            output_id TEXT NOT NULL,
            pid INTEGER,
            status TEXT NOT NULL CHECK(status IN ('running', 'stopped', 'failed')),
            started_at TEXT,
            ended_at TEXT,
            exit_code INTEGER,
            exit_signal TEXT,
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE,
            FOREIGN KEY(output_id) REFERENCES outputs(id) ON DELETE CASCADE
        );",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_pipeline_output_unique ON jobs(pipeline_id, output_id);",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS ingests (
            id TEXT PRIMARY KEY,
            filename TEXT NOT NULL,
            stream_key TEXT NOT NULL CHECK(length(stream_key) > 0),
            loop INTEGER NOT NULL DEFAULT 0 CHECK(loop IN (0, 1)),
            start_time TEXT NOT NULL DEFAULT '',
            live_optimized INTEGER NOT NULL DEFAULT 0 CHECK(live_optimized IN (0, 1)),
            target_gop_seconds INTEGER NOT NULL DEFAULT 2 CHECK(target_gop_seconds >= 1)
        );",
    )
    .execute(pool)
    .await?;
    super::migrations::ensure_column_exists(
        pool,
        "ingests",
        "live_optimized",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    super::migrations::ensure_column_exists(
        pool,
        "ingests",
        "target_gop_seconds",
        "INTEGER NOT NULL DEFAULT 2",
    )
    .await?;
    sqlx::query(
        "CREATE UNIQUE INDEX IF NOT EXISTS idx_ingests_stream_key_unique ON ingests(stream_key);",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT
        );",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS sessions (
            token TEXT PRIMARY KEY,
            created_at INTEGER NOT NULL
        );",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS app_logs (
            id          INTEGER PRIMARY KEY AUTOINCREMENT,
            ts          TEXT    NOT NULL,
            level       TEXT    NOT NULL,
            target      TEXT    NOT NULL,
            message     TEXT    NOT NULL,
            fields      TEXT,
            pipeline_id TEXT,
            output_id   TEXT,
            event_type  TEXT,
            event_class TEXT
        );",
    )
    .execute(pool)
    .await?;

    sqlx::query("CREATE INDEX IF NOT EXISTS idx_app_logs_ts ON app_logs(ts DESC);")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_app_logs_level ON app_logs(level, ts DESC);")
        .execute(pool)
        .await?;
    sqlx::query("CREATE INDEX IF NOT EXISTS idx_app_logs_target ON app_logs(target, ts DESC);")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_app_logs_pipeline ON app_logs(pipeline_id, ts DESC) WHERE pipeline_id IS NOT NULL;",
    )
    .execute(pool)
    .await?;
    sqlx::query("DROP INDEX IF EXISTS idx_app_logs_history;")
        .execute(pool)
        .await?;
    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_app_logs_scope ON app_logs(pipeline_id, output_id, event_class, ts) WHERE pipeline_id IS NOT NULL;",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS recordings (
            recording_id   TEXT PRIMARY KEY,
            pipeline_id    TEXT NOT NULL,
            started_at     TEXT NOT NULL,
            ended_at       TEXT,
            status         TEXT NOT NULL DEFAULT 'recording' CHECK(status IN ('recording', 'finalizing', 'ready', 'failed')),
            temp_path      TEXT,
            final_path     TEXT,
            codec_summary  TEXT,
            error          TEXT,
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        );",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE INDEX IF NOT EXISTS idx_recordings_pipeline ON recordings(pipeline_id, started_at DESC);",
    )
    .execute(pool)
    .await?;

    super::migrations::record_schema_migration(
        pool,
        super::migrations::CURRENT_SCHEMA_VERSION,
        super::migrations::CURRENT_SCHEMA_MIGRATION_NAME,
    )
    .await?;

    Ok(())
}
