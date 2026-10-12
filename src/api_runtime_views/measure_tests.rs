use super::*;
use crate::domain::stage::{StageKey, StageKind};
use crate::media::avio::MemoryQueue;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_runtime::StageRuntimeManager;
use std::sync::Arc;

fn packet(bytes: usize) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 0,
        dts: 0,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: bytes::Bytes::from(vec![0; bytes]),
    }
}

async fn egress_with_delivery(
    engine: &MediaEngine,
    output_id: &str,
    pipeline_id: &str,
    rate: Option<(f64, f64)>,
) {
    let _ = engine
        .register_egress(output_id, pipeline_id, "rtmp://dest/live")
        .await;
    let active = engine.egresses.active.read().await;
    let mut quality = crate::sync::lock(&active[output_id].quality);
    quality.delivered_bps = rate.map(|(bps, _)| bps);
    quality.delivery_ratio = rate.map(|(_, ratio)| ratio);
}

/// The snapshot replaces the harness's per-sample reads of the telemetry and
/// health payloads, so it must equal what the harness computed from them.
#[tokio::test]
async fn the_snapshot_equals_the_sums_of_the_payloads_it_replaces() {
    let engine = Arc::new(MediaEngine::new());
    for (pipeline, key) in [("pipe-a", "key-a"), ("pipe-b", "key-b")] {
        let ring = engine.get_or_create_pipeline(pipeline).await;
        ring.push(packet(1_000));
        ring.push(packet(500));
        let _ = engine
            .try_register_ingest_attempt(pipeline, key, "rtmp")
            .await
            .unwrap();
    }
    egress_with_delivery(&engine, "a-ok", "pipe-a", Some((8e6, 1.0))).await;
    egress_with_delivery(&engine, "a-slow", "pipe-a", Some((2e6, 0.25))).await;
    egress_with_delivery(&engine, "a-new", "pipe-a", None).await;
    egress_with_delivery(&engine, "b-ok", "pipe-b", Some((8e6, 0.99))).await;

    let stage = StageKey::new("pipe-a", StageKind::video_preset("720p"));
    let (handle, _lease1, _) = StageRuntimeManager::new(engine.clone())
        .ensure_stage(stage.clone(), Arc::new(RingBuffer::new(4)), None)
        .await;
    handle.ring.push(packet(700));
    let queue = Arc::new(MemoryQueue::new_with_capacity(128));
    queue.write_sync(b"queued bytes");
    engine.register_input_queue(stage, queue).await;

    // A stage known only by its metrics (no runtime) still counts as a stage.
    let _ = engine
        .get_or_create_stage_metrics(StageKey::new("pipe-b", StageKind::video_preset("480p")))
        .await;
    for (pipeline, bytes) in [("pipe-a", 1_234_u64), ("pipe-b", 4_321)] {
        engine.ingests.active.read().await[pipeline]
            .bytes_received
            .store(bytes, std::sync::atomic::Ordering::Relaxed);
    }

    let snapshot = measure_snapshot(&engine, 2).await;
    let telemetry = super::super::engine_telemetry(&engine).await;
    let accounting = &telemetry["memoryAccounting"];
    let ring_bytes = |name: &str| {
        accounting[name]
            .as_array()
            .unwrap()
            .iter()
            .map(|ring| ring["payloadStats"]["payloadBytes"].as_u64().unwrap())
            .sum::<u64>()
    };

    assert_eq!(snapshot.pipelines, 2);
    assert_eq!(
        snapshot.ingests,
        telemetry["ingests"].as_array().unwrap().len()
    );
    assert_eq!(
        snapshot.egresses,
        telemetry["egresses"].as_array().unwrap().len()
    );
    assert_eq!(snapshot.egresses, 4);
    assert_eq!(
        snapshot.stages,
        telemetry["stages"].as_array().unwrap().len()
    );
    assert_eq!(
        snapshot.stages, 2,
        "one runtime stage and one metrics-only stage"
    );
    let ingest_bytes = telemetry["ingests"]
        .as_array()
        .unwrap()
        .iter()
        .map(|ingest| ingest["bytesReceived"].as_u64().unwrap())
        .sum::<u64>();
    assert_eq!(snapshot.ingest_bytes_received, ingest_bytes);
    assert_eq!(snapshot.ingest_bytes_received, 5_555);
    assert_eq!(
        snapshot.active_transcoder_buffers as u64,
        telemetry["activeTranscoderBuffers"].as_u64().unwrap()
    );
    assert_eq!(
        snapshot.retained_payload_bytes,
        accounting["retainedPayloadBytes"].as_u64().unwrap()
    );
    assert_eq!(
        snapshot.source_ring_payload_bytes,
        ring_bytes("sourceRings")
    );
    assert_eq!(snapshot.source_ring_payload_bytes, 3_000);
    assert_eq!(
        snapshot.transcoder_ring_payload_bytes,
        ring_bytes("transcoderRings")
    );
    assert_eq!(snapshot.transcoder_ring_payload_bytes, 700);
    assert_eq!(
        snapshot.ts_muxer_ring_payload_bytes,
        ring_bytes("tsMuxerRings")
    );
    assert_eq!(
        snapshot.stage_input_queue_len_bytes,
        accounting["avioQueues"]["totalLenBytes"].as_u64().unwrap()
    );
    assert_eq!(
        snapshot.stage_input_queue_len_bytes,
        b"queued bytes".len() as u64
    );
    let high_water = accounting["avioQueues"]["inputQueues"]
        .as_array()
        .unwrap()
        .iter()
        .map(|queue| queue["highWaterBytes"].as_u64().unwrap())
        .sum::<u64>();
    assert_eq!(snapshot.stage_input_queue_high_water_bytes, high_water);

    // Delivery: the per-pipeline feed rows, folded as the harness folded them.
    let mut rated = 0;
    let mut delivered = 0;
    let mut destinations = 0;
    let mut ratio_min = f64::INFINITY;
    let mut jain_min = f64::INFINITY;
    for pipeline in ["pipe-a", "pipe-b"] {
        let view = super::super::pipeline_telemetry(&engine, pipeline).await;
        for feed in view["delivery"].as_array().unwrap() {
            destinations += feed["destinations"].as_u64().unwrap() as usize;
            rated += feed["rated"].as_u64().unwrap() as usize;
            delivered += feed["delivered"].as_u64().unwrap() as usize;
            ratio_min = ratio_min.min(feed["ratioMin"].as_f64().unwrap_or(f64::INFINITY));
            jain_min = jain_min.min(feed["jain"].as_f64().unwrap_or(f64::INFINITY));
        }
    }
    assert_eq!(snapshot.delivery.destinations, destinations);
    assert_eq!(snapshot.delivery.rated, rated);
    assert_eq!(
        snapshot.delivery.rated, 3,
        "an output without a sample is not rated"
    );
    assert_eq!(snapshot.delivery.delivered, delivered);
    assert_eq!(snapshot.delivery.delivered, 2);
    assert_eq!(snapshot.delivery.ratio_min, Some(ratio_min));
    assert_eq!(snapshot.delivery.ratio_min, Some(0.25));
    assert_eq!(snapshot.delivery.jain_min, Some(jain_min));
    assert!(jain_min < 1.0, "pipe-a's two rated outputs are unfair");
}
