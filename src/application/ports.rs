//! Application-layer port traits and adapter types defining the storage and
//! catalog capabilities that orchestration code depends on.

use crate::domain::output_spec::OutputConfig;
use crate::domain::state::DesiredOutputState;
use crate::logging::types::{AppLogFilters, AppLogRow};
use crate::types::{Ingest, Job, Output, Pipeline};
use sqlx::SqlitePool;
use std::fmt;
use std::future::Future;
use std::pin::Pin;

pub type PipelineLookupFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Pipeline>, PipelineStoreError>> + Send + 'a>>;
pub type PipelineListFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<Pipeline>, PipelineStoreError>> + Send + 'a>>;
pub type PipelineCreateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Pipeline, PipelineStoreError>> + Send + 'a>>;
pub type PipelineDeleteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<bool, PipelineStoreError>> + Send + 'a>>;
pub type PipelineIngestHostFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<String>, PipelineStoreError>> + Send + 'a>>;
pub type IngestLookupFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Ingest>, IngestLookupError>> + Send + 'a>>;
pub type IngestCatalogFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<Ingest>, IngestLookupError>> + Send + 'a>>;
pub type IngestWriteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Ingest, IngestWriteError>> + Send + 'a>>;
pub type IngestUpdateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Ingest>, IngestWriteError>> + Send + 'a>>;
pub type IngestDeleteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<bool, IngestWriteError>> + Send + 'a>>;
pub type MetaLookupFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<String>, MetaLookupError>> + Send + 'a>>;
pub type MetaWriteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<String, MetaLookupError>> + Send + 'a>>;
pub type SessionWriteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<(), SessionStoreError>> + Send + 'a>>;
pub type SessionListFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<String>, SessionStoreError>> + Send + 'a>>;
pub type JobListFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<Job>, JobStoreError>> + Send + 'a>>;
pub type PipelineUpdateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Pipeline>, PipelineStoreError>> + Send + 'a>>;
pub type OutputLookupFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Output>, OutputStoreError>> + Send + 'a>>;
pub type OutputListFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<Output>, OutputStoreError>> + Send + 'a>>;
pub type OutputCreateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Output, OutputStoreError>> + Send + 'a>>;
pub type OutputUpdateFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<Output>, OutputStoreError>> + Send + 'a>>;
pub type OutputDeleteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<bool, OutputStoreError>> + Send + 'a>>;
pub type LogListFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<AppLogRow>, LogStoreError>> + Send + 'a>>;

#[derive(Debug, Clone)]
pub struct PipelineStoreError {
    message: String,
}

impl PipelineStoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for PipelineStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for PipelineStoreError {}

#[derive(Debug, Clone)]
pub struct OutputStoreError {
    message: String,
}

impl OutputStoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for OutputStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for OutputStoreError {}

#[derive(Debug, Clone)]
pub struct LogStoreError {
    message: String,
}

impl LogStoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for LogStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for LogStoreError {}

#[derive(Debug, Clone)]
pub struct IngestLookupError {
    message: String,
}

impl IngestLookupError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for IngestLookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for IngestLookupError {}

#[derive(Debug, Clone)]
pub struct IngestWriteError {
    message: String,
}

impl IngestWriteError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for IngestWriteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for IngestWriteError {}

#[derive(Debug, Clone)]
pub struct MetaLookupError {
    message: String,
}

impl MetaLookupError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for MetaLookupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for MetaLookupError {}

#[derive(Debug, Clone)]
pub struct SessionStoreError {
    message: String,
}

impl SessionStoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for SessionStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for SessionStoreError {}

#[derive(Debug, Clone)]
pub struct JobStoreError {
    message: String,
}

impl JobStoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for JobStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for JobStoreError {}

