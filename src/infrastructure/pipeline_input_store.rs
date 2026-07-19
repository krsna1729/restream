use sqlx::SqlitePool;

use crate::application::ingest::{PipelineInputLookup, PipelineInputLookupFuture};
use crate::application::pipeline_inputs::{
    InputDeleteFuture, InputListFuture, InputLookupFuture, InputUpdateFuture, InputWriteFuture,
    PipelineInputStore, PipelineInputStoreError,
};
use crate::application::ports::PipelineStoreError;

#[derive(Clone)]
pub struct SqlitePipelineInputStore {
    pool: SqlitePool,
}

impl SqlitePipelineInputStore {
    pub fn new(pool: SqlitePool) -> Self {
        Self { pool }
    }
}

impl PipelineInputStore for SqlitePipelineInputStore {
    fn get<'a>(&'a self, pipeline_id: &'a str, input_id: &'a str) -> InputLookupFuture<'a> {
        Box::pin(async move {
            crate::db::get_pipeline_input(&self.pool, pipeline_id, input_id)
                .await
                .map_err(map_error)
        })
    }

    fn get_by_stream_key<'a>(&'a self, stream_key: &'a str) -> InputLookupFuture<'a> {
        Box::pin(async move {
            crate::db::get_pipeline_input_by_stream_key(&self.pool, stream_key)
                .await
                .map_err(map_error)
        })
    }

    fn list<'a>(&'a self, pipeline_id: &'a str) -> InputListFuture<'a> {
        Box::pin(async move {
            crate::db::list_pipeline_inputs(&self.pool, pipeline_id)
                .await
                .map_err(map_error)
        })
    }

    fn create<'a>(
        &'a self,
        id: &'a str,
        pipeline_id: &'a str,
        label: &'a str,
        stream_key: &'a str,
    ) -> InputWriteFuture<'a> {
        Box::pin(async move {
            crate::db::create_pipeline_input(&self.pool, id, pipeline_id, label, stream_key)
                .await
                .map_err(map_error)
        })
    }

    fn update<'a>(
        &'a self,
        pipeline_id: &'a str,
        input_id: &'a str,
        label: &'a str,
        enabled: bool,
    ) -> InputUpdateFuture<'a> {
        Box::pin(async move {
            crate::db::update_pipeline_input(&self.pool, pipeline_id, input_id, label, enabled)
                .await
                .map_err(map_error)
        })
    }

    fn delete<'a>(&'a self, pipeline_id: &'a str, input_id: &'a str) -> InputDeleteFuture<'a> {
        Box::pin(async move {
            crate::db::delete_pipeline_input(&self.pool, pipeline_id, input_id)
                .await
                .map_err(map_error)
        })
    }

    fn promote<'a>(&'a self, pipeline_id: &'a str, input_id: &'a str) -> InputUpdateFuture<'a> {
        Box::pin(async move {
            crate::db::promote_pipeline_input(&self.pool, pipeline_id, input_id)
                .await
                .map_err(map_error)
        })
    }
}

impl PipelineInputLookup for SqlitePipelineInputStore {
    fn get_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineInputLookupFuture<'a> {
        Box::pin(async move {
            crate::db::get_pipeline_input_by_stream_key(&self.pool, stream_key)
                .await
                .map_err(|error| PipelineStoreError::new(error.to_string()))
        })
    }
}

fn map_error(error: sqlx::Error) -> PipelineInputStoreError {
    let message = error.to_string();
    if message.contains("UNIQUE constraint failed")
        || message.contains("pipeline input limit exceeded")
    {
        PipelineInputStoreError::Conflict(message)
    } else {
        PipelineInputStoreError::Internal(message)
    }
}
