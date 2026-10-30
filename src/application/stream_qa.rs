//! Application control of the picture and sound checks: the per-pipeline
//! switch (`stream_qa_enabled:{id}`, off unless turned on) and starting or
//! stopping a pipeline's sampler with its ingest, as recordings do.

use std::sync::Arc;

use crate::application::ports::{MetaStore, PipelineStore, PipelineStoreError};
use crate::application::reconcile::{RecordingAction, decide_recording_action};
use restream_engine::engine::MediaEngine;

pub fn stream_qa_enabled_meta_key(pipeline_id: &str) -> String {
    format!("stream_qa_enabled:{pipeline_id}")
}

pub async fn load_stream_qa_enabled(meta_store: &dyn MetaStore, pipeline_id: &str) -> bool {
    meta_store
        .get_meta(&stream_qa_enabled_meta_key(pipeline_id))
        .await
        .ok()
        .flatten()
        .is_some_and(|value| value == "1")
}

/// Starts the sampler for `pipeline_id` unless it runs already; its entry
/// is released when it ends.
pub async fn spawn_stream_qa(engine: Arc<MediaEngine>, pipeline_id: String) -> bool {
    let ring = engine.get_or_create_pipeline(&pipeline_id).await;
    let Some((cancel, findings)) = engine.register_stream_qa(&pipeline_id) else {
        return false;
    };
    tokio::spawn(async move {
        restream_engine::stream_qa::runner::run(
            engine.clone(),
            pipeline_id.clone(),
            ring,
            findings.clone(),
            cancel,
        )
        .await;
        engine.release_stream_qa(&pipeline_id, &findings);
    });
    true
}

/// Every pipeline whose checks are on and whose source is live (or within
/// the reconnect grace) runs a sampler; every other one does not.
pub async fn reconcile_stream_qa(
    engine: &Arc<MediaEngine>,
    pipeline_catalog: &dyn PipelineStore,
    meta_store: &dyn MetaStore,
    ingest_disconnect_grace_ms: u64,
) -> Result<(), PipelineStoreError> {
    for pipeline in pipeline_catalog.list_pipelines().await? {
        let effective_has_ingest = engine.has_active_ingest(&pipeline.id).await
            || engine
                .has_recent_ingest_disconnect(&pipeline.id, ingest_disconnect_grace_ms)
                .await;
        let enabled = load_stream_qa_enabled(meta_store, &pipeline.id).await;
        // The same rule as recordings: wanted and fed, start; else stop.
        match decide_recording_action(
            enabled,
            effective_has_ingest,
            engine.is_stream_qa_active(&pipeline.id),
        ) {
            RecordingAction::Keep => {}
            RecordingAction::Start => {
                spawn_stream_qa(engine.clone(), pipeline.id).await;
            }
            RecordingAction::Stop => engine.unregister_stream_qa(&pipeline.id),
        }
    }
    Ok(())
}
