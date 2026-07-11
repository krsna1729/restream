//! Application-layer port traits defining the storage and catalog capabilities
//! that orchestration code depends on.

use crate::application::models::{Ingest, Job, Output, Pipeline};
use crate::domain::output_spec::OutputConfig;
use crate::domain::state::DesiredOutputState;
use crate::logging::types::{AppLogFilters, AppLogRow};
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
pub type SessionLookupFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<i64>, SessionStoreError>> + Send + 'a>>;
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
pub type RecordingListFuture<'a> = Pin<
    Box<dyn Future<Output = Result<Vec<RecordingCatalogRow>, RecordingStoreError>> + Send + 'a>,
>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordingCatalogRow {
    pub recording_id: String,
    pub pipeline_id: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub status: String,
    pub temp_path: Option<String>,
    pub final_path: Option<String>,
    pub codec_summary: Option<String>,
    pub error: Option<String>,
}

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

#[derive(Debug, Clone)]
pub struct RecordingStoreError {
    message: String,
}

impl RecordingStoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for RecordingStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for RecordingStoreError {}

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
    fn delete_sessions_except<'a>(&'a self, token: &'a str) -> SessionWriteFuture<'a>;
    fn get_session_created_at<'a>(&'a self, token: &'a str) -> SessionLookupFuture<'a>;
    fn prune_expired_sessions<'a>(&'a self, max_age_ms: i64) -> SessionWriteFuture<'a>;
    fn list_sessions<'a>(&'a self) -> SessionListFuture<'a>;
}

pub trait LogStore: Send + Sync {
    fn list_app_logs<'a>(&'a self, filters: &'a AppLogFilters) -> LogListFuture<'a>;
}

pub trait JobStore: Send + Sync {
    fn list_jobs<'a>(&'a self) -> JobListFuture<'a>;
}

pub trait RecordingStore: Send + Sync {
    fn list_recordings<'a>(&'a self) -> RecordingListFuture<'a>;
}
