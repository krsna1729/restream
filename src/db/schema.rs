use sqlx::SqlitePool;

pub async fn setup_database_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    // WAL is also set on every pooled connection via `create_pool`. Keep this
    // schema-time pragma so existing files stay in WAL even if an older
    // process created them before the connect-option change.
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
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        );",
    )
    .execute(pool)
    .await?;
    super::migrations::ensure_column_exists(pool, "outputs", "monitoring_url", "TEXT").await?;
    super::migrations::ensure_column_exists(
        pool,
        "outputs",
        "retries_exhausted",
        "INTEGER NOT NULL DEFAULT 0",
    )
    .await?;
    // Exhausted retries were once stored as desired_state 'failed'. The
    // operator's intent was 'running'; exhaustion is now its own flag.
    sqlx::query(
        "UPDATE outputs SET desired_state = 'running', retries_exhausted = 1 \
         WHERE desired_state = 'failed'",
    )
    .execute(pool)
    .await?;
    super::migrations::ensure_column_exists(
        pool,
        "outputs",
        "config",
        "TEXT NOT NULL DEFAULT '{\"video\":{\"mode\":\"source\"},\"audio\":{\"mode\":\"all\"}}'",
    )
    .await?;

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
            source_kind TEXT NOT NULL DEFAULT 'keyed' CHECK(source_kind IN ('keyed', 'hls_pull')),
            source_url TEXT,
            variant_rule TEXT NOT NULL DEFAULT 'highest_bandwidth' CHECK(variant_rule IN ('highest_bandwidth', 'lowest_bandwidth')),
            source_auth TEXT CHECK(source_auth IS NULL OR (json_valid(source_auth) AND json_extract(source_auth, '$.kind') IN ('none', 'bearer', 'cookies', 'basic', 'headers'))),
            push_auth TEXT CHECK(push_auth IS NULL OR (json_valid(push_auth) AND json_extract(push_auth, '$.kind') IN ('none', 'basic'))),
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        );",
    )
    .execute(pool)
    .await?;
    // HLS pull inputs (Q-068) add a source kind, remote URL, variant rule and
    // write-only auth to rows created before this feature.
    super::migrations::ensure_column_exists(
        pool,
        "pipeline_inputs",
        "source_kind",
        "TEXT NOT NULL DEFAULT 'keyed' CHECK(source_kind IN ('keyed', 'hls_pull'))",
    )
    .await?;
    super::migrations::ensure_column_exists(pool, "pipeline_inputs", "source_url", "TEXT").await?;
    super::migrations::ensure_column_exists(
        pool,
        "pipeline_inputs",
        "variant_rule",
        "TEXT NOT NULL DEFAULT 'highest_bandwidth' CHECK(variant_rule IN ('highest_bandwidth', 'lowest_bandwidth'))",
    )
    .await?;
    super::migrations::ensure_column_exists(
        pool,
        "pipeline_inputs",
        "source_auth",
        "TEXT CHECK(source_auth IS NULL OR (json_valid(source_auth) AND json_extract(source_auth, '$.kind') IN ('none', 'bearer', 'cookies', 'basic', 'headers')))",
    )
    .await?;
    // Existing keyed inputs keep their unauthenticated behavior until configured.
    super::migrations::ensure_column_exists(
        pool,
        "pipeline_inputs",
        "push_auth",
        "TEXT CHECK(push_auth IS NULL OR (json_valid(push_auth) AND json_extract(push_auth, '$.kind') IN ('none', 'basic')))",
    )
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
    super::migrations::ensure_column_exists(pool, "ingests", "playlist", "TEXT").await?;
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
            created_at INTEGER NOT NULL,
            user_id TEXT,
            view_as TEXT
        );",
    )
    .execute(pool)
    .await?;
    // A session without a member is the built-in admin's.
    super::migrations::ensure_column_exists(pool, "sessions", "user_id", "TEXT").await?;
    super::migrations::ensure_column_exists(pool, "sessions", "view_as", "TEXT").await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            role TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            disabled INTEGER NOT NULL DEFAULT 0,
            password_hash TEXT NOT NULL
        );",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS notify_channels (
            id TEXT PRIMARY KEY,
            kind TEXT NOT NULL,
            name TEXT NOT NULL,
            target TEXT NOT NULL,
            secret TEXT,
            min_severity TEXT NOT NULL,
            on_resolved INTEGER NOT NULL,
            enabled INTEGER NOT NULL,
            created_at INTEGER NOT NULL
        );",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS push_subscriptions (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL,
            endpoint TEXT NOT NULL UNIQUE,
            p256dh TEXT NOT NULL,
            auth TEXT NOT NULL,
            min_severity TEXT NOT NULL,
            created_at INTEGER NOT NULL
        );",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS notify_deliveries (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            at INTEGER NOT NULL,
            target_id TEXT NOT NULL,
            target_name TEXT NOT NULL,
            event TEXT NOT NULL,
            alert_id TEXT NOT NULL,
            title TEXT NOT NULL,
            ok INTEGER NOT NULL,
            attempts INTEGER NOT NULL,
            detail TEXT NOT NULL
        );",
    )
    .execute(pool)
    .await?;

    sqlx::query(
        "CREATE TABLE IF NOT EXISTS audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            at INTEGER NOT NULL,
            username TEXT NOT NULL,
            role TEXT NOT NULL,
            method TEXT NOT NULL,
            path TEXT NOT NULL,
            status INTEGER NOT NULL
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
