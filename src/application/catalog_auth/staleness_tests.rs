//! How stale the index can get: a committed write lands even when its caller
//! is cancelled, and the periodic reload enforces a change no write path
//! reported.

use super::write_visibility_tests::Catalog;
use super::*;
use crate::application::ports::{
    PipelineCreateFuture, PipelineDeleteFuture, PipelineIngestHostFuture, PipelineListFuture,
    PipelineLookupFuture, PipelineStore, PipelineUpdateFuture,
};
use crate::application::services::PipelineService;
use crate::infrastructure::sqlite_ports::SqlitePipelineStore;
use restream_domain::models::Pipeline;

/// The SQLite store, except that `update_pipeline` holds its answer behind a
/// gate after the update has committed: the window in which a cancelled
/// caller used to skip its refresh.
struct CommitThenWait {
    inner: SqlitePipelineStore,
    committed: tokio::sync::Notify,
    gate: tokio::sync::watch::Receiver<bool>,
}

impl PipelineStore for CommitThenWait {
    fn get_pipeline<'a>(&'a self, id: &'a str) -> PipelineLookupFuture<'a> {
        self.inner.get_pipeline(id)
    }
    fn get_pipeline_by_stream_key<'a>(&'a self, stream_key: &'a str) -> PipelineLookupFuture<'a> {
        self.inner.get_pipeline_by_stream_key(stream_key)
    }
    fn list_pipelines<'a>(&'a self) -> PipelineListFuture<'a> {
        self.inner.list_pipelines()
    }
    fn create_pipeline<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        stream_key: &'a str,
        input_source: Option<&'a str>,
        srt_ingest_policy: Option<&'a str>,
    ) -> PipelineCreateFuture<'a> {
        self.inner
            .create_pipeline(id, name, stream_key, input_source, srt_ingest_policy)
    }
    fn update_pipeline<'a>(
        &'a self,
        id: &'a str,
        name: &'a str,
        stream_key: &'a str,
        input_source: Option<&'a str>,
        srt_ingest_policy: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a> {
        Box::pin(async move {
            let updated = self
                .inner
                .update_pipeline(id, name, stream_key, input_source, srt_ingest_policy)
                .await;
            self.committed.notify_one();
            let mut gate = self.gate.clone();
            let _ = gate.wait_for(|open| *open).await;
            updated
        })
    }
    fn delete_pipeline<'a>(&'a self, id: &'a str) -> PipelineDeleteFuture<'a> {
        self.inner.delete_pipeline(id)
    }
    fn get_ingest_host<'a>(&'a self) -> PipelineIngestHostFuture<'a> {
        self.inner.get_ingest_host()
    }
    fn update_pipeline_input_source<'a>(
        &'a self,
        pipeline: &'a Pipeline,
        input_source: Option<&'a str>,
    ) -> PipelineUpdateFuture<'a> {
        self.inner
            .update_pipeline_input_source(pipeline, input_source)
    }
}

/// Polls admission until `key` is refused or `within` passes.
async fn eventually_refused(c: &Catalog, key: &str, within: Duration) -> bool {
    let deadline = tokio::time::Instant::now() + within;
    loop {
        if c.admitted(PipelineAccessMode::RtmpPublish, key)
            .await
            .is_none()
        {
            return true;
        }
        if tokio::time::Instant::now() >= deadline {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// A key rotation whose caller is cancelled after SQLite committed it (a
/// dropped HTTP request) is still enforced: the write and its refresh run as
/// one task the caller cannot cancel.
#[tokio::test]
async fn a_rotation_whose_caller_is_cancelled_after_commit_is_still_enforced() {
    let c = Catalog::open("cancelled-rotation").await;
    c.pipelines
        .create_pipeline("pipe", "Pipe", "key-a", None, None)
        .await
        .unwrap();
    let (open, gate) = tokio::sync::watch::channel(false);
    let store = Arc::new(CommitThenWait {
        inner: SqlitePipelineStore::new(c.pool.clone()),
        committed: tokio::sync::Notify::new(),
        gate,
    });
    let service = PipelineService::with_store(store.clone(), c.catalog.clone());

    let caller = tokio::spawn(async move {
        service
            .update_pipeline("pipe", "Pipe", "key-b", None, None)
            .await
    });
    store.committed.notified().await;
    caller.abort();
    assert!(caller.await.unwrap_err().is_cancelled());
    open.send(true).unwrap();

    assert!(
        eventually_refused(&c, "key-a", Duration::from_secs(2)).await,
        "the committed rotation was never enforced: the old key still connects"
    );
    assert!(
        c.admitted(PipelineAccessMode::RtmpPublish, "key-b")
            .await
            .is_some()
    );
}

/// A row changed in SQLite behind the services' back (no refresh asked) is
/// enforced by the periodic reload within its interval, and the reload stops
/// when told to.
#[tokio::test]
async fn the_periodic_reload_enforces_a_change_no_write_reported() {
    const INTERVAL: Duration = Duration::from_millis(50);
    let c = Catalog::open("periodic").await;
    c.pipelines
        .create_pipeline("pipe", "Pipe", "key-a", None, None)
        .await
        .unwrap();
    let reloads = tokio::spawn(c.catalog.clone().reload_periodically(INTERVAL));

    sqlx::query("UPDATE pipeline_inputs SET enabled = 0 WHERE pipeline_id = 'pipe'")
        .execute(&c.pool)
        .await
        .unwrap();

    assert!(
        eventually_refused(&c, "key-a", INTERVAL * 4).await,
        "a disabled key still connects after four reload intervals"
    );
    c.catalog.shutdown();
    tokio::time::timeout(Duration::from_secs(1), reloads)
        .await
        .expect("the periodic reload stops when told to")
        .unwrap();
}
