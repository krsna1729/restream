use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::application::services::{PipelineService, ServiceError, ServiceResult};
use crate::domain::pipeline_input::{PipelineInput, PipelineInputRole};

pub const MAX_PIPELINE_INPUTS: usize = 4;

pub type InputLookupFuture<'a> = Pin<
    Box<dyn Future<Output = Result<Option<PipelineInput>, PipelineInputStoreError>> + Send + 'a>,
>;
pub type InputListFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Vec<PipelineInput>, PipelineInputStoreError>> + Send + 'a>>;
pub type InputWriteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<PipelineInput, PipelineInputStoreError>> + Send + 'a>>;
pub type InputUpdateFuture<'a> = Pin<
    Box<dyn Future<Output = Result<Option<PipelineInput>, PipelineInputStoreError>> + Send + 'a>,
>;
pub type InputDeleteFuture<'a> =
    Pin<Box<dyn Future<Output = Result<bool, PipelineInputStoreError>> + Send + 'a>>;

#[derive(Debug, Clone)]
pub enum PipelineInputStoreError {
    Conflict(String),
    Internal(String),
}

impl fmt::Display for PipelineInputStoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Conflict(message) | Self::Internal(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for PipelineInputStoreError {}

pub trait PipelineInputStore: Send + Sync {
    fn get<'a>(&'a self, pipeline_id: &'a str, input_id: &'a str) -> InputLookupFuture<'a>;
    fn get_by_stream_key<'a>(&'a self, stream_key: &'a str) -> InputLookupFuture<'a>;
    fn list<'a>(&'a self, pipeline_id: &'a str) -> InputListFuture<'a>;
    fn create<'a>(
        &'a self,
        id: &'a str,
        pipeline_id: &'a str,
        label: &'a str,
        stream_key: &'a str,
    ) -> InputWriteFuture<'a>;
    fn update<'a>(
        &'a self,
        pipeline_id: &'a str,
        input_id: &'a str,
        label: &'a str,
        enabled: bool,
    ) -> InputUpdateFuture<'a>;
    fn delete<'a>(&'a self, pipeline_id: &'a str, input_id: &'a str) -> InputDeleteFuture<'a>;
    fn promote<'a>(&'a self, pipeline_id: &'a str, input_id: &'a str) -> InputUpdateFuture<'a>;
}

#[derive(Clone)]
pub struct PipelineInputService {
    store: Arc<dyn PipelineInputStore>,
    pipelines: PipelineService,
}

impl PipelineInputService {
    pub fn with_store(store: Arc<dyn PipelineInputStore>, pipelines: PipelineService) -> Self {
        Self { store, pipelines }
    }

    pub async fn list(&self, pipeline_id: &str) -> ServiceResult<Vec<PipelineInput>> {
        self.pipelines.get_by_id(pipeline_id).await?;
        self.store
            .list(pipeline_id)
            .await
            .map_err(|error| ServiceError::internal(format!("list pipeline inputs: {error}")))
    }

    pub async fn get_by_stream_key(
        &self,
        stream_key: &str,
    ) -> ServiceResult<Option<PipelineInput>> {
        self.store
            .get_by_stream_key(stream_key)
            .await
            .map(|input| input.filter(|candidate| candidate.enabled))
            .map_err(|error| ServiceError::internal(format!("get pipeline input: {error}")))
    }

    pub async fn create(&self, pipeline_id: &str, label: &str) -> ServiceResult<PipelineInput> {
        let existing = self.list(pipeline_id).await?;
        if existing.len() >= MAX_PIPELINE_INPUTS {
            return Err(ServiceError::conflict(format!(
                "pipeline input limit is {MAX_PIPELINE_INPUTS}"
            )));
        }
        for _ in 0..16 {
            let input_id = format!("input_{}", hex(&rand::random::<[u8; 8]>()));
            let stream_key = generated_stream_key();
            match self
                .store
                .create(&input_id, pipeline_id, label, &stream_key)
                .await
            {
                Ok(input) => return Ok(input),
                Err(PipelineInputStoreError::Conflict(message))
                    if message.contains("pipeline input limit exceeded") =>
                {
                    return Err(ServiceError::conflict(format!(
                        "pipeline input limit is {MAX_PIPELINE_INPUTS}"
                    )));
                }
                Err(PipelineInputStoreError::Conflict(_)) => continue,
                Err(PipelineInputStoreError::Internal(message)) => {
                    return Err(ServiceError::internal(format!(
                        "create pipeline input: {message}"
                    )));
                }
            }
        }
        Err(ServiceError::internal(
            "could not allocate a unique pipeline input credential",
        ))
    }

    pub async fn update(
        &self,
        pipeline_id: &str,
        input_id: &str,
        label: &str,
        enabled: bool,
    ) -> ServiceResult<PipelineInput> {
        let current = self.get(pipeline_id, input_id).await?;
        if !enabled && (current.role == PipelineInputRole::Primary || current.selected) {
            return Err(ServiceError::conflict(
                "primary or selected inputs cannot be disabled",
            ));
        }
        self.store
            .update(pipeline_id, input_id, label, enabled)
            .await
            .map_err(store_error)?
            .ok_or_else(|| input_not_found(input_id))
    }

    pub async fn delete(&self, pipeline_id: &str, input_id: &str) -> ServiceResult<bool> {
        let current = self.get(pipeline_id, input_id).await?;
        if current.role == PipelineInputRole::Primary || current.selected {
            return Err(ServiceError::conflict(
                "primary or selected inputs cannot be deleted",
            ));
        }
        self.store
            .delete(pipeline_id, input_id)
            .await
            .map_err(store_error)
    }

    pub async fn promote(&self, pipeline_id: &str, input_id: &str) -> ServiceResult<PipelineInput> {
        let current = self.get(pipeline_id, input_id).await?;
        if !current.enabled {
            return Err(ServiceError::conflict("disabled inputs cannot be promoted"));
        }
        self.store
            .promote(pipeline_id, input_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| input_not_found(input_id))
    }

    pub async fn get(&self, pipeline_id: &str, input_id: &str) -> ServiceResult<PipelineInput> {
        self.store
            .get(pipeline_id, input_id)
            .await
            .map_err(store_error)?
            .ok_or_else(|| input_not_found(input_id))
    }
}

pub fn generated_stream_key() -> String {
    format!("sk_{}", hex(&rand::random::<[u8; 32]>()))
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn input_not_found(input_id: &str) -> ServiceError {
    ServiceError::not_found(format!("pipeline input {input_id} not found"))
}

fn store_error(error: PipelineInputStoreError) -> ServiceError {
    match error {
        PipelineInputStoreError::Conflict(message) => ServiceError::conflict(message),
        // Internal messages are the raw sqlx Display. A SQLITE_BUSY failure
        // here becomes HTTP 500 `{"error":"error returned from database:
        // (code: 5) database is locked"}` with no service prefix — the
        // shape seen on the concurrency-contract flake. Retry happens in
        // `SqlitePipelineInputStore` before this mapping.
        PipelineInputStoreError::Internal(message) => ServiceError::internal(message),
    }
}
