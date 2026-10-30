use std::sync::Arc;

use restream_domain::domain::stage::{StageKey, StageKind};
use restream_engine::engine::MediaEngine;
use restream_engine::stage_runtime::StageRuntimeManager;
use restream_sync::RingBuffer;

use super::spawn_pipeline_hls_segmenter;

/// The pipeline's HLS segmenter outlives the output that started it (other
/// HLS outputs share it, and a retry reuses it), so the transcode stage it
/// reads must outlive that output too, and stop once the segmenter ends.
#[tokio::test]
async fn the_shared_hls_segmenter_holds_the_stage_it_reads_until_it_ends() {
    let engine = Arc::new(MediaEngine::new());
    let manager = StageRuntimeManager::new(engine.clone());
    let source = Arc::new(RingBuffer::new(16));
    let key = StageKey::new("pipe", StageKind::video_preset("720p"));
    let (stage, starting_output, _) = manager
        .ensure_stage(key.clone(), source.clone(), None)
        .await;
    let (store, _) = engine.ensure_hls_segmenter("pipe").await;
    let segmenter_cancel = engine.get_hls_cancel_token("pipe").await.unwrap();

    let segmenter = spawn_pipeline_hls_segmenter(
        engine.clone(),
        "pipe".to_string(),
        store,
        stage.ring.clone(),
        segmenter_cancel.clone(),
        key.clone(),
        vec![starting_output.clone()],
    );
    drop(starting_output);
    tokio::task::yield_now().await;
    assert!(
        !stage.cancel.is_cancelled(),
        "the starting output ended; the segmenter still reads the stage"
    );
    let (retry, retry_lease, created) = manager.ensure_stage(key, source, None).await;
    assert!(
        !created && Arc::ptr_eq(&retry.ring, &stage.ring),
        "a retry feeds the ring the segmenter reads"
    );

    segmenter_cancel.cancel();
    segmenter.await.unwrap();
    drop(retry_lease);
    assert!(
        stage.cancel.is_cancelled(),
        "the segmenter ended; the stage stops"
    );
}

/// A stage that ends on its own (its transcoder exits) is never fed again,
/// so the segmenter on its ring ends and leaves the pipeline, letting the
/// next HLS attempt start one on its new stage.
#[tokio::test]
async fn the_shared_hls_segmenter_ends_when_its_stage_ends_on_its_own() {
    let engine = Arc::new(MediaEngine::new());
    let key = StageKey::new("pipe", StageKind::video_preset("720p"));
    let (stage, lease, _) = StageRuntimeManager::new(engine.clone())
        .ensure_stage(key.clone(), Arc::new(RingBuffer::new(16)), None)
        .await;
    let (store, _) = engine.ensure_hls_segmenter("pipe").await;
    let segmenter_cancel = engine.get_hls_cancel_token("pipe").await.unwrap();
    let segmenter = spawn_pipeline_hls_segmenter(
        engine.clone(),
        "pipe".to_string(),
        store,
        stage.ring.clone(),
        segmenter_cancel,
        key,
        vec![lease],
    );

    stage.cancel.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(5), segmenter)
        .await
        .expect("the segmenter ends with its stage")
        .unwrap();
    assert!(
        engine.get_hls_cancel_token("pipe").await.is_none(),
        "the pipeline has no segmenter, so the next attempt starts one"
    );
}