pub trait PipelineStore: Send + Sync {
    fn get_pipeline<'a>(&'a self, id: &'a str) -> PipelineLookupFuture<'a>;
    fn get_pipeline_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineLookupFuture<'a>;
    fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a>;
    fn create_pipeline<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        stream_key: &'a str,
        input_source: Option<&'a str>,
        srt_ingest_policy: Option<&'a str>,
    ) -> PipelineCreateFuture<'a>;
    fn update_pipeline<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        stream_key: &'a str,
        input_source: Option<&'a str>,
        srt_ingest_policy: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a>;
    fn delete_pipeline<'a>(&'a self, id: &'a str) -> PipelineDeleteFuture<'a>;
    fn get_ingest_host<'a>(&'a self) -> PipelineIngestHostFuture<'a>;
    fn update_pipeline_input_source<'a>(
        &'a self,
        pipeline: &'a Pipeline,
        input_source: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a>;
}

pub trait OutputStore: Send + Sync {
    fn list_outputs<'a>(&'a self) -> OutputListFuture<'a>;
    fn list_outputs_for_pipeline<'a>(&'a self, pipeline_id: &'a str) -> OutputListFuture<'a>;
    fn get_output<'a>(&'a self, pipeline_id: &'a str, id: &'a str) -> OutputLookupFuture<'a>;
    #[allow(clippy::too_many_arguments)]
    fn create_output<'a>(
        &'a self,
        id: &'a str,
        pipeline_id: &'a str,
        name: &'a str,
        url: &'a str,
        monitoring_url: Option<&'a str>,
        desired_state: DesiredOutputState,
        config: &'a OutputConfig,
    ) -> OutputCreateFuture<'a>;
    fn update_output<'a>(
        &'a self,
        pipeline_id: &'a str,
        id: &'a str,
        name: &'a str,
        url: &'a str,
        monitoring_url: Option<&'a str>,
        config: &'a OutputConfig,
    ) -> OutputUpdateFuture<'a>;
    fn delete_output<'a>(&'a self, pipeline_id: &'a str, id: &'a str) -> OutputDeleteFuture<'a>;
    fn set_output_desired_state<'a>(
        &'a self,
        pipeline_id: &'a str,
        id: &'a str,
        desired_state: DesiredOutputState,
    ) -> OutputCreateFuture<'a>;
}

pub trait IngestLookup: Send + Sync {
    fn get_ingest<'a>(&'a self, id: &'a str) -> IngestLookupFuture<'a>;
    fn get_ingest_by_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestLookupFuture<'a>;
    fn list_ingests<'a>(&'a self) -> IngestCatalogFuture<'a>;
    fn list_ingests_for_filename<'a>(&'a self, filename: &'a str) -> IngestCatalogFuture<'a>;
    fn list_ingests_for_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestCatalogFuture<'a>;
}

pub trait IngestWriter: Send + Sync {
    #[allow(clippy::too_many_arguments)]
    fn create_ingest<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
        stream_key: &'a str,
        loop_flag: bool,
        start_time: &'a str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> IngestWriteFuture<'a>;
    #[allow(clippy::too_many_arguments)]
    fn update_ingest<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
        stream_key: &'a str,
        loop_flag: bool,
        start_time: &'a str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> IngestUpdateFuture<'a>;
    fn delete_ingest<'a>(&'a self, id: &'a str) -> IngestDeleteFuture<'a>;
}

pub trait MetaStore: Send + Sync {
    fn get_meta<'a>(&'a self, key: &'a str) -> MetaLookupFuture<'a>;
}

pub trait MetaStoreWriter: Send + Sync {
    fn set_meta<'a>(&'a self, key: &'a str, value: &'a str) -> MetaWriteFuture<'a>;
}

pub trait IngestHostStore: Send + Sync {
    fn get_ingest_host<'a>(&'a self) -> MetaLookupFuture<'a>;
    fn set_ingest_host<'a>(&'a self, host: &'a str) -> MetaWriteFuture<'a>;
}

pub trait SessionStore: Send + Sync {
    fn create_session<'a>(&'a self, token: &'a str, ts: i64) -> SessionWriteFuture<'a>;
    fn delete_session<'a>(&'a self, token: &'a str) -> SessionWriteFuture<'a>;
    fn prune_expired_sessions<'a>(&'a self, max_age_ms: i64) -> SessionWriteFuture<'a>;
    fn list_sessions<'a>(&'a self) -> SessionListFuture<'a>;
}

