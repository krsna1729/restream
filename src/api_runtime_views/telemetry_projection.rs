use std::sync::atomic::Ordering;

use crate::domain::stage::StageKey;
use crate::media::engine::ActiveIngest;
use crate::media::ring_buffer::RingBuffer;

pub(super) use super::common::{egress_runtime_json, ring_payload_stats_json};

pub(super) fn ingest_telemetry_json(pipeline_id: &str, ingest: &ActiveIngest) -> serde_json::Value {
    serde_json::json!({
        "pipelineId": pipeline_id,
        "protocol": ingest.protocol,
        "uptimeSecs": ingest.start_time.elapsed().as_secs_f64(),
        "bytesReceived": ingest.bytes_received.load(Ordering::Relaxed),
        "metrics": ingest.metrics.snapshot(),
    })
}

pub(super) fn stage_telemetry_row_json(
    key: &StageKey,
    metrics: serde_json::Value,
    pipe_metrics: Option<serde_json::Value>,
    active: Option<bool>,
    payload_stats: Option<serde_json::Value>,
    lifecycle: Option<&crate::runtime::stage::StageRuntimeSnapshot>,
) -> serde_json::Value {
    let mut value = serde_json::json!({
        "stageKey": key.to_string(),
        "pipelineId": key.pipeline.as_str(),
        "kind": key.kind.to_string(),
        "metrics": metrics,
    });
    if let Some(pipe_metrics) = pipe_metrics {
        value["pipeMetrics"] = pipe_metrics;
    }
    if let Some(active) = active {
        value["active"] = serde_json::Value::Bool(active);
    }
    if let Some(payload_stats) = payload_stats {
        value["payloadStats"] = payload_stats;
    }
    if let Some(snap) = lifecycle {
        value["lifecycle"] = super::stage_projection::stage_runtime_snapshot_json(snap);
    }
    value
}

pub(super) fn source_ring_telemetry_json(
    pipeline_id: &str,
    ring: &RingBuffer,
) -> serde_json::Value {
    serde_json::json!({
        "pipelineId": pipeline_id,
        "payloadStats": ring_payload_stats_json(ring),
    })
}

pub(super) fn transcoder_ring_telemetry_json(
    key: &StageKey,
    ring: &RingBuffer,
    active: bool,
) -> serde_json::Value {
    serde_json::json!({
        "stageKey": key.to_string(),
        "pipelineId": key.pipeline.as_str(),
        "kind": key.kind.to_string(),
        "active": active,
        "payloadStats": ring_payload_stats_json(ring),
    })
}

pub(super) fn ts_muxer_ring_telemetry_json(
    stage_key: &str,
    ring: &RingBuffer,
    active: bool,
) -> serde_json::Value {
    serde_json::json!({
        "stageKey": stage_key,
        "active": active,
        "payloadStats": ring_payload_stats_json(ring),
    })
}

pub(super) fn avio_input_queue_json(
    key: &StageKey,
    len_bytes: usize,
    capacity_bytes: usize,
    high_water_bytes: usize,
    blocked_writes: u64,
    blocked_write_us: u64,
) -> serde_json::Value {
    serde_json::json!({
        "stageKey": key.to_string(),
        "pipelineId": key.pipeline.as_str(),
        "lenBytes": len_bytes,
        "capacityBytes": capacity_bytes,
        "highWaterBytes": high_water_bytes,
        "blockedWrites": blocked_writes,
        "blockedWriteUs": blocked_write_us,
    })
}

#[allow(clippy::too_many_arguments)]
pub(super) fn memory_accounting_json(
    retained_payload_bytes: u64,
    source_rings: Vec<serde_json::Value>,
    transcoder_rings: Vec<serde_json::Value>,
    ts_muxer_rings: Vec<serde_json::Value>,
    avio_total_len_bytes: usize,
    avio_total_capacity_bytes: usize,
    avio_input_queues: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "retainedPayloadBytes": retained_payload_bytes,
        "sourceRings": source_rings,
        "transcoderRings": transcoder_rings,
        "tsMuxerRings": ts_muxer_rings,
        "avioQueues": {
            "totalLenBytes": avio_total_len_bytes,
            "totalCapacityBytes": avio_total_capacity_bytes,
            "inputQueues": avio_input_queues,
        },
    })
}

pub(super) fn engine_telemetry_json(
    generated_at: String,
    ingests: Vec<serde_json::Value>,
    stages: Vec<serde_json::Value>,
    egresses: Vec<serde_json::Value>,
    active_transcoder_buffers: usize,
    memory_accounting: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "generatedAt": generated_at,
        "ingests": ingests,
        "stages": stages,
        "egresses": egresses,
        "activeTranscoderBuffers": active_transcoder_buffers,
        "memoryAccounting": memory_accounting,
    })
}

