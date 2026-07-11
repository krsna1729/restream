use std::sync::Arc;

use crate::application::ports::OutputStore;
use crate::domain::output_spec::OutputConfig;
use crate::domain::state::DesiredOutputState;
use crate::types::Output;

use super::error::{ApiError, ApiResult};

/// Application service for output CRUD and lifecycle requests.
///
/// Depends on `OutputStore` rather than `SqlitePool` directly. Infrastructure
/// wiring provides the default SQLite constructor; tests can inject any
/// implementation.
#[derive(Clone)]
pub struct OutputService {
    store: Arc<dyn OutputStore>,
}

impl OutputService {
    pub fn with_store(store: Arc<dyn OutputStore>) -> Self {
        Self { store }
    }

    pub async fn list_outputs(&self) -> ApiResult<Vec<Output>> {
        self.store
            .list_outputs()
            .await
            .map_err(|e| ApiError::internal(format!("list outputs: {e}")))
    }

    pub async fn list_for_pipeline(&self, pipeline_id: &str) -> ApiResult<Vec<Output>> {
        self.store
            .list_outputs_for_pipeline(pipeline_id)
            .await
            .map_err(|e| ApiError::internal(format!("list outputs for pipeline: {e}")))
    }

    pub async fn get_by_id(&self, pipeline_id: &str, id: &str) -> ApiResult<Output> {
        self.store
            .get_output(pipeline_id, id)
            .await
            .map_err(|e| ApiError::internal(format!("get output: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("output {id} not found")))
    }

    #[allow(clippy::too_many_arguments)]
    pub async fn create_output(
        &self,
        id: &str,
        pipeline_id: &str,
        name: &str,
        url: &str,
        monitoring_url: Option<&str>,
        desired_state: &str,
        config: &OutputConfig,
    ) -> ApiResult<Output> {
        let desired_state = DesiredOutputState::from(desired_state);
        self.store
            .create_output(
                id,
                pipeline_id,
                name,
                url,
                monitoring_url,
                desired_state,
                config,
            )
            .await
            .map_err(|e| ApiError::internal(format!("create output: {e}")))
    }

    pub async fn update_output(
        &self,
        pipeline_id: &str,
        id: &str,
        name: &str,
        url: &str,
        monitoring_url: Option<&str>,
        config: &OutputConfig,
    ) -> ApiResult<Output> {
        self.store
            .update_output(pipeline_id, id, name, url, monitoring_url, config)
            .await
            .map_err(|e| ApiError::internal(format!("update output: {e}")))?
            .ok_or_else(|| ApiError::not_found(format!("output {id} not found")))
    }

    pub async fn delete_output(&self, pipeline_id: &str, id: &str) -> ApiResult<bool> {
        self.store
            .delete_output(pipeline_id, id)
            .await
            .map_err(|e| ApiError::internal(format!("delete output: {e}")))
    }

    /// Set the output's desired state to `running`, resuming any stopped egress.
    pub async fn request_start(&self, pipeline_id: &str, id: &str) -> ApiResult<Output> {
        self.store
            .set_output_desired_state(pipeline_id, id, DesiredOutputState::Running)
            .await
            .map_err(|e| ApiError::internal(format!("request start: {e}")))
    }

    /// Set the output's desired state to `stopped`, halting any active egress.
    pub async fn request_stop(&self, pipeline_id: &str, id: &str) -> ApiResult<Output> {
        self.store
            .set_output_desired_state(pipeline_id, id, DesiredOutputState::Stopped)
            .await
            .map_err(|e| ApiError::internal(format!("request stop: {e}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::{
        OutputCreateFuture, OutputDeleteFuture, OutputListFuture, OutputLookupFuture, OutputStore,
        OutputStoreError, OutputUpdateFuture,
    };
    use std::sync::Mutex;

    struct FakeOutputStore {
        output: Mutex<Output>,
    }

    impl FakeOutputStore {
        fn new(output: Output) -> Self {
            Self {
                output: Mutex::new(output),
            }
        }
    }

    impl OutputStore for FakeOutputStore {
        fn list_outputs<'a>(&'a self) -> OutputListFuture<'a> {
            Box::pin(async move { Ok(vec![self.output.lock().unwrap().clone()]) })
        }

        fn list_outputs_for_pipeline<'a>(&'a self, pipeline_id: &'a str) -> OutputListFuture<'a> {
            Box::pin(async move {
                let output = self.output.lock().unwrap().clone();
                Ok((output.pipeline_id == pipeline_id)
                    .then_some(output)
                    .into_iter()
                    .collect())
            })
        }

        fn get_output<'a>(&'a self, pipeline_id: &'a str, id: &'a str) -> OutputLookupFuture<'a> {
            Box::pin(async move {
                let output = self.output.lock().unwrap().clone();
                Ok((output.pipeline_id == pipeline_id && output.id == id).then_some(output))
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
                let output = Output {
                    id: id.to_string(),
                    pipeline_id: pipeline_id.to_string(),
                    name: name.to_string(),
                    url: url.to_string(),
                    monitoring_url: monitoring_url.map(str::to_string),
                    desired_state,
                    config: config.clone(),
                };
                *self.output.lock().unwrap() = output.clone();
                Ok(output)
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
                let mut guard = self.output.lock().unwrap();
                if guard.pipeline_id != pipeline_id || guard.id != id {
                    return Ok(None);
                }
                guard.name = name.to_string();
                guard.url = url.to_string();
                guard.monitoring_url = monitoring_url.map(str::to_string);
                guard.config = config.clone();
                Ok(Some(guard.clone()))
            })
        }

        fn delete_output<'a>(
            &'a self,
            pipeline_id: &'a str,
            id: &'a str,
        ) -> OutputDeleteFuture<'a> {
            Box::pin(async move {
                let output = self.output.lock().unwrap();
                Ok(output.pipeline_id == pipeline_id && output.id == id)
            })
        }

        fn set_output_desired_state<'a>(
            &'a self,
            pipeline_id: &'a str,
            id: &'a str,
            desired_state: DesiredOutputState,
        ) -> OutputCreateFuture<'a> {
            Box::pin(async move {
                let mut output = self.output.lock().unwrap();
                if output.pipeline_id != pipeline_id || output.id != id {
                    return Err(OutputStoreError::new("not found"));
                }
                output.desired_state = desired_state;
                Ok(output.clone())
            })
        }
    }

    #[tokio::test]
    async fn output_service_lifecycle_uses_injected_store() {
        let output = Output {
            id: "out-1".to_string(),
            pipeline_id: "pipe-1".to_string(),
            name: "Output".to_string(),
            url: "rtmp://localhost/live/key".to_string(),
            monitoring_url: None,
            desired_state: DesiredOutputState::Running,
            config: OutputConfig::default(),
        };
        let service = OutputService::with_store(Arc::new(FakeOutputStore::new(output)));

        let stopped = service.request_stop("pipe-1", "out-1").await.unwrap();
        assert_eq!(stopped.desired_state, DesiredOutputState::Stopped);

        let running = service.request_start("pipe-1", "out-1").await.unwrap();
        assert_eq!(running.desired_state, DesiredOutputState::Running);
    }
}
