use super::*;
use crate::application::ports::{
    PipelineCreateFuture, PipelineDeleteFuture, PipelineIngestHostFuture, PipelineListFuture,
    PipelineLookupFuture, PipelineUpdateFuture,
};
use crate::db::{SHARED_LOOP_DEADLINE, create_pool, setup_database_schema};

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

fn housekeeping(catalog: Arc<dyn PipelineStore>, db: DeadlineDb) -> Housekeeping {
    Housekeeping::new(
        Arc::new(AppConfig::default()),
        db,
        Arc::new(MediaEngine::new()),
        catalog,
        Arc::default(),
        RecordingMetadataReporter::new(tokio::sync::mpsc::unbounded_channel().0),
    )
}

async fn memory_pool() -> sqlx::SqlitePool {
    let pool = create_pool("sqlite::memory:").await.unwrap();
    setup_database_schema(&pool).await.unwrap();
    pool
}

/// A panic in a tick is contained, reported as a failed step, and the next
/// tick runs again.
#[tokio::test]
async fn a_panicking_housekeeping_step_is_contained_and_the_next_tick_runs() {
    let pool = memory_pool().await;
    let housekeeping = housekeeping(
        Arc::new(PanickingCatalog),
        DeadlineDb::new(pool, SHARED_LOOP_DEADLINE),
    );
    let task = tokio::spawn(async move {
        let first = housekeeping.contained_tick(1, u64::MAX).await;
        let second = housekeeping.contained_tick(2, u64::MAX).await;
        (first, second)
    });
    let (first, second) = task.await.expect("the panic stays inside the step");
    assert!(!first && !second, "both ticks ran and reported the panic");
}

/// Every connection of its pool held: the tick's reads (pipelines,
/// switches) answer at their deadline and the tick ends, so the next one
/// runs, instead of each waiting out the pool's 30 s acquire timeout. The
/// prune's writes are not in this tick: a write waits for its outcome.
#[tokio::test]
async fn a_housekeeping_ticks_reads_return_at_their_deadlines_while_every_connection_is_held() {
    let pool = memory_pool().await;
    let db = DeadlineDb::new(pool.clone(), Duration::from_millis(200));
    let housekeeping = housekeeping(
        Arc::new(crate::infrastructure::sqlite_ports::SqlitePipelineStore::new(pool.clone())),
        db,
    );
    let mut held = Vec::new();
    while let Ok(connection) =
        tokio::time::timeout(Duration::from_millis(200), pool.acquire()).await
    {
        held.push(connection.unwrap());
    }

    let completed = tokio::time::timeout(
        Duration::from_secs(5),
        housekeeping.contained_tick(1, u64::MAX),
    )
    .await
    .expect("the tick answers at its deadlines, not the acquire timeout");
    assert!(completed);
    drop(held);
}