pub(super) fn pipeline_ingest_telemetry_json(ingest: &ActiveIngest) -> serde_json::Value {
    let metadata = ingest.metadata();
    serde_json::json!({
        "protocol": ingest.protocol,
        "streamKey": ingest.stream_key,
        "uptimeSecs": ingest.start_time.elapsed().as_secs_f64(),
        "bytesReceived": ingest.bytes_received.load(Ordering::Relaxed),
        "video": metadata.video,
        "audio": metadata.audio,
        "metrics": ingest.metrics.snapshot(),
    })
}

pub(super) fn pipeline_source_ring_json(ring: &RingBuffer) -> serde_json::Value {
    let (fill, cap) = ring.fill_and_capacity();
    let readers: Vec<serde_json::Value> = ring
        .reader_snapshots()
        .into_iter()
        .map(|reader| {
            serde_json::json!({
                "name": reader.name,
                "lagSlots": reader.lag_slots,
                "overflowCount": reader.overflow_count,
                "packetAgeMs": reader.packet_age_ms,
            })
        })
        .collect();

    serde_json::json!({
        "fill": fill,
        "capacity": cap,
        "fillPercent": (fill * 100).checked_div(cap).unwrap_or(0),
        "estimatedPktRatePerSec": ring.estimated_pkt_rate.load(Ordering::Relaxed),
        "bufferDepthSecs": ring.buffer_depth_secs(),
        "payloadStats": ring_payload_stats_json(ring),
        "readers": readers,
    })
}

pub(super) fn pipeline_telemetry_json(
    generated_at: String,
    pipeline_id: &str,
    ingest: Option<serde_json::Value>,
    source_ring: Option<serde_json::Value>,
    stages: Vec<serde_json::Value>,
    egresses: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "generatedAt": generated_at,
        "pipelineId": pipeline_id,
        "ingest": ingest,
        "sourceRing": source_ring,
        "stages": stages,
        "egresses": egresses,
    })
}

pub(super) fn single_stage_telemetry_json(
    generated_at: String,
    key: &StageKey,
    metrics: serde_json::Value,
    pipe_metrics: Option<serde_json::Value>,
    lifecycle: Option<&crate::runtime::stage::StageRuntimeSnapshot>,
) -> serde_json::Value {
    let mut value = stage_telemetry_row_json(key, metrics, pipe_metrics, None, None, lifecycle);
    value["generatedAt"] = serde_json::Value::String(generated_at);
    value
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::stage::StageKind;

    #[test]
    fn stage_row_includes_optional_fields() {
        let key = StageKey::new("telemetry-pipe", StageKind::video_preset("720p"));
        let value = stage_telemetry_row_json(
            &key,
            serde_json::json!({"packetsIn": 1}),
            Some(serde_json::json!({"drops": 2})),
            Some(true),
            Some(serde_json::json!({"payloadBytes": 256})),
            None,
        );

        assert_eq!(value["stageKey"], "telemetry-pipe:video:720p");
        assert_eq!(value["pipelineId"], "telemetry-pipe");
        assert_eq!(value["kind"], "video:720p");
        assert_eq!(value["metrics"]["packetsIn"], 1);
        assert_eq!(value["pipeMetrics"]["drops"], 2);
        assert_eq!(value["active"], true);
        assert_eq!(value["payloadStats"]["payloadBytes"], 256);
    }

    #[test]
    fn engine_view_wraps_memory_accounting() {
        let value = engine_telemetry_json(
            "2026-06-30T12:00:00Z".to_string(),
            vec![serde_json::json!({"pipelineId": "pipeline-a"})],
            vec![serde_json::json!({"stageKey": "pipeline-a:source"})],
            vec![serde_json::json!({"outputId": "egress-a"})],
            2,
            memory_accounting_json(
                4096,
                vec![serde_json::json!({"pipelineId": "pipeline-a"})],
                vec![serde_json::json!({"stageKey": "pipeline-a:video:720p"})],
                vec![serde_json::json!({"stageKey": "pipeline-a_ts"})],
                128,
                1024,
                vec![serde_json::json!({"stageKey": "pipeline-a:source"})],
            ),
        );

        assert_eq!(value["generatedAt"], "2026-06-30T12:00:00Z");
        assert_eq!(value["activeTranscoderBuffers"], 2);
        assert_eq!(value["memoryAccounting"]["retainedPayloadBytes"], 4096);
        assert_eq!(
            value["memoryAccounting"]["avioQueues"]["totalCapacityBytes"],
            1024
        );
        assert_eq!(
            value["memoryAccounting"]["tsMuxerRings"][0]["stageKey"],
            "pipeline-a_ts"
        );
    }
}
