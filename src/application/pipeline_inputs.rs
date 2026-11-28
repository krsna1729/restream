use std::sync::Arc;

use crate::application::catalog_auth::CatalogAuth;
use crate::application::service_error::{ServiceError, ServiceResult};
use crate::application::services::PipelineService;
use crate::db::{self, DeadlineDb};
use restream_domain::domain::pipeline_input::{
    PipelineInput, PipelineInputRole, PipelineInputSource,
};

pub const MAX_PIPELINE_INPUTS: usize = 4;

/// Pipeline input CRUD. Each write commits through the admission index
/// ([`CatalogAuth::commit`]), the same one `pipelines` writes through.
#[derive(Clone)]
pub struct PipelineInputService {
    db: DeadlineDb,
    pipelines: PipelineService,
    catalog_auth: Arc<CatalogAuth>,
}

impl PipelineInputService {
    pub fn new(db: DeadlineDb, pipelines: PipelineService, catalog_auth: Arc<CatalogAuth>) -> Self {
        Self {
            db,
            pipelines,
            catalog_auth,
        }
    }

    pub async fn list(&self, pipeline_id: &str) -> ServiceResult<Vec<PipelineInput>> {
        self.pipelines.get_by_id(pipeline_id).await?;
        self.db
            .read(|pool| db::with_busy_retry(|| db::list_pipeline_inputs(pool, pipeline_id)))
            .await
            .map_err(|error| ServiceError::internal(format!("list pipeline inputs: {error}")))
    }

    pub async fn get_by_stream_key(
        &self,
        stream_key: &str,
    ) -> ServiceResult<Option<PipelineInput>> {
        read_pipeline_input_by_stream_key(&self.db, stream_key)
            .await
            .map(|input| input.filter(|candidate| candidate.enabled))
            .map_err(|error| ServiceError::internal(format!("get pipeline input: {error}")))
    }

    pub async fn create(
        &self,
        pipeline_id: &str,
        label: &str,
        source: PipelineInputSource,
    ) -> ServiceResult<PipelineInput> {
        let existing = self.list(pipeline_id).await?;
        if existing.len() >= MAX_PIPELINE_INPUTS {
            return Err(ServiceError::conflict(format!(
                "pipeline input limit is {MAX_PIPELINE_INPUTS}"
            )));
        }
        for _ in 0..16 {
            let input_id = format!("input_{}", hex(&rand::random::<[u8; 8]>()));
            let stream_key = generated_stream_key();
            let db = self.db.clone();
            let (pipeline_id, label) = (pipeline_id.to_owned(), label.to_owned());
            let source = source.clone();
            match self
                .catalog_auth
                .commit(async move {
                    db.write(move |pool| async move {
                        db::with_busy_retry(|| {
                            db::create_pipeline_input(
                                &pool,
                                &input_id,
                                &pipeline_id,
                                &label,
                                &stream_key,
                                &source,
                            )
                        })
                        .await
                    })
                    .await
                })
                .await
            {
                Ok(input) => return Ok(input),
                Err(error) if is_input_limit(&error) => {
                    return Err(ServiceError::conflict(format!(
                        "pipeline input limit is {MAX_PIPELINE_INPUTS}"
                    )));
                }
                Err(error) if is_unique_violation(&error) => continue,
                Err(error) => {
                    return Err(ServiceError::internal(format!(
                        "create pipeline input: {error}"
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
        source: PipelineInputSource,
    ) -> ServiceResult<PipelineInput> {
        let current = self.get(pipeline_id, input_id).await?;
        if !enabled && (current.role == PipelineInputRole::Primary || current.selected) {
            return Err(ServiceError::conflict(
                "primary or selected inputs cannot be disabled",
            ));
        }
        let db = self.db.clone();
        let owned = (
            pipeline_id.to_owned(),
            input_id.to_owned(),
            label.to_owned(),
        );
        self.catalog_auth
            .commit(async move {
                db.write(move |pool| async move {
                    let (pipeline_id, input_id, label) = owned;
                    db::with_busy_retry(|| {
                        db::update_pipeline_input(
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
            })
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
        let db = self.db.clone();
        let owned = (pipeline_id.to_owned(), input_id.to_owned());
        self.catalog_auth
            .commit(async move {
                db.write(move |pool| async move {
                    db::with_busy_retry(|| db::delete_pipeline_input(&pool, &owned.0, &owned.1))
                        .await
                })
                .await
            })
            .await
            .map_err(store_error)
    }

    pub async fn promote(&self, pipeline_id: &str, input_id: &str) -> ServiceResult<PipelineInput> {
        let current = self.get(pipeline_id, input_id).await?;
        if !current.enabled {
            return Err(ServiceError::conflict("disabled inputs cannot be promoted"));
        }
        let db = self.db.clone();
        let owned = (pipeline_id.to_owned(), input_id.to_owned());
        self.catalog_auth
            .commit(async move {
                db.write(move |pool| async move {
                    db::with_busy_retry(|| db::promote_pipeline_input(&pool, &owned.0, &owned.1))
                        .await
                })
                .await
            })
            .await
            .map_err(store_error)?
            .ok_or_else(|| input_not_found(input_id))
    }

    pub async fn get(&self, pipeline_id: &str, input_id: &str) -> ServiceResult<PipelineInput> {
        self.db
            .read(|pool| {
                db::with_busy_retry(|| db::get_pipeline_input(pool, pipeline_id, input_id))
            })
            .await
            .map_err(store_error)?
            .ok_or_else(|| input_not_found(input_id))
    }
}

/// Resolution without an enabled check, for callers that own eligibility.
async fn read_pipeline_input_by_stream_key(
    db: &DeadlineDb,
    stream_key: &str,
) -> Result<Option<PipelineInput>, sqlx::Error> {
    db.read(|pool| db::with_busy_retry(|| db::get_pipeline_input_by_stream_key(pool, stream_key)))
        .await
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

fn is_input_limit(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|error| error.message().contains("pipeline input limit exceeded"))
}

fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|error| error.is_unique_violation())
}

fn store_error(error: sqlx::Error) -> ServiceError {
    if is_unique_violation(&error) || is_input_limit(&error) {
        ServiceError::conflict(error.to_string())
    } else {
        // Preserve the raw failure message when the bounded SQLite retry fails.
        ServiceError::internal(error.to_string())
    }
}
