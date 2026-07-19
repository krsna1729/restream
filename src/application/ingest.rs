//! Application-layer ingest coordination that resolves pipelines, loads
//! file-ingest context, and validates stream access before media processing begins.

use crate::application::models::{Ingest, Pipeline};
use crate::application::ports::{
    IngestLookup, IngestLookupError, IngestWriteError, IngestWriter, PipelineStore,
    PipelineStoreError,
};
use crate::domain::pipeline_input::PipelineInput;
use crate::media::engine::MediaEngine;
use crate::media::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessError, PipelineAccessFuture,
    PipelineAccessMode,
};
use crate::media::security::{IngestSecurityService, RateLimitScope};
use std::sync::Arc;
use std::{future::Future, pin::Pin};

pub type PipelineInputLookupFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Option<PipelineInput>, PipelineStoreError>> + Send + 'a>>;

pub trait PipelineInputLookup: Send + Sync {
    fn get_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineInputLookupFuture<'a>;
}

#[derive(Debug)]
pub enum IngestAuthError {
    InvalidStreamKey,
    LookupFailed(PipelineStoreError),
}

pub struct PipelineStoreIngestAuthenticator {
    pipeline_lookup: Arc<dyn PipelineStore>,
    input_lookup: Arc<dyn PipelineInputLookup>,
    security: Arc<IngestSecurityService>,
}

impl PipelineStoreIngestAuthenticator {
    pub fn new(
        pipeline_lookup: Arc<dyn PipelineStore>,
        input_lookup: Arc<dyn PipelineInputLookup>,
        security: Arc<IngestSecurityService>,
    ) -> Self {
        Self {
            pipeline_lookup,
            input_lookup,
            security,
        }
    }
}

impl PipelineAccessAuthenticator for PipelineStoreIngestAuthenticator {
    fn authenticate<'a>(
        &'a self,
        mode: PipelineAccessMode,
        stream_key: &'a str,
        client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            let pipeline = match mode {
                PipelineAccessMode::RtmpPublish => authenticate_publish_stream_key(
                    self.pipeline_lookup.as_ref(),
                    &self.security,
                    stream_key,
                    client_ip,
                )
                .await
                .map_err(pipeline_access_error)?,
                PipelineAccessMode::RtmpPlay => self
                    .pipeline_lookup
                    .get_pipeline_by_stream_key(stream_key)
                    .await
                    .map_err(|err| PipelineAccessError::LookupFailed(err.to_string()))?
                    .ok_or(PipelineAccessError::InvalidStreamKey)?,
                PipelineAccessMode::SrtPublish => authenticate_srt_stream_key(
                    self.pipeline_lookup.as_ref(),
                    &self.security,
                    stream_key,
                    client_ip,
                    RateLimitScope::SrtPublish,
                )
                .await
                .map_err(pipeline_access_error)?,
                PipelineAccessMode::SrtRead => authenticate_srt_stream_key(
                    self.pipeline_lookup.as_ref(),
                    &self.security,
                    stream_key,
                    client_ip,
                    RateLimitScope::SrtRead,
                )
                .await
                .map_err(pipeline_access_error)?,
            };

            let input = self
                .input_lookup
                .get_by_stream_key(stream_key)
                .await
                .map_err(|error| PipelineAccessError::LookupFailed(error.to_string()))?
                .filter(|input| input.pipeline_id == pipeline.id && input.enabled)
                .ok_or(PipelineAccessError::InvalidStreamKey)?;

            Ok(AuthenticatedPipeline {
                id: pipeline.id,
                input_id: input.id,
                selected: input.selected,
            })
        })
    }
}

fn pipeline_access_error(error: IngestAuthError) -> PipelineAccessError {
    match error {
        IngestAuthError::InvalidStreamKey => PipelineAccessError::InvalidStreamKey,
        IngestAuthError::LookupFailed(error) => {
            PipelineAccessError::LookupFailed(error.to_string())
        }
    }
}

#[derive(Debug)]
pub struct FileIngestContext {
    pub ingest: Ingest,
    pub pipeline: Pipeline,
}

#[derive(Debug)]
pub struct PipelineFileIngestState {
    pub ingest: Option<Ingest>,
    pub running: bool,
}

#[derive(Debug)]
pub enum ClearFileIngestsError {
    PipelineStore(PipelineStoreError),
    IngestLookup(IngestLookupError),
}