pub trait LogStore: Send + Sync {
    fn list_app_logs<'a>(&'a self, filters: &'a AppLogFilters) -> LogListFuture<'a>;
}

pub trait JobStore: Send + Sync {
    fn list_jobs<'a>(&'a self) -> JobListFuture<'a>;
}

#[derive(Clone)]
pub struct SqlitePipelineStore {
    pool: SqlitePool,
}

#[derive(Clone)]
pub struct SqliteOutputStore {
    pool: SqlitePool,
}

#[derive(Clone)]
pub struct SqliteIngestLookup {
    pool: SqlitePool,
}

#[derive(Clone)]
pub struct SqliteMetaStore {
    pool: SqlitePool,
}

#[derive(Clone)]
pub struct SqliteSessionStore {
    pool: SqlitePool,
}

#[derive(Clone)]
pub struct SqliteJobStore {
    pool: SqlitePool,
}

#[derive(Clone)]
pub struct SqliteLogStore {
    pool: SqlitePool,
}

impl SqlitePipelineStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteOutputStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteIngestLookup {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteMetaStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteSessionStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteJobStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl SqliteLogStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl PipelineStore for SqlitePipelineStore {
    fn get_pipeline<'a>(&'a self, id: &'a str) -> PipelineLookupFuture<'a> {
        Box::pin(async move {
            crate::db::get_pipeline(&self.pool, id)
                .await
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn get_pipeline_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineLookupFuture<'a> {
        Box::pin(async move {
            crate::db::get_pipeline_by_stream_key(&self.pool, stream_key)
                .await
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a> {
        Box::pin(async move {
            crate::db::list_pipelines(&self.pool)
                .await
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn create_pipeline<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        stream_key: &'a str,
        input_source: Option<&'a str>,
        srt_ingest_policy: Option<&'a str>,
    ) -> PipelineCreateFuture<'a> {
        Box::pin(async move {
            crate::db::create_pipeline(
                &self.pool,
                id,
                name,
                stream_key,
                input_source,
                srt_ingest_policy,
            )
            .await
            .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn update_pipeline<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        stream_key: &'a str,
        input_source: Option<&'a str>,
        srt_ingest_policy: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a> {
        Box::pin(async move {
            crate::db::update_pipeline(
                &self.pool,
                id,
                name,
                stream_key,
                input_source,
                srt_ingest_policy,
            )
            .await
            .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn delete_pipeline<'a>(&'a self, id: &'a str) -> PipelineDeleteFuture<'a> {
        Box::pin(async move {
            crate::db::delete_pipeline(&self.pool, id)
                .await
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn get_ingest_host<'a>(&'a self) -> PipelineIngestHostFuture<'a> {
        Box::pin(async move {
            crate::db::get_ingest_host(&self.pool)
                .await
                .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }

    fn update_pipeline_input_source<'a>(
        &'a self,
        pipeline: &'a Pipeline,
        input_source: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a> {
        Box::pin(async move {
            crate::db::update_pipeline(
                &self.pool,
                &pipeline.id,
                &pipeline.name,
                &pipeline.stream_key,
                input_source,
                pipeline.srt_ingest_policy.as_deref(),
            )
            .await
            .map_err(|err| PipelineStoreError::new(err.to_string()))
        })
    }
}

impl OutputStore for SqliteOutputStore {
    fn list_outputs<'a>(&'a self) -> OutputListFuture<'a> {
        Box::pin(async move {
            crate::db::list_outputs(&self.pool)
                .await
                .map_err(|err| OutputStoreError::new(err.to_string()))
        })
    }

    fn list_outputs_for_pipeline<'a>(&'a self, pipeline_id: &'a str) -> OutputListFuture<'a> {
        Box::pin(async move {
            crate::db::list_outputs_for_pipeline(&self.pool, pipeline_id)
                .await
                .map_err(|err| OutputStoreError::new(err.to_string()))
        })
    }

    fn get_output<'a>(&'a self, pipeline_id: &'a str, id: &'a str) -> OutputLookupFuture<'a> {
        Box::pin(async move {
            crate::db::get_output(&self.pool, pipeline_id, id)
                .await
                .map_err(|err| OutputStoreError::new(err.to_string()))
        })
    }

    fn create_output<'a>(
        &'a self,
        id: &'a str,
        pipeline_id: &'a str,
        name: &'a str,
        url: &'a str,
        monitoring_url: Option<&'a str>,
        desired_state: DesiredOutputState,
        config: &'a OutputConfig,
    ) -> OutputCreateFuture<'a> {
        Box::pin(async move {
            crate::db::create_output(
                &self.pool,
                id,
                pipeline_id,
                name,
                url,
                monitoring_url,
                desired_state,
                config,
            )
            .await
            .map_err(|err| OutputStoreError::new(err.to_string()))
        })
    }

    fn update_output<'a>(
        &'a self,
        pipeline_id: &'a str,
        id: &'a str,
        name: &'a str,
        url: &'a str,
        monitoring_url: Option<&'a str>,
        config: &'a OutputConfig,
    ) -> OutputUpdateFuture<'a> {
        Box::pin(async move {
            crate::db::update_output(
                &self.pool,
                pipeline_id,
                id,
                name,
                url,
                monitoring_url,
                config,
            )
            .await
            .map_err(|err| OutputStoreError::new(err.to_string()))
        })
    }

    fn delete_output<'a>(&'a self, pipeline_id: &'a str, id: &'a str) -> OutputDeleteFuture<'a> {
        Box::pin(async move {
            crate::db::delete_output(&self.pool, pipeline_id, id)
                .await
                .map_err(|err| OutputStoreError::new(err.to_string()))
        })
    }

    fn set_output_desired_state<'a>(
        &'a self,
        pipeline_id: &'a str,
        id: &'a str,
        desired_state: DesiredOutputState,
    ) -> OutputCreateFuture<'a> {
        Box::pin(async move {
            crate::db::set_output_desired_state(&self.pool, pipeline_id, id, desired_state)
                .await
                .map_err(|err| OutputStoreError::new(err.to_string()))
        })
    }
}

impl IngestLookup for SqliteIngestLookup {
    fn get_ingest<'a>(&'a self, id: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move {
            crate::db::get_ingest(&self.pool, id)
                .await
                .map_err(|err| IngestLookupError::new(err.to_string()))
        })
    }

    fn get_ingest_by_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestLookupFuture<'a> {
        Box::pin(async move {
            crate::db::get_ingest_by_stream_key(&self.pool, stream_key)
                .await
                .map_err(|err| IngestLookupError::new(err.to_string()))
        })
    }

