//! The database schema: one canonical definition per table, created as is on
//! a new database. A database written by an earlier schema version is
//! rebuilt onto these definitions once ([`super::migrations::upgrade`]), so a
//! new and an upgraded database are the same, with no column bolted on later.

use sqlx::SqlitePool;

/// Every table, parents before children: the order they are created and
/// rebuilt in.
pub(crate) const TABLES: &[(&str, &str)] = &[
    (
        "pipelines",
        "CREATE TABLE IF NOT EXISTS pipelines (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            input_source TEXT,
            srt_ingest_policy TEXT CHECK(srt_ingest_policy IS NULL OR srt_ingest_policy IN ('', 'source', 'encrypted') OR json_valid(srt_ingest_policy))
        )",
    ),
    (
        "outputs",
        "CREATE TABLE IF NOT EXISTS outputs (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            name TEXT NOT NULL,
            url TEXT NOT NULL,
            monitoring_url TEXT,
            desired_state TEXT NOT NULL DEFAULT 'running' CHECK(desired_state IN ('running', 'stopped', 'failed')),
            config TEXT NOT NULL DEFAULT '{\"video\":{\"mode\":\"source\"},\"audio\":{\"mode\":\"all\"}}' CHECK(json_valid(config)),
            retries_exhausted INTEGER NOT NULL DEFAULT 0 CHECK(retries_exhausted IN (0, 1)),
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        )",
    ),
    (
        "pipeline_inputs",
        "CREATE TABLE IF NOT EXISTS pipeline_inputs (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            label TEXT NOT NULL CHECK(length(trim(label)) > 0),
            stream_key TEXT NOT NULL CHECK(length(stream_key) > 0),
            role TEXT NOT NULL CHECK(role IN ('primary', 'backup')),
            enabled INTEGER NOT NULL DEFAULT 1 CHECK(enabled IN (0, 1)),
            selected INTEGER NOT NULL DEFAULT 0 CHECK(selected IN (0, 1)),
            source_kind TEXT NOT NULL DEFAULT 'keyed' CHECK(source_kind IN ('keyed', 'hls_pull', 'srt_call')),
            source_url TEXT,
            variant_rule TEXT NOT NULL DEFAULT 'highest_bandwidth' CHECK(variant_rule IN ('highest_bandwidth', 'lowest_bandwidth')),
            source_auth TEXT CHECK(source_auth IS NULL OR (json_valid(source_auth) AND json_extract(source_auth, '$.kind') IN ('none', 'bearer', 'cookies', 'basic', 'headers'))),
            push_auth TEXT CHECK(push_auth IS NULL OR (json_valid(push_auth) AND json_extract(push_auth, '$.kind') IN ('none', 'basic'))),
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        )",
    ),
    (
        "jobs",
        "CREATE TABLE IF NOT EXISTS jobs (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            output_id TEXT NOT NULL,
            status TEXT NOT NULL CHECK(status IN ('running', 'stopped', 'failed')),
            started_at TEXT,
            ended_at TEXT,
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE,
            FOREIGN KEY(output_id) REFERENCES outputs(id) ON DELETE CASCADE
        )",
    ),
    (
        "ingests",
        "CREATE TABLE IF NOT EXISTS ingests (
            id TEXT PRIMARY KEY,
            filename TEXT NOT NULL,
            stream_key TEXT NOT NULL CHECK(length(stream_key) > 0),
            loop INTEGER NOT NULL DEFAULT 0 CHECK(loop IN (0, 1)),
            start_time TEXT NOT NULL DEFAULT '',
            live_optimized INTEGER NOT NULL DEFAULT 0 CHECK(live_optimized IN (0, 1)),
            target_gop_seconds INTEGER NOT NULL DEFAULT 2 CHECK(target_gop_seconds >= 1),
            playlist TEXT
        )",
    ),
    (
        "meta",
        "CREATE TABLE IF NOT EXISTS meta (
            key TEXT PRIMARY KEY,
            value TEXT
        )",
    ),
    (
        "users",
        "CREATE TABLE IF NOT EXISTS users (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL UNIQUE,
            role TEXT NOT NULL,
            created_at INTEGER NOT NULL,
            disabled INTEGER NOT NULL DEFAULT 0,
            password_hash TEXT NOT NULL
        )",
    ),
    // A session without a member is the built-in admin's.
    (
        "sessions",
        "CREATE TABLE IF NOT EXISTS sessions (
            token TEXT PRIMARY KEY,
            created_at INTEGER NOT NULL,
            user_id TEXT,
            view_as TEXT
        )",
    ),
    // The flight recorder's longer history: one sample per pipeline every
    // FLIGHT_PERSIST_EVERY ticks, pruned to RESTREAM_FLIGHT_HISTORY_HOURS. No
    // foreign key: a batch written as its pipeline is deleted must not fail
    // the other pipelines' samples; deletion forgets them explicitly.
    (
        "flight_samples",
        "CREATE TABLE IF NOT EXISTS flight_samples (
            pipeline_id TEXT NOT NULL,
            at_ms INTEGER NOT NULL,
            sample TEXT NOT NULL,
            PRIMARY KEY (pipeline_id, at_ms)
        ) WITHOUT ROWID",
    ),
    (
        "share_links",
        "CREATE TABLE IF NOT EXISTS share_links (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            label TEXT NOT NULL,
            token TEXT NOT NULL UNIQUE,
            created_at INTEGER NOT NULL,
            expires_at INTEGER,
            views INTEGER NOT NULL DEFAULT 0,
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        )",
    ),
    (
        "schedules",
        "CREATE TABLE IF NOT EXISTS schedules (
            id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            name TEXT NOT NULL,
            action TEXT NOT NULL,
            output_ids TEXT NOT NULL DEFAULT '[]',
            at INTEGER NOT NULL,
            repeat TEXT NOT NULL,
            enabled INTEGER NOT NULL,
            last_run INTEGER,
            last_result TEXT,
            created_at INTEGER NOT NULL,
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        )",
    ),
    (
        "notify_channels",
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
        )",
    ),
    (
        "push_subscriptions",
        "CREATE TABLE IF NOT EXISTS push_subscriptions (
            id TEXT PRIMARY KEY,
            username TEXT NOT NULL,
            endpoint TEXT NOT NULL UNIQUE,
            p256dh TEXT NOT NULL,
            auth TEXT NOT NULL,
            min_severity TEXT NOT NULL,
            created_at INTEGER NOT NULL
        )",
    ),
    (
        "notify_deliveries",
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
        )",
    ),
    (
        "audit_log",
        "CREATE TABLE IF NOT EXISTS audit_log (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            at INTEGER NOT NULL,
            username TEXT NOT NULL,
            role TEXT NOT NULL,
            method TEXT NOT NULL,
            path TEXT NOT NULL,
            status INTEGER NOT NULL
        )",
    ),
    (
        "app_logs",
        "CREATE TABLE IF NOT EXISTS app_logs (
            id INTEGER PRIMARY KEY AUTOINCREMENT,
            ts TEXT NOT NULL,
            level TEXT NOT NULL,
            target TEXT NOT NULL,
            message TEXT NOT NULL,
            fields TEXT,
            pipeline_id TEXT,
            output_id TEXT,
            event_type TEXT,
            event_class TEXT
        )",
    ),
    (
        "recordings",
        "CREATE TABLE IF NOT EXISTS recordings (
            recording_id TEXT PRIMARY KEY,
            pipeline_id TEXT NOT NULL,
            started_at TEXT NOT NULL,
            ended_at TEXT,
            status TEXT NOT NULL DEFAULT 'recording' CHECK(status IN ('recording', 'finalizing', 'ready', 'failed')),
            temp_path TEXT,
            final_path TEXT,
            codec_summary TEXT,
            error TEXT,
            FOREIGN KEY(pipeline_id) REFERENCES pipelines(id) ON DELETE CASCADE
        )",
    ),
];