#[derive(Debug)]
pub enum ResolveFileIngestError {
    IngestLookup(IngestLookupError),
    PipelineStore(PipelineStoreError),
    MissingPipelineForStreamKey(String),
}

#[derive(Debug, Clone)]
pub struct FileIngestConfig {
    pub filename: String,
    pub loop_flag: bool,
    pub start_time: String,
    pub live_optimized: bool,
    pub target_gop_seconds: u32,
}

#[derive(Debug)]
pub enum PersistFileIngestError {
    IngestLookup(IngestLookupError),
    IngestWrite(IngestWriteError),
    PipelineStore(PipelineStoreError),
}

pub async fn resolve_file_ingest_context(
    ingest_lookup: &dyn IngestLookup,
    pipeline_lookup: &dyn PipelineStore,
    ingest_id: &str,
) -> Result<Option<FileIngestContext>, ResolveFileIngestError> {
    let Some(ingest) = ingest_lookup
        .get_ingest(ingest_id)
        .await
        .map_err(ResolveFileIngestError::IngestLookup)?
    else {
        return Ok(None);
    };

    let pipeline = pipeline_lookup
        .get_pipeline_by_stream_key(&ingest.stream_key)
        .await
        .map_err(ResolveFileIngestError::PipelineStore)?
        .ok_or_else(|| {
            ResolveFileIngestError::MissingPipelineForStreamKey(ingest.stream_key.clone())
        })?;

    Ok(Some(FileIngestContext { ingest, pipeline }))
}

pub async fn load_pipeline_file_ingest_state(
    ingest_lookup: &dyn IngestLookup,
    engine: &MediaEngine,
    pipeline: &Pipeline,
) -> Result<PipelineFileIngestState, IngestLookupError> {
    let ingest = ingest_lookup
        .get_ingest_by_stream_key(&pipeline.stream_key)
        .await?;
    let running = match ingest.as_ref() {
        Some(ingest) => engine.is_file_ingest_running(&ingest.id).await,
        None => false,
    };

    Ok(PipelineFileIngestState { ingest, running })
}

pub async fn clear_stream_key_file_ingests(
    pipeline_lookup: &dyn PipelineStore,
    ingest_lookup: &dyn IngestLookup,
    engine: &MediaEngine,
    stream_key: &str,
) -> Result<(), ClearFileIngestsError> {
    if let Some(pipeline) = pipeline_lookup
        .get_pipeline_by_stream_key(stream_key)
        .await
        .map_err(ClearFileIngestsError::PipelineStore)?
    {
        engine.unregister_ingest(&pipeline.id).await;
    }

    let ingests = ingest_lookup
        .list_ingests_for_stream_key(stream_key)
        .await
        .map_err(ClearFileIngestsError::IngestLookup)?;
    for ingest in ingests {
        let _ = engine.stop_file_ingest_child(&ingest.id).await;
        engine.clear_file_ingest_running(&ingest.id).await;
    }

    Ok(())
}

pub async fn persist_pipeline_file_ingest(
    ingest_lookup: &dyn IngestLookup,
    ingest_writer: &dyn IngestWriter,
    pipeline_store: &dyn PipelineStore,
    pipeline: &Pipeline,
    config: &FileIngestConfig,
    id_factory: impl FnOnce() -> String,
) -> Result<Ingest, PersistFileIngestError> {
    let existing = ingest_lookup
        .get_ingest_by_stream_key(&pipeline.stream_key)
        .await
        .map_err(PersistFileIngestError::IngestLookup)?;

    let saved = match existing {
        Some(ingest) => ingest_writer
            .update_ingest(
                &ingest.id,
                &config.filename,
                &pipeline.stream_key,
                config.loop_flag,
                &config.start_time,
                config.live_optimized,
                config.target_gop_seconds,
            )
            .await
            .map_err(PersistFileIngestError::IngestWrite)?
            .ok_or_else(|| {
                PersistFileIngestError::IngestWrite(IngestWriteError::new("ingest not found"))
            })?,
        None => ingest_writer
            .create_ingest(
                &id_factory(),
                &config.filename,
                &pipeline.stream_key,
                config.loop_flag,
                &config.start_time,
                config.live_optimized,
                config.target_gop_seconds,
            )
            .await
            .map_err(PersistFileIngestError::IngestWrite)?,
    };

    let ingests = ingest_lookup
        .list_ingests_for_stream_key(&pipeline.stream_key)
        .await
        .map_err(PersistFileIngestError::IngestLookup)?;
    for ingest in ingests.into_iter().filter(|ingest| ingest.id != saved.id) {
        let _ = ingest_writer.delete_ingest(&ingest.id).await;
    }

    let input_source = format!("file:{}", config.filename);
    pipeline_store
        .update_pipeline_input_source(pipeline, Some(&input_source))
        .await
        .map_err(PersistFileIngestError::PipelineStore)?;

    Ok(saved)
}

