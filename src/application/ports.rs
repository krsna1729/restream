//! Application-layer port traits defining the storage and catalog capabilities
//! that orchestration code depends on.

use crate::application::notify::PushSubscription;
use restream_domain::models::{Ingest, Job, Pipeline};
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

/// The error every store port returns: what failed, as text. Each port keeps
/// its own name for readability; they are one type.
#[derive(Debug, Clone)]
pub struct StoreError {
    message: String,
}

impl StoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for StoreError {}

pub type PipelineStoreError = StoreError;

pub type IngestLookupError = StoreError;

pub type IngestWriteError = StoreError;

pub type MetaLookupError = StoreError;

pub type SessionStoreError = StoreError;

pub type JobStoreError = StoreError;

pub type RecordingStoreError = StoreError;

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
    /// Updates only `filename`, leaving `stream_key`, `loop_flag`, `start_time`,
    /// `live_optimized`, and `target_gop_seconds` untouched. Callers that only
    /// intend to rename an ingest's backing file (media-library rename and its
    /// rollback) must use this instead of `update_ingest`: that method requires
    /// the full field set and overwrites every column from whatever `Ingest`
    /// snapshot the caller holds, silently reverting any of those fields if
    /// they were changed by a concurrent request after the snapshot was taken.
    fn update_ingest_filename<'a>(
        &'a self,
        id: &'a str,
        filename: &'a str,
    ) -> IngestUpdateFuture<'a>;
    /// Sets the ingest's playlist (play order, the first being its
    /// `filename`); an empty list makes it a single file again.
    fn set_ingest_playlist<'a>(
        &'a self,
        id: &'a str,
        playlist: &'a [String],
    ) -> IngestUpdateFuture<'a>;
    /// Renames one media file wherever this ingest uses it: its `filename`
    /// and every playlist entry. Rename and its rollback use this.
    fn rename_ingest_file<'a>(
        &'a self,
        id: &'a str,
        from: &'a str,
        to: &'a str,
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

/// How one HTTP attempt ended: the status it got, or why it got none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PostOutcome {
    pub status: Option<u16>,
    pub error: Option<String>,
}

impl PostOutcome {
    pub fn ok(&self) -> bool {
        self.status
            .is_some_and(|status| (200..300).contains(&status))
    }
}

pub type PostFuture<'a> = Pin<Box<dyn Future<Output = PostOutcome> + Send + 'a>>;

/// Sends notifications over the network.
pub trait NotifyTransport: Send + Sync {
    /// POSTs a JSON body with extra headers.
    fn post_json<'a>(
        &'a self,
        url: &'a str,
        body: Vec<u8>,
        headers: Vec<(&'static str, String)>,
    ) -> PostFuture<'a>;
    /// Encrypts `payload` for the subscription and sends it to its push
    /// service, signed with the engine's VAPID key naming `subject`.
    fn push<'a>(
        &'a self,
        subscription: &'a PushSubscription,
        payload: Vec<u8>,
        urgent: bool,
        subject: &'a str,
    ) -> PostFuture<'a>;
    /// The VAPID public key browsers subscribe with (base64url).
    fn push_public_key(&self) -> Pin<Box<dyn Future<Output = Result<String, String>> + Send + '_>>;
}

pub trait JobStore: Send + Sync {
    fn list_jobs<'a>(&'a self) -> JobListFuture<'a>;
}

pub trait RecordingStore: Send + Sync {
    fn list_recordings<'a>(&'a self) -> RecordingListFuture<'a>;
}