/// Indexes and triggers. A table rebuild drops its own, so these are created
/// after every table exists, on a new and an upgraded database alike.
const INDEXES: &[&str] = &[
    "CREATE INDEX IF NOT EXISTS idx_outputs_pipeline ON outputs(pipeline_id)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_pipeline_inputs_stream_key_unique ON pipeline_inputs(stream_key)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_pipeline_inputs_one_primary ON pipeline_inputs(pipeline_id) WHERE role = 'primary'",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_pipeline_inputs_one_selected ON pipeline_inputs(pipeline_id) WHERE selected = 1",
    "CREATE INDEX IF NOT EXISTS idx_pipeline_inputs_pipeline ON pipeline_inputs(pipeline_id)",
    "CREATE TRIGGER IF NOT EXISTS trg_pipeline_inputs_limit
     BEFORE INSERT ON pipeline_inputs
     WHEN (SELECT COUNT(*) FROM pipeline_inputs WHERE pipeline_id = NEW.pipeline_id) >= 4
     BEGIN
         SELECT RAISE(ABORT, 'pipeline input limit exceeded');
     END",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_jobs_pipeline_output_unique ON jobs(pipeline_id, output_id)",
    "CREATE UNIQUE INDEX IF NOT EXISTS idx_ingests_stream_key_unique ON ingests(stream_key)",
    "CREATE INDEX IF NOT EXISTS flight_samples_at ON flight_samples (at_ms)",
    "CREATE INDEX IF NOT EXISTS idx_share_links_pipeline ON share_links(pipeline_id)",
    "CREATE INDEX IF NOT EXISTS idx_schedules_pipeline ON schedules(pipeline_id)",
    "CREATE INDEX IF NOT EXISTS idx_app_logs_ts ON app_logs(ts DESC)",
    "CREATE INDEX IF NOT EXISTS idx_app_logs_level ON app_logs(level, ts DESC)",
    "CREATE INDEX IF NOT EXISTS idx_app_logs_target ON app_logs(target, ts DESC)",
    "CREATE INDEX IF NOT EXISTS idx_app_logs_pipeline ON app_logs(pipeline_id, ts DESC) WHERE pipeline_id IS NOT NULL",
    "CREATE INDEX IF NOT EXISTS idx_app_logs_scope ON app_logs(pipeline_id, output_id, event_class, ts) WHERE pipeline_id IS NOT NULL",
    "CREATE INDEX IF NOT EXISTS idx_recordings_pipeline ON recordings(pipeline_id, started_at DESC)",
];

pub async fn setup_database_schema(pool: &SqlitePool) -> Result<(), sqlx::Error> {
    // WAL is also set on every pooled connection via `create_pool`; this
    // keeps a file an older process created in WAL too.
    sqlx::query("PRAGMA journal_mode = WAL;")
        .execute(pool)
        .await?;
    sqlx::query("PRAGMA journal_size_limit = 67108864;")
        .execute(pool)
        .await?;
    super::migrations::upgrade(pool).await?;
    for (_, create) in TABLES {
        sqlx::query(*create).execute(pool).await?;
    }
    for create in INDEXES {
        sqlx::query(*create).execute(pool).await?;
    }
    super::migrations::record_current(pool).await
}