pub async fn remove_pipeline_file_ingest(
    ingest_lookup: &dyn IngestLookup,
    ingest_writer: &dyn IngestWriter,
    pipeline_store: &dyn PipelineStore,
    pipeline: &Pipeline,
) -> Result<(), PersistFileIngestError> {
    let ingests = ingest_lookup
        .list_ingests_for_stream_key(&pipeline.stream_key)
        .await
        .map_err(PersistFileIngestError::IngestLookup)?;
    for ingest in ingests {
        let _ = ingest_writer.delete_ingest(&ingest.id).await;
    }

    pipeline_store
        .update_pipeline_input_source(pipeline, None)
        .await
        .map_err(PersistFileIngestError::PipelineStore)?;

    Ok(())
}

pub async fn authenticate_publish_stream_key(
    pipeline_lookup: &dyn PipelineStore,
    security: &IngestSecurityService,
    stream_key: &str,
    client_ip: &str,
) -> Result<Pipeline, IngestAuthError> {
    authenticate_stream_key_for_scope(
        pipeline_lookup,
        security,
        stream_key,
        client_ip,
        RateLimitScope::RtmpPublish,
        false,
    )
    .await
}

pub async fn authenticate_srt_stream_key(
    pipeline_lookup: &dyn PipelineStore,
    security: &IngestSecurityService,
    stream_key: &str,
    client_ip: &str,
    scope: RateLimitScope,
) -> Result<Pipeline, IngestAuthError> {
    authenticate_stream_key_for_scope(
        pipeline_lookup,
        security,
        stream_key,
        client_ip,
        scope,
        true,
    )
    .await
}

