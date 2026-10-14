use std::sync::Arc;

use crate::domain::stage::{StageKey, StageKind};
use crate::media::engine::MediaEngine;
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_runtime::StageRuntimeManager;

use super::spawn_pipeline_hls_segmenter;

/// The pipeline's HLS segmenter outlives the output that started it (other
/// HLS outputs share it), so the transcode stage it reads must outlive that
/// output too, and stop once the segmenter ends.
#[tokio::test]
async fn the_shared_hls_segmenter_holds_the_stage_it_reads_until_it_ends() {
    let engine = Arc::new(MediaEngine::new());
    let key = StageKey::new("pipe", StageKind::video_preset("720p"));
    let (stage, starting_output, _) = StageRuntimeManager::new(engine.clone())
        .ensure_stage(key.clone(), Arc::new(RingBuffer::new(16)), None)
        .await;
    let (store, _) = engine.ensure_hls_segmenter("pipe").await;
    let segmenter_cancel = engine.get_hls_cancel_token("pipe").await.unwrap();

    let segmenter = spawn_pipeline_hls_segmenter(
        engine.clone(),
        "pipe".to_string(),
        store,
        stage.ring.clone(),
        segmenter_cancel.clone(),
        key,
        vec![starting_output.clone()],
    );
    drop(starting_output);
    tokio::task::yield_now().await;
    assert!(
        !stage.cancel.is_cancelled(),
        "the starting output ended; the segmenter still reads the stage"
    );

    segmenter_cancel.cancel();
    segmenter.await.unwrap();
    assert!(
        stage.cancel.is_cancelled(),
        "the segmenter ended; the stage stops"
    );
}
