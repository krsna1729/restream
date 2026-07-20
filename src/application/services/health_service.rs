//! Application service wrapper for lightweight health probes.
//!
//! This module keeps store-backed liveness checks out of the HTTP layer so
//! handlers only decide which probes to expose and how to serialize them.

use std::sync::Arc;

use crate::application::ports::PipelineStore;

use super::error::{ServiceError, ServiceResult};

#[derive(Clone)]
/// Application service that exposes lightweight persistence-backed health
/// probes for the dashboard and external liveness checks.
pub struct HealthService {
    store: Arc<dyn PipelineStore>,
}

impl HealthService {
    /// Builds the service from the pipeline store used for low-cost DB checks.
    pub fn with_store(store: Arc<dyn PipelineStore>) -> Self {
        Self { store }
    }

    /// Performs the low-cost database health probe by issuing one catalog read
    /// against the pipeline store and translating success into a simple boolean.
    pub async fn check_db(&self) -> ServiceResult<bool> {
        self.store
            .list_pipelines()
            .await
            .map(|_| true)
            .map_err(|e| ServiceError::internal(format!("db health check: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::models::Pipeline;
    use crate::application::ports::{
        PipelineCreateFuture, PipelineDeleteFuture, PipelineIngestHostFuture, PipelineListFuture,
        PipelineLookupFuture, PipelineStoreError, PipelineUpdateFuture,
    };

    struct FakePipelineStore {
        fail: bool,
    }

    impl PipelineStore for FakePipelineStore {
        fn get_pipeline<'a>(&'a self, _id: &'a str) -> PipelineLookupFuture<'a> {
            Box::pin(async { Ok(None) })
        }

        fn get_pipeline_by_stream_key<'a>(
            &'a self,
            _stream_key: &'a str,
        ) -> PipelineLookupFuture<'a> {
            Box::pin(async { Ok(None) })
        }

        fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a> {
            Box::pin(async move {
                if self.fail {
                    Err(PipelineStoreError::new("database unavailable"))
                } else {
                    Ok(Vec::new())
                }
            })
        }

        fn create_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> PipelineCreateFuture<'a> {
            Box::pin(async { Err(PipelineStoreError::new("not implemented")) })
        }

        fn update_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> PipelineUpdateFuture<'a> {
            Box::pin(async { Err(PipelineStoreError::new("not implemented")) })
        }

        fn delete_pipeline<'a>(&'a self, _id: &'a str) -> PipelineDeleteFuture<'a> {
            Box::pin(async { Ok(false) })
        }

        fn get_ingest_host<'a>(&'a self) -> PipelineIngestHostFuture<'a> {
            Box::pin(async { Ok(None) })
        }

        fn update_pipeline_input_source<'a>(
            &'a self,
            _pipeline: &'a Pipeline,
            _input_source: Option<&'a str>,
        ) -> PipelineUpdateFuture<'a> {
            Box::pin(async { Err(PipelineStoreError::new("not implemented")) })
        }
    }

    #[tokio::test]
    async fn health_service_uses_injected_store_for_db_check() {
        let service = HealthService::with_store(Arc::new(FakePipelineStore { fail: false }));
        assert!(service.check_db().await.unwrap());
    }

    #[tokio::test]
    async fn health_service_surfaces_store_failure() {
        let service = HealthService::with_store(Arc::new(FakePipelineStore { fail: true }));
        assert!(matches!(
            service.check_db().await.unwrap_err(),
            ServiceError::Internal(_)
        ));
    }
}
