use crate::db::DeadlineDb;

use crate::application::catalog_auth::{CatalogAuthLoadFuture, CatalogAuthSource, UnreadableInput};
use crate::application::ingest::{PipelineInputLookup, PipelineInputLookupFuture};
use crate::application::pipeline_inputs::{
    InputDeleteFuture, InputListFuture, InputLookupFuture, InputUpdateFuture, InputWriteFuture,
    PipelineInputStore, PipelineInputStoreError,
};
use crate::application::ports::PipelineStoreError;
use restream_domain::domain::pipeline_input::PipelineInputSource;

#[derive(Clone)]
pub struct SqlitePipelineInputStore {
    db: DeadlineDb,
}

impl SqlitePipelineInputStore {
    pub fn new(db: DeadlineDb) -> Self {
        Self { db }
    }
}

impl PipelineInputStore for SqlitePipelineInputStore {
    fn get<'a>(&'a self, pipeline_id: &'a str, input_id: &'a str) -> InputLookupFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| {
                    crate::db::with_busy_retry(|| {
                        crate::db::get_pipeline_input(pool, pipeline_id, input_id)
                    })
                })
                .await
                .map_err(map_error)
        })
    }

    fn get_by_stream_key<'a>(&'a self, stream_key: &'a str) -> InputLookupFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| {
                    crate::db::with_busy_retry(|| {
                        crate::db::get_pipeline_input_by_stream_key(pool, stream_key)
                    })
                })
                .await
                .map_err(map_error)
        })
    }

    fn list<'a>(&'a self, pipeline_id: &'a str) -> InputListFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| {
                    crate::db::with_busy_retry(|| {
                        crate::db::list_pipeline_inputs(pool, pipeline_id)
                    })
                })
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
        source: PipelineInputSource,
    ) -> InputWriteFuture<'a> {
        Box::pin(async move {
            let (id, pipeline_id, label, stream_key) = (
                id.to_owned(),
                pipeline_id.to_owned(),
                label.to_owned(),
                stream_key.to_owned(),
            );
            self.db
                .write(move |pool| async move {
                    crate::db::with_busy_retry(|| {
                        crate::db::create_pipeline_input(
                            &pool,
                            &id,
                            &pipeline_id,
                            &label,
                            &stream_key,
                            &source,
                        )
                    })
                    .await
                })
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
        source: PipelineInputSource,
    ) -> InputUpdateFuture<'a> {
        Box::pin(async move {
            let (pipeline_id, input_id, label) = (
                pipeline_id.to_owned(),
                input_id.to_owned(),
                label.to_owned(),
            );
            self.db
                .write(move |pool| async move {
                    crate::db::with_busy_retry(|| {
                        crate::db::update_pipeline_input(
                            &pool,
                            &pipeline_id,
                            &input_id,
                            &label,
                            enabled,
                            &source,
                        )
                    })
                    .await
                })
                .await
                .map_err(map_error)
        })
    }

    fn delete<'a>(&'a self, pipeline_id: &'a str, input_id: &'a str) -> InputDeleteFuture<'a> {
        Box::pin(async move {
            let (pipeline_id, input_id) = (pipeline_id.to_owned(), input_id.to_owned());
            self.db
                .write(move |pool| async move {
                    crate::db::with_busy_retry(|| {
                        crate::db::delete_pipeline_input(&pool, &pipeline_id, &input_id)
                    })
                    .await
                })
                .await
                .map_err(map_error)
        })
    }

    fn promote<'a>(&'a self, pipeline_id: &'a str, input_id: &'a str) -> InputUpdateFuture<'a> {
        Box::pin(async move {
            let (pipeline_id, input_id) = (pipeline_id.to_owned(), input_id.to_owned());
            self.db
                .write(move |pool| async move {
                    crate::db::with_busy_retry(|| {
                        crate::db::promote_pipeline_input(&pool, &pipeline_id, &input_id)
                    })
                    .await
                })
                .await
                .map_err(map_error)
        })
    }
}

impl PipelineInputLookup for SqlitePipelineInputStore {
    fn get_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineInputLookupFuture<'a> {
        Box::pin(async move {
            self.db
                .read(|pool| {
                    crate::db::with_busy_retry(|| {
                        crate::db::get_pipeline_input_by_stream_key(pool, stream_key)
                    })
                })
                .await
                .map_err(|error| PipelineStoreError::new(error.to_string()))
        })
    }
}

/// The ingest admission index's source: every input row, read through the
/// shared loops' deadline executor, so a reload (a catalog write's, the
/// periodic one, a retry) answers within its deadline and fails over to the
/// retry task instead of holding the reload lock behind a stalled pool.
#[derive(Clone)]
pub struct SqliteCatalogSource {
    db: crate::db::DeadlineDb,
}

impl SqliteCatalogSource {
    pub fn new(db: crate::db::DeadlineDb) -> Self {
        Self { db }
    }
}

impl CatalogAuthSource for SqliteCatalogSource {
    fn load_inputs(&self) -> CatalogAuthLoadFuture<'_> {
        Box::pin(async move {
            let rows = self
                .db
                .read(|pool| {
                    crate::db::with_busy_retry(|| {
                        crate::db::list_pipeline_inputs_for_admission(pool)
                    })
                })
                .await
                .map_err(|error| PipelineStoreError::new(error.to_string()))?;
            Ok(rows
                .into_iter()
                .map(|row| {
                    row.map_err(|row| UnreadableInput {
                        input_id: row.id,
                        stream_key: row.stream_key,
                        enabled: row.enabled,
                        error: row.error.to_string(),
                    })
                })
                .collect())
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