    fn list_ingests<'a>(&'a self) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            crate::db::list_ingests(&self.pool)
                .await
                .map_err(|err| IngestLookupError::new(err.to_string()))
        })
    }

    fn list_ingests_for_filename<'a>(&'a self, filename: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            crate::db::list_ingests_for_filename(&self.pool, filename)
                .await
                .map_err(|err| IngestLookupError::new(err.to_string()))
        })
    }

    fn list_ingests_for_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestCatalogFuture<'a> {
        Box::pin(async move {
            crate::db::list_ingests_for_stream_key(&self.pool, stream_key)
                .await
                .map_err(|err| IngestLookupError::new(err.to_string()))
        })
    }
}

impl IngestWriter for SqliteIngestLookup {
    fn create_ingest<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
        stream_key: &'a str,
        loop_flag: bool,
        start_time: &'a str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> IngestWriteFuture<'a> {
        Box::pin(async move {
            crate::db::create_ingest(
                &self.pool,
                id,
                filename,
                stream_key,
                loop_flag,
                start_time,
                live_optimized,
                target_gop_seconds,
            )
            .await
            .map_err(|err| IngestWriteError::new(err.to_string()))
        })
    }

    fn update_ingest<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
        stream_key: &'a str,
        loop_flag: bool,
        start_time: &'a str,
        live_optimized: bool,
        target_gop_seconds: u32,
    ) -> IngestUpdateFuture<'a> {
        Box::pin(async move {
            crate::db::update_ingest(
                &self.pool,
                id,
                filename,
                stream_key,
                loop_flag,
                start_time,
                live_optimized,
                target_gop_seconds,
            )
            .await
            .map_err(|err| IngestWriteError::new(err.to_string()))
        })
    }

    fn delete_ingest<'a>(&'a self, id: &'a str) -> IngestDeleteFuture<'a> {
        Box::pin(async move {
            crate::db::delete_ingest(&self.pool, id)
                .await
                .map_err(|err| IngestWriteError::new(err.to_string()))
        })
    }
}

