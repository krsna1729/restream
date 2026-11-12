use super::*;
use crate::application::ports::{
    PipelineCreateFuture, PipelineDeleteFuture, PipelineIngestHostFuture, PipelineListFuture,
    PipelineLookupFuture, PipelineUpdateFuture,
};
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_domain::models::Pipeline;
use restream_engine::security::IngestSecurityService;

/// A pipeline catalog whose every read panics, standing in for any panic in
/// a reconciler step (`resume_unwind` skips the panic hook: quiet logs).
struct PanickingCatalog;

fn panics<T: Send + 'static>() -> std::pin::Pin<Box<dyn std::future::Future<Output = T> + Send>> {
    Box::pin(async { std::panic::resume_unwind(Box::new("injected pipeline catalog panic")) })
}

impl PipelineStore for PanickingCatalog {
    fn get_pipeline<'a>(&'a self, _: &'a str) -> PipelineLookupFuture<'a> {
        panics()
    }
    fn get_pipeline_by_stream_key<'a>(&'a self, _: &'a str) -> PipelineLookupFuture<'a> {
        panics()
    }
    fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a> {
        panics()
    }
    fn create_pipeline<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        _: Option<&'a str>,
        _: Option<&'a str>,
    ) -> PipelineCreateFuture<'a> {
        panics()
    }
    fn update_pipeline<'a>(
        &'a self,
        _: &'a str,
        _: &'a str,
        _: &'a str,
        _: Option<&'a str>,
        _: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a> {
        panics()
    }
    fn delete_pipeline<'a>(&'a self, _: &'a str) -> PipelineDeleteFuture<'a> {
        panics()
    }
    fn get_ingest_host<'a>(&'a self) -> PipelineIngestHostFuture<'a> {
        panics()
    }
    fn update_pipeline_input_source<'a>(
        &'a self,
        _: &'a Pipeline,
        _: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a> {
        panics()
    }
}

async fn reconciler(catalog: Arc<dyn PipelineStore>) -> Reconciler {
    let config = Arc::new(AppConfig::default());
    let pool = crate::db::create_pool("sqlite::memory:").await.unwrap();
    crate::db::setup_database_schema(&pool).await.unwrap();
    let engine = Arc::new(MediaEngine::new());
    let security = Arc::new(IngestSecurityService::new(IngestSecurityConfig::default()));
    let hls_ingest = restream_engine::hls_ingest::HlsIngest::new(
        engine.clone(),
        Arc::new(
            crate::application::catalog_auth::CatalogIngestAuthenticator::new(
                crate::infrastructure::service_wiring::SqliteServiceFactory::new(&pool)
                    .catalog_auth(),
                security.clone(),
            ),
        ),
        security,
        restream_engine::hls_ingest::HlsIngestLimits::default(),
    );
    let runtime_state = Arc::new(crate::application::runtime_state::hub::StateHub::new(
        config.ports,
        config.tuning.output_max_retries,
    ));
    Reconciler::new(
        config,
        pool.clone(),
        engine,
        catalog,
        SqliteMetaStore::new(pool),
        Arc::default(),
        RecordingMetadataReporter::new(tokio::sync::mpsc::unbounded_channel().0),
        Arc::new(ReconcileRequests::new()),
        runtime_state,
        hls_ingest,
    )
}

/// The reconciler runs on the main future: a panic in a step is contained,
/// reported as a failed step, and the next tick runs again.
#[tokio::test]
async fn a_panicking_housekeeping_step_is_contained_and_the_next_tick_runs() {
    let reconciler = reconciler(Arc::new(PanickingCatalog)).await;
    let task = tokio::spawn(async move {
        let first = reconciler.contained_housekeeping(1, u64::MAX).await;
        let second = reconciler.contained_housekeeping(2, u64::MAX).await;
        (first, second)
    });
    let (first, second) = task.await.expect("the panic stays inside the step");
    assert!(!first && !second, "both ticks ran and reported the panic");
}