async fn authenticate_stream_key_for_scope(
    pipeline_lookup: &dyn PipelineStore,
    security: &IngestSecurityService,
    stream_key: &str,
    client_ip: &str,
    scope: RateLimitScope,
    clear_on_success: bool,
) -> Result<Pipeline, IngestAuthError> {
    match pipeline_lookup.get_pipeline_by_stream_key(stream_key).await {
        Ok(Some(pipeline)) => {
            if clear_on_success {
                security.record_success_for(scope, client_ip);
            }
            Ok(pipeline)
        }
        Ok(None) => {
            security.record_failure_for(scope, client_ip);
            Err(IngestAuthError::InvalidStreamKey)
        }
        Err(err) => {
            security.record_failure_for(scope, client_ip);
            Err(IngestAuthError::LookupFailed(err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::ports::{
        IngestCatalogFuture, IngestDeleteFuture, IngestLookupFuture, IngestUpdateFuture,
        IngestWriteFuture, PipelineLookupFuture,
    };
    use crate::domain::ingest_security::IngestSecurityConfig;
    use crate::media::security::IngestSecurityService;
    use std::collections::HashMap;
    use std::sync::Arc;

    fn test_security_config() -> IngestSecurityConfig {
        IngestSecurityConfig {
            failure_limit: 2,
            failure_window_ms: 60_000,
            ban_ms: 60_000,
            tracked_ip_limit: 100,
        }
    }

    struct FakePipelineStore {
        pipelines: HashMap<String, Pipeline>,
        error: Option<&'static str>,
    }

    impl FakePipelineStore {
        fn success(stream_key: &str) -> Self {
            let mut pipelines = HashMap::new();
            pipelines.insert(
                stream_key.to_string(),
                Pipeline {
                    id: "pipeline-1".to_string(),
                    name: "Pipeline".to_string(),
                    stream_key: stream_key.to_string(),
                    input_source: None,
                    srt_ingest_policy: None,
                },
            );
            Self {
                pipelines,
                error: None,
            }
        }
    }

    impl PipelineStore for FakePipelineStore {
        fn get_pipeline_by_stream_key<'a>(
            &'a self,
            stream_key: &'a str,
        ) -> PipelineLookupFuture<'a> {
            Box::pin(async move {
                if let Some(message) = self.error {
                    return Err(PipelineStoreError::new(message));
                }
                Ok(self.pipelines.get(stream_key).cloned())
            })
        }

        fn list_pipelines<'a>(&'a self) -> crate::application::ports::PipelineListFuture<'a> {
            Box::pin(async move { Ok(self.pipelines.values().cloned().collect()) })
        }

        fn get_pipeline<'a>(
            &'a self,
            id: &'a str,
        ) -> crate::application::ports::PipelineLookupFuture<'a> {
            Box::pin(async move { Ok(self.pipelines.values().find(|p| p.id == id).cloned()) })
        }

        fn create_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> crate::application::ports::PipelineCreateFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
        }

        fn update_pipeline<'a>(
            &'a self,
            _id: &'a str,
            _name: &'a str,
            _stream_key: &'a str,
            _input_source: Option<&'a str>,
            _srt_ingest_policy: Option<&'a str>,
        ) -> crate::application::ports::PipelineUpdateFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
        }

        fn delete_pipeline<'a>(
            &'a self,
            _id: &'a str,
        ) -> crate::application::ports::PipelineDeleteFuture<'a> {
            Box::pin(async move { Err(PipelineStoreError::new("not implemented")) })
        }

        fn get_ingest_host<'a>(
            &'a self,
        ) -> crate::application::ports::PipelineIngestHostFuture<'a> {
            Box::pin(async move { Ok(None) })
        }

        fn update_pipeline_input_source<'a>(
            &'a self,
            pipeline: &'a Pipeline,
            input_source: Option<&'a str>,
        ) -> crate::application::ports::PipelineUpdateFuture<'a> {
            Box::pin(async move {
                let mut updated = pipeline.clone();
                updated.input_source = input_source.map(ToOwned::to_owned);
                Ok(Some(updated))
            })
        }
    }

    impl PipelineInputLookup for FakePipelineStore {
        fn get_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineInputLookupFuture<'a> {
            Box::pin(async move {
                if let Some(message) = self.error {
                    return Err(PipelineStoreError::new(message));
                }
                Ok(self
                    .pipelines
                    .get(stream_key)
                    .map(|pipeline| PipelineInput {
                        id: "input-primary".to_string(),
                        pipeline_id: pipeline.id.clone(),
                        label: "Primary".to_string(),
                        stream_key: stream_key.to_string(),
                        role: crate::domain::pipeline_input::PipelineInputRole::Primary,
                        enabled: true,
                        selected: true,
                    }))
            })
        }
    }

    struct FakeIngestLookup {
        by_id: HashMap<String, Ingest>,
        by_stream_key: HashMap<String, Vec<Ingest>>,
        error: Option<&'static str>,
    }

    struct FakeIngestWriter {
        created: std::sync::Mutex<Vec<Ingest>>,
        deleted: std::sync::Mutex<Vec<String>>,
        fail: Option<&'static str>,
        update_returns_none: bool,
    }

    impl IngestWriter for FakeIngestWriter {
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
                if let Some(message) = self.fail {
                    return Err(IngestWriteError::new(message));
                }
                let ingest = Ingest {
                    id: id.to_string(),
                    filename: filename.to_string(),
                    stream_key: stream_key.to_string(),
                    loop_flag,
                    start_time: start_time.to_string(),
                    live_optimized,
                    target_gop_seconds,
                };
                self.created
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(ingest.clone());
                Ok(ingest)
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
                if self.update_returns_none {
                    return Ok(None);
                }
                self.create_ingest(
                    id,
                    filename,
                    stream_key,
                    loop_flag,
                    start_time,
                    live_optimized,
                    target_gop_seconds,
                )
                .await
                .map(Some)
            })
        }

        fn delete_ingest<'a>(&'a self, id: &'a str) -> IngestDeleteFuture<'a> {
            Box::pin(async move {
                if let Some(message) = self.fail {
                    return Err(IngestWriteError::new(message));
                }
                self.deleted
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .push(id.to_string());
                Ok(true)
            })
        }
    }

    impl FakeIngestLookup {
        fn ingest(id: &str, stream_key: &str) -> Ingest {
            Ingest {
                id: id.to_string(),
                filename: "clip.mp4".to_string(),
                stream_key: stream_key.to_string(),
                loop_flag: true,
                start_time: "00:00:05".to_string(),
                live_optimized: true,
                target_gop_seconds: 4,
            }
        }
    }

    impl IngestLookup for FakeIngestLookup {
        fn get_ingest<'a>(&'a self, id: &'a str) -> IngestLookupFuture<'a> {
            Box::pin(async move {
                if let Some(message) = self.error {
                    return Err(IngestLookupError::new(message));
                }
                Ok(self.by_id.get(id).cloned())
            })
        }

        fn get_ingest_by_stream_key<'a>(&'a self, stream_key: &'a str) -> IngestLookupFuture<'a> {
            Box::pin(async move {
                if let Some(message) = self.error {
                    return Err(IngestLookupError::new(message));
                }
                Ok(self
                    .by_stream_key
                    .get(stream_key)
                    .and_then(|ingests| ingests.last().cloned()))
            })
        }

        fn list_ingests<'a>(&'a self) -> IngestCatalogFuture<'a> {
            Box::pin(async move {
                if let Some(message) = self.error {
                    return Err(IngestLookupError::new(message));
                }
                Ok(self.by_id.values().cloned().collect())
            })
        }

        fn list_ingests_for_filename<'a>(&'a self, filename: &'a str) -> IngestCatalogFuture<'a> {
            Box::pin(async move {
                if let Some(message) = self.error {
                    return Err(IngestLookupError::new(message));
                }
                Ok(self
                    .by_id
                    .values()
                    .filter(|ingest| ingest.filename == filename)
                    .cloned()
                    .collect())
            })
        }

        fn list_ingests_for_stream_key<'a>(
            &'a self,
            stream_key: &'a str,
        ) -> IngestCatalogFuture<'a> {
            Box::pin(async move {
                if let Some(message) = self.error {
                    return Err(IngestLookupError::new(message));
                }
                Ok(self
                    .by_stream_key
                    .get(stream_key)
                    .cloned()
                    .unwrap_or_default())
            })
        }
    }

    #[tokio::test]
    async fn publish_auth_records_failure_for_missing_stream_key() {
        let lookup = FakePipelineStore {
            pipelines: HashMap::new(),
            error: None,
        };
        let security = IngestSecurityService::new(test_security_config());

        let result =
            authenticate_publish_stream_key(&lookup, &security, "missing", "10.0.0.1").await;

        assert!(matches!(result, Err(IngestAuthError::InvalidStreamKey)));
        assert!(security.is_ip_banned("10.0.0.1").is_none());
        assert!(security.record_failure("10.0.0.1"));
    }

    #[tokio::test]
    async fn publish_auth_returns_pipeline_on_success() {
        let lookup = FakePipelineStore::success("live");
        let security = IngestSecurityService::new(test_security_config());

        let pipeline = authenticate_publish_stream_key(&lookup, &security, "live", "10.0.0.1")
            .await
            .unwrap();

        assert_eq!(pipeline.id, "pipeline-1");
    }

    #[tokio::test]
    async fn publish_auth_surfaces_lookup_error_and_records_failure() {
        let lookup = FakePipelineStore {
            pipelines: HashMap::new(),
            error: Some("db unavailable"),
        };
        let security = IngestSecurityService::new(test_security_config());
        let ip = "10.0.0.3";

        let result = authenticate_publish_stream_key(&lookup, &security, "live", ip).await;

        assert!(matches!(result, Err(IngestAuthError::LookupFailed(_))));
        assert!(security.is_ip_banned(ip).is_none());
        assert!(security.record_failure(ip));
    }

    #[tokio::test]
    async fn publish_auth_success_does_not_clear_prior_failure_state() {
        let lookup = FakePipelineStore::success("live");
        let security = IngestSecurityService::new(test_security_config());
        let ip = "10.0.0.9";

        assert!(!security.record_failure_for(RateLimitScope::RtmpPublish, ip));

        let pipeline = authenticate_publish_stream_key(&lookup, &security, "live", ip)
            .await
            .unwrap();
        assert_eq!(pipeline.id, "pipeline-1");

        // Unlike SRT auth, a successful RTMP publish auth does not clear the
        // prior failure count: one more failure should still trip the ban.
        assert!(security.record_failure_for(RateLimitScope::RtmpPublish, ip));
        assert!(
            security
                .is_ip_banned_for(RateLimitScope::RtmpPublish, ip)
                .is_some()
        );
    }

    #[tokio::test]
    async fn srt_auth_clears_failure_state_after_success() {
        let lookup = FakePipelineStore::success("live");
        let security = Arc::new(IngestSecurityService::new(test_security_config()));
        let ip = "10.0.0.2";

        assert!(!security.record_failure_for(RateLimitScope::SrtPublish, ip));
        assert!(security.record_failure_for(RateLimitScope::SrtPublish, ip));
        assert!(
            security
                .is_ip_banned_for(RateLimitScope::SrtPublish, ip)
                .is_some()
        );

        let pipeline =
            authenticate_srt_stream_key(&lookup, &security, "live", ip, RateLimitScope::SrtPublish)
                .await
                .unwrap();

        assert_eq!(pipeline.id, "pipeline-1");
        assert!(
            security
                .is_ip_banned_for(RateLimitScope::SrtPublish, ip)
                .is_none()
        );
    }

    #[tokio::test]
    async fn pipeline_access_authenticator_resolves_rtmp_play_without_rate_limit_side_effects() {
        let lookup = Arc::new(FakePipelineStore::success("live"));
        let security = Arc::new(IngestSecurityService::new(test_security_config()));
        let auth = PipelineStoreIngestAuthenticator::new(lookup.clone(), lookup, security.clone());

        let pipeline = auth
            .authenticate(PipelineAccessMode::RtmpPlay, "live", "10.0.0.4")
            .await
            .unwrap();

        assert_eq!(pipeline.id, "pipeline-1");
        assert!(security.snapshots().is_empty());
    }

    #[tokio::test]
    async fn pipeline_access_authenticator_records_srt_publish_failures() {
        let lookup = Arc::new(FakePipelineStore {
            pipelines: HashMap::new(),
            error: None,
        });
        let security = Arc::new(IngestSecurityService::new(test_security_config()));
        let auth = PipelineStoreIngestAuthenticator::new(lookup.clone(), lookup, security.clone());
        let ip = "10.0.0.5";

        let result = auth
            .authenticate(PipelineAccessMode::SrtPublish, "missing", ip)
            .await;

        assert!(matches!(result, Err(PipelineAccessError::InvalidStreamKey)));
        let snapshots = security.snapshots();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].scope, "srt-publish");
        assert_eq!(snapshots[0].ip, ip);
        assert_eq!(snapshots[0].failure_count, 1);
    }

    #[tokio::test]
    async fn resolve_file_ingest_context_returns_none_for_missing_ingest() {
        let ingest_lookup = FakeIngestLookup {
            by_id: HashMap::new(),
            by_stream_key: HashMap::new(),
            error: None,
        };
        let pipeline_lookup = FakePipelineStore {
            pipelines: HashMap::new(),
            error: None,
        };

        let result = resolve_file_ingest_context(&ingest_lookup, &pipeline_lookup, "missing")
            .await
            .unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn resolve_file_ingest_context_surfaces_missing_pipeline() {
        let ingest = FakeIngestLookup::ingest("ingest-1", "stream-key");
        let ingest_lookup = FakeIngestLookup {
            by_id: HashMap::from([(ingest.id.clone(), ingest.clone())]),
            by_stream_key: HashMap::from([("stream-key".to_string(), vec![ingest])]),
            error: None,
        };
        let pipeline_lookup = FakePipelineStore {
            pipelines: HashMap::new(),
            error: None,
        };

        let result =
            resolve_file_ingest_context(&ingest_lookup, &pipeline_lookup, "ingest-1").await;

        assert!(matches!(
            result,
            Err(ResolveFileIngestError::MissingPipelineForStreamKey(stream_key))
                if stream_key == "stream-key"
        ));
    }

    #[tokio::test]
    async fn resolve_file_ingest_context_returns_ingest_and_pipeline() {
        let ingest = FakeIngestLookup::ingest("ingest-1", "stream-key");
        let pipeline = Pipeline {
            id: "pipeline-1".to_string(),
            name: "Pipeline".to_string(),
            stream_key: "stream-key".to_string(),
            input_source: None,
            srt_ingest_policy: None,
        };
        let ingest_lookup = FakeIngestLookup {
            by_id: HashMap::from([(ingest.id.clone(), ingest.clone())]),
            by_stream_key: HashMap::from([("stream-key".to_string(), vec![ingest.clone()])]),
            error: None,
        };
        let pipeline_lookup = FakePipelineStore {
            pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
            error: None,
        };

        let result = resolve_file_ingest_context(&ingest_lookup, &pipeline_lookup, "ingest-1")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(result.ingest.id, "ingest-1");
        assert_eq!(result.pipeline.id, "pipeline-1");
    }

    #[tokio::test]
    async fn load_pipeline_file_ingest_state_returns_latest_ingest_and_running_flag() {
        let ingest = FakeIngestLookup::ingest("ingest-1", "stream-key");
        let lookup = FakeIngestLookup {
            by_id: HashMap::new(),
            by_stream_key: HashMap::from([("stream-key".to_string(), vec![ingest.clone()])]),
            error: None,
        };
        let engine = MediaEngine::new();
        engine.mark_file_ingest_running(&ingest.id).await;
        let pipeline = Pipeline {
            id: "pipeline-1".to_string(),
            name: "Pipeline".to_string(),
            stream_key: "stream-key".to_string(),
            input_source: None,
            srt_ingest_policy: None,
        };

        let state = load_pipeline_file_ingest_state(&lookup, &engine, &pipeline)
            .await
            .unwrap();

        assert_eq!(state.ingest.unwrap().id, "ingest-1");
        assert!(state.running);
    }

    #[tokio::test]
    async fn clear_stream_key_file_ingests_unregisters_pipeline_and_clears_running_state() {
        let ingest = FakeIngestLookup::ingest("ingest-1", "stream-key");
        let pipeline = Pipeline {
            id: "pipeline-1".to_string(),
            name: "Pipeline".to_string(),
            stream_key: "stream-key".to_string(),
            input_source: None,
            srt_ingest_policy: None,
        };
        let ingest_lookup = FakeIngestLookup {
            by_id: HashMap::new(),
            by_stream_key: HashMap::from([("stream-key".to_string(), vec![ingest.clone()])]),
            error: None,
        };
        let pipeline_lookup = FakePipelineStore {
            pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
            error: None,
        };
        let engine = MediaEngine::new();
        let _registration = engine
            .try_register_ingest_attempt(&pipeline.id, &pipeline.stream_key, "file")
            .await
            .expect("pipeline should register");
        engine.mark_file_ingest_running(&ingest.id).await;

        clear_stream_key_file_ingests(
            &pipeline_lookup,
            &ingest_lookup,
            &engine,
            &pipeline.stream_key,
        )
        .await
        .unwrap();

        assert!(!engine.has_active_ingest(&pipeline.id).await);
        assert!(!engine.is_file_ingest_running(&ingest.id).await);
    }

    #[tokio::test]
    async fn persist_pipeline_file_ingest_updates_pipeline_and_deletes_stale_ingests() {
        let current = FakeIngestLookup::ingest("ingest-current", "stream-key");
        let stale = FakeIngestLookup::ingest("ingest-stale", "stream-key");
        let pipeline = Pipeline {
            id: "pipeline-1".to_string(),
            name: "Pipeline".to_string(),
            stream_key: "stream-key".to_string(),
            input_source: None,
            srt_ingest_policy: None,
        };
        let ingest_lookup = FakeIngestLookup {
            by_id: HashMap::new(),
            by_stream_key: HashMap::from([(
                "stream-key".to_string(),
                vec![stale.clone(), current.clone()],
            )]),
            error: None,
        };
        let pipeline_store = FakePipelineStore {
            pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
            error: None,
        };
        let ingest_writer = FakeIngestWriter {
            created: std::sync::Mutex::new(Vec::new()),
            deleted: std::sync::Mutex::new(Vec::new()),
            fail: None,
            update_returns_none: false,
        };

        let saved = persist_pipeline_file_ingest(
            &ingest_lookup,
            &ingest_writer,
            &pipeline_store,
            &pipeline,
            &FileIngestConfig {
                filename: "updated.mp4".to_string(),
                loop_flag: false,
                start_time: "00:00:10".to_string(),
                live_optimized: false,
                target_gop_seconds: 2,
            },
            || "generated".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(saved.id, "ingest-current");
        assert_eq!(saved.filename, "updated.mp4");
        let deleted = ingest_writer
            .deleted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(deleted, vec!["ingest-stale".to_string()]);
    }

    #[tokio::test]
    async fn resolve_file_ingest_context_surfaces_ingest_lookup_error() {
        let ingest_lookup = FakeIngestLookup {
            by_id: HashMap::new(),
            by_stream_key: HashMap::new(),
            error: Some("db unavailable"),
        };
        let pipeline_lookup = FakePipelineStore {
            pipelines: HashMap::new(),
            error: None,
        };

        let result =
            resolve_file_ingest_context(&ingest_lookup, &pipeline_lookup, "ingest-1").await;

        assert!(matches!(
            result,
            Err(ResolveFileIngestError::IngestLookup(_))
        ));
    }

    #[tokio::test]
    async fn persist_pipeline_file_ingest_creates_new_ingest_when_none_exists() {
        let pipeline = Pipeline {
            id: "pipeline-1".to_string(),
            name: "Pipeline".to_string(),
            stream_key: "stream-key".to_string(),
            input_source: None,
            srt_ingest_policy: None,
        };
        let ingest_lookup = FakeIngestLookup {
            by_id: HashMap::new(),
            by_stream_key: HashMap::new(),
            error: None,
        };
        let pipeline_store = FakePipelineStore {
            pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
            error: None,
        };
        let ingest_writer = FakeIngestWriter {
            created: std::sync::Mutex::new(Vec::new()),
            deleted: std::sync::Mutex::new(Vec::new()),
            fail: None,
            update_returns_none: false,
        };

        let saved = persist_pipeline_file_ingest(
            &ingest_lookup,
            &ingest_writer,
            &pipeline_store,
            &pipeline,
            &FileIngestConfig {
                filename: "new.mp4".to_string(),
                loop_flag: true,
                start_time: "00:00:00".to_string(),
                live_optimized: true,
                target_gop_seconds: 4,
            },
            || "generated-id".to_string(),
        )
        .await
        .unwrap();

        assert_eq!(saved.id, "generated-id");
        assert_eq!(saved.filename, "new.mp4");
        let created = ingest_writer
            .created
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert_eq!(created.len(), 1);
        assert_eq!(created[0].id, "generated-id");
        let deleted = ingest_writer
            .deleted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        assert!(deleted.is_empty());
    }

    #[tokio::test]
    async fn persist_pipeline_file_ingest_surfaces_race_when_update_target_disappears() {
        let existing = FakeIngestLookup::ingest("ingest-current", "stream-key");
        let pipeline = Pipeline {
            id: "pipeline-1".to_string(),
            name: "Pipeline".to_string(),
            stream_key: "stream-key".to_string(),
            input_source: None,
            srt_ingest_policy: None,
        };
        let ingest_lookup = FakeIngestLookup {
            by_id: HashMap::new(),
            by_stream_key: HashMap::from([("stream-key".to_string(), vec![existing])]),
            error: None,
        };
        let pipeline_store = FakePipelineStore {
            pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
            error: None,
        };
        let ingest_writer = FakeIngestWriter {
            created: std::sync::Mutex::new(Vec::new()),
            deleted: std::sync::Mutex::new(Vec::new()),
            fail: None,
            update_returns_none: true,
        };

        let result = persist_pipeline_file_ingest(
            &ingest_lookup,
            &ingest_writer,
            &pipeline_store,
            &pipeline,
            &FileIngestConfig {
                filename: "updated.mp4".to_string(),
                loop_flag: false,
                start_time: "00:00:10".to_string(),
                live_optimized: false,
                target_gop_seconds: 2,
            },
            || "generated".to_string(),
        )
        .await;

        assert!(matches!(
            result,
            Err(PersistFileIngestError::IngestWrite(_))
        ));
    }

    #[tokio::test]
    async fn remove_pipeline_file_ingest_deletes_all_ingests_and_clears_input_source() {
        let first = FakeIngestLookup::ingest("ingest-1", "stream-key");
        let second = FakeIngestLookup::ingest("ingest-2", "stream-key");
        let pipeline = Pipeline {
            id: "pipeline-1".to_string(),
            name: "Pipeline".to_string(),
            stream_key: "stream-key".to_string(),
            input_source: Some("file:clip.mp4".to_string()),
            srt_ingest_policy: None,
        };
        let ingest_lookup = FakeIngestLookup {
            by_id: HashMap::new(),
            by_stream_key: HashMap::from([(
                "stream-key".to_string(),
                vec![first.clone(), second.clone()],
            )]),
            error: None,
        };
        let pipeline_store = FakePipelineStore {
            pipelines: HashMap::from([("stream-key".to_string(), pipeline.clone())]),
            error: None,
        };
        let ingest_writer = FakeIngestWriter {
            created: std::sync::Mutex::new(Vec::new()),
            deleted: std::sync::Mutex::new(Vec::new()),
            fail: None,
            update_returns_none: false,
        };

        remove_pipeline_file_ingest(&ingest_lookup, &ingest_writer, &pipeline_store, &pipeline)
            .await
            .unwrap();

        let mut deleted = ingest_writer
            .deleted
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        deleted.sort();
        assert_eq!(
            deleted,
            vec!["ingest-1".to_string(), "ingest-2".to_string()]
        );
    }
}