impl MetaStore for SqliteMetaStore {
    fn get_meta<'a>(&'a self, key: &'a str) -> MetaLookupFuture<'a> {
        Box::pin(async move {
            crate::db::get_meta(&self.pool, key)
                .await
                .map_err(|err| MetaLookupError::new(err.to_string()))
        })
    }
}

impl MetaStoreWriter for SqliteMetaStore {
    fn set_meta<'a>(&'a self, key: &'a str, value: &'a str) -> MetaWriteFuture<'a> {
        Box::pin(async move {
            crate::db::set_meta(&self.pool, key, value)
                .await
                .map_err(|err| MetaLookupError::new(err.to_string()))
        })
    }
}

impl IngestHostStore for SqliteMetaStore {
    fn get_ingest_host<'a>(&'a self) -> MetaLookupFuture<'a> {
        Box::pin(async move {
            crate::db::get_ingest_host(&self.pool)
                .await
                .map_err(|err| MetaLookupError::new(err.to_string()))
        })
    }

    fn set_ingest_host<'a>(&'a self, host: &'a str) -> MetaWriteFuture<'a> {
        Box::pin(async move {
            crate::db::set_ingest_host(&self.pool, host)
                .await
                .map_err(|err| MetaLookupError::new(err.to_string()))
        })
    }
}

impl SessionStore for SqliteSessionStore {
    fn create_session<'a>(&'a self, token: &'a str, ts: i64) -> SessionWriteFuture<'a> {
        Box::pin(async move {
            crate::db::create_session(&self.pool, token, ts)
                .await
                .map_err(|err| SessionStoreError::new(err.to_string()))
        })
    }

    fn delete_session<'a>(&'a self, token: &'a str) -> SessionWriteFuture<'a> {
        Box::pin(async move {
            crate::db::delete_session(&self.pool, token)
                .await
                .map_err(|err| SessionStoreError::new(err.to_string()))
        })
    }

    fn prune_expired_sessions<'a>(&'a self, max_age_ms: i64) -> SessionWriteFuture<'a> {
        Box::pin(async move {
            crate::db::prune_expired_sessions(&self.pool, max_age_ms)
                .await
                .map_err(|err| SessionStoreError::new(err.to_string()))
        })
    }

    fn list_sessions<'a>(&'a self) -> SessionListFuture<'a> {
        Box::pin(async move {
            crate::db::list_sessions(&self.pool)
                .await
                .map_err(|err| SessionStoreError::new(err.to_string()))
        })
    }
}

impl JobStore for SqliteJobStore {
    fn list_jobs<'a>(&'a self) -> JobListFuture<'a> {
        Box::pin(async move {
            crate::db::list_jobs(&self.pool)
                .await
                .map_err(|err| JobStoreError::new(err.to_string()))
        })
    }
}

