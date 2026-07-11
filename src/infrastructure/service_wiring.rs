//! SQLite-backed service constructors.

use std::sync::Arc;

use sqlx::SqlitePool;

use crate::application::services::{
    AgentService, AuthService, FileIngestService, HealthService, IngestService, LogService,
    MediaLibraryService, OutputService, PipelineService, SettingsService,
};
use crate::infrastructure::recording_metadata::spawn_recording_metadata_reporter;
use crate::infrastructure::sqlite_ports::{
    SqliteIngestLookup, SqliteJobStore, SqliteLogStore, SqliteMetaStore, SqliteOutputStore,
    SqlitePipelineStore, SqliteRecordingStore, SqliteSessionStore,
};

impl PipelineService {
    pub fn new(db: SqlitePool) -> Self {
        Self::with_store(Arc::new(SqlitePipelineStore::new(db)))
    }
}

impl OutputService {
    pub fn new(db: SqlitePool) -> Self {
        Self::with_store(Arc::new(SqliteOutputStore::new(db)))
    }
}

impl IngestService {
    pub fn new(db: SqlitePool) -> Self {
        let store = Arc::new(SqliteIngestLookup::new(db));
        Self::with_ports(store.clone(), store)
    }
}

impl AuthService {
    pub fn new(db: SqlitePool) -> Self {
        let meta_store = Arc::new(SqliteMetaStore::new(db.clone()));
        Self::with_stores(
            meta_store.clone(),
            meta_store,
            Arc::new(SqliteSessionStore::new(db)),
        )
    }
}

impl SettingsService {
    pub fn new(db: SqlitePool) -> Self {
        let meta_store = Arc::new(SqliteMetaStore::new(db.clone()));
        Self::with_stores(
            meta_store.clone(),
            meta_store.clone(),
            meta_store,
            Arc::new(SqliteJobStore::new(db.clone())),
            PipelineService::new(db.clone()),
            OutputService::new(db),
        )
    }
}

impl HealthService {
    pub fn new(db: SqlitePool) -> Self {
        Self::with_store(Arc::new(SqlitePipelineStore::new(db)))
    }
}

impl FileIngestService {
    pub fn new(db: SqlitePool, pipeline_service: PipelineService) -> Self {
        let ingest_store = Arc::new(SqliteIngestLookup::new(db.clone()));
        Self::with_ports(
            ingest_store.clone(),
            ingest_store,
            Arc::new(SqlitePipelineStore::new(db)),
            pipeline_service,
        )
    }
}

impl MediaLibraryService {
    pub fn new(
        db: SqlitePool,
        pipeline_service: PipelineService,
        ingest_service: IngestService,
    ) -> Self {
        let meta_store = Arc::new(SqliteMetaStore::new(db.clone()));
        Self::with_stores(
            meta_store.clone(),
            meta_store,
            Arc::new(SqliteRecordingStore::new(db.clone())),
            pipeline_service,
            ingest_service,
        )
        .with_recording_metadata(spawn_recording_metadata_reporter(db))
    }
}

impl LogService {
    pub fn new(db: SqlitePool) -> Self {
        Self::with_store(Arc::new(SqliteLogStore::new(db)))
    }
}

impl AgentService {
    pub fn new(db: SqlitePool) -> Self {
        let meta_store = Arc::new(SqliteMetaStore::new(db.clone()));
        Self::with_stores(
            Arc::new(SqlitePipelineStore::new(db.clone())),
            Arc::new(SqliteOutputStore::new(db.clone())),
            Arc::new(SqliteJobStore::new(db.clone())),
            Arc::new(SqliteIngestLookup::new(db)),
            meta_store.clone(),
            meta_store,
        )
    }
}
