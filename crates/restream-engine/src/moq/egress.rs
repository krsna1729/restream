//! The engine query behind a MoQ broadcast's startup snapshot. The snapshot
//! types and the egress backend live in [`crate::egress::backends::moq`];
//! this function reads the engine's ingest metadata, so it stays above the
//! engine.

use std::sync::Arc;

use crate::egress::backends::moq::MoqPublishStartup;
use crate::egress::backends::moq::startup::{startup_audio, startup_video};
use crate::engine::MediaEngine;
use crate::rtmp::{output_ring_video_codec_kind, resolved_output_audio_tracks};
use restream_sync::RingBuffer;

/// The tracks and configurations of `ring`, which carries `pipeline_id`'s
/// media: H.264 or HEVC video and the first AAC track.
pub async fn publish_startup(
    engine: &MediaEngine,
    pipeline_id: &str,
    ring: &Arc<RingBuffer>,
) -> MoqPublishStartup {
    let codec = output_ring_video_codec_kind(engine, pipeline_id, ring).await;
    let (ingest_video_header, ingest_audio_header) = engine.get_sequence_headers(pipeline_id).await;
    let video_meta = engine
        .with_active_ingest(pipeline_id, |ingest| ingest.metadata().video)
        .await
        .flatten();
    let video = startup_video(codec, video_meta.as_ref(), ring, ingest_video_header);
    let audio = resolved_output_audio_tracks(engine, pipeline_id, ring)
        .await
        .into_iter()
        .find(|track| track.codec.eq_ignore_ascii_case("aac"))
        .map(|track| startup_audio(&track, ingest_audio_header));
    MoqPublishStartup {
        video,
        audio,
        ..MoqPublishStartup::default()
    }
}