impl LogStore for SqliteLogStore {
    fn list_app_logs<'a>(&'a self, filters: &'a AppLogFilters) -> LogListFuture<'a> {
        Box::pin(async move {
            crate::db::list_app_logs(&self.pool, filters)
                .await
                .map_err(|err| LogStoreError::new(err.to_string()))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn test_pool() -> SqlitePool {
        let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
        crate::db::setup_database_schema(&pool).await.unwrap();
        pool
    }

    #[tokio::test]
    async fn sqlite_pipeline_store_returns_pipeline_for_stream_key() {
        let pool = test_pool().await;
        crate::db::create_pipeline(&pool, "p1", "Pipeline", "stream-key", None, None)
            .await
            .unwrap();
        let store = SqlitePipelineStore::new(pool);

        let pipeline = store
            .get_pipeline_by_stream_key("stream-key")
            .await
            .unwrap();

        assert_eq!(pipeline.unwrap().id, "p1");
    }

    #[tokio::test]
    async fn sqlite_pipeline_store_returns_none_for_missing_stream_key() {
        let pool = test_pool().await;
        let store = SqlitePipelineStore::new(pool);

        let pipeline = store.get_pipeline_by_stream_key("missing").await.unwrap();

        assert!(pipeline.is_none());
    }

    #[tokio::test]
    async fn sqlite_pipeline_store_lists_pipelines() {
        let pool = test_pool().await;
        crate::db::create_pipeline(&pool, "p1", "Pipeline One", "stream-one", None, None)
            .await
            .unwrap();
        crate::db::create_pipeline(&pool, "p2", "Pipeline Two", "stream-two", None, None)
            .await
            .unwrap();
        let store = SqlitePipelineStore::new(pool);

        let pipelines = store.list_pipelines().await.unwrap();

        assert_eq!(pipelines.len(), 2);
        assert!(pipelines.iter().any(|pipeline| pipeline.id == "p1"));
        assert!(pipelines.iter().any(|pipeline| pipeline.id == "p2"));
    }

    #[tokio::test]
    async fn sqlite_ingest_lookup_reads_ingest_by_id_and_latest_stream_key_entry() {
        let pool = test_pool().await;
        crate::db::create_ingest(
            &pool,
            "i1",
            "clip.mp4",
            "stream-key",
            true,
            "00:00:05",
            true,
            4,
        )
        .await
        .unwrap();
        crate::db::create_ingest(
            &pool,
            "i2",
            "clip-latest.mp4",
            "stream-key",
            false,
            "00:00:10",
            false,
            2,
        )
        .await
        .unwrap();
        let lookup = SqliteIngestLookup::new(pool);

        let by_id = lookup.get_ingest("i1").await.unwrap();
        let by_stream_key = lookup.get_ingest_by_stream_key("stream-key").await.unwrap();

        assert_eq!(by_id.as_ref().map(|ingest| ingest.id.as_str()), Some("i1"));
        assert_eq!(
            by_stream_key.as_ref().map(|ingest| ingest.id.as_str()),
            Some("i2")
        );
    }

    #[tokio::test]
    async fn sqlite_ingest_lookup_lists_ingests_for_stream_key() {
        let pool = test_pool().await;
        crate::db::create_ingest(&pool, "i1", "clip.mp4", "stream-key", true, "", false, 2)
            .await
            .unwrap();
        crate::db::create_ingest(&pool, "i2", "clip-2.mp4", "other-key", false, "", false, 2)
            .await
            .unwrap();
        crate::db::create_ingest(&pool, "i3", "clip-3.mp4", "stream-key", false, "", false, 2)
            .await
            .unwrap();
        let lookup = SqliteIngestLookup::new(pool);

        let ingests = lookup
            .list_ingests_for_stream_key("stream-key")
            .await
            .unwrap();

        assert_eq!(ingests.len(), 2);
        assert_eq!(ingests[0].id, "i1");
        assert_eq!(ingests[1].id, "i3");
    }

    #[tokio::test]
    async fn sqlite_meta_store_returns_meta_value() {
        let pool = test_pool().await;
        crate::db::set_meta(&pool, "test-key", "test-value")
            .await
            .unwrap();
        let store = SqliteMetaStore::new(pool);

        let value = store.get_meta("test-key").await.unwrap();

        assert_eq!(value.as_deref(), Some("test-value"));
    }

    #[tokio::test]
    async fn sqlite_meta_store_writes_meta_value() {
        let pool = test_pool().await;
        let store = SqliteMetaStore::new(pool.clone());

        store.set_meta("test-key", "test-value").await.unwrap();

        let value = crate::db::get_meta(&pool, "test-key").await.unwrap();
        assert_eq!(value.as_deref(), Some("test-value"));
    }
}
