use std::sync::atomic::Ordering;

use crate::media::engine::{ActiveEgress, ActiveIngest, MediaEngine};
use crate::media::snapshots::PublisherQuality;

use super::common::{egress_runtime_json, ingest_video_track_selection_json};
pub(super) use super::common::{reader_snapshot_json, ring_payload_stats_json};

pub(super) fn processing_graph_node(
    id: impl Into<String>,
    node_type: &'static str,
    label: impl Into<String>,
    active: bool,
    details: Option<serde_json::Value>,
    metrics: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "id": id.into(),
        "type": node_type,
        "label": label.into(),
        "active": active,
        "details": details,
        "metrics": metrics,
    })
}

pub(super) fn processing_graph_edge(
    from: impl Into<String>,
    to: impl Into<String>,
    label: impl Into<String>,
) -> serde_json::Value {
    serde_json::json!({
        "from": from.into(),
        "to": to.into(),
        "label": label.into(),
    })
}

pub(super) fn processing_graph_ingest_details(ingest: &ActiveIngest) -> serde_json::Value {
    let metadata = ingest.metadata();
    let bytes_received = ingest.bytes_received.load(Ordering::Relaxed);
    let bitrate_kbps = MediaEngine::sample_ingest_bitrate_kbps(ingest);
    let last_progress_ms = ingest.last_progress_ms.load(Ordering::Relaxed);
    let last_progress_age_ms = if last_progress_ms > 0 {
        Some(MediaEngine::now_epoch_ms().saturating_sub(last_progress_ms))
    } else {
        None
    };
    let srt_recv_buffer = srt_recv_buffer_occupancy(&metadata.quality);

    let mut health_status = None;
    let mut health_reason = None;
    if let Some((recv, total, pct)) = srt_recv_buffer
        && pct >= 95.0
    {
        health_status = Some("warning");
        health_reason = Some(format!(
            "SRT receive buffer {:.0}% full ({} / {})",
            pct,
            human_bytes(recv),
            human_bytes(total)
        ));
    }
    if health_status.is_none() && last_progress_age_ms.is_some_and(|age| age >= 10_000) {
        health_status = Some("warning");
        health_reason = Some(format!(
            "input has not received bytes for {}",
            human_duration_ms(last_progress_age_ms.unwrap_or(0))
        ));
    }

    let mut details = serde_json::json!({
        "protocol": ingest.protocol,
        "remoteAddr": metadata.remote_addr,
        "video": metadata.video,
        "videoTrackSelection": ingest_video_track_selection_json(ingest),
        "audio": metadata.audio,
        "bytesReceived": bytes_received,
        "bitrateKbps": bitrate_kbps,
        "bytesReceivedPerSec": bitrate_kbps.map(|kbps| (kbps * 1000.0 / 8.0).round() as u64),
        "lastProgressAgeMs": last_progress_age_ms,
    });
    if let Some((recv, total, pct)) = srt_recv_buffer {
        details["srtRecvBufferBytes"] = serde_json::json!(recv);
        details["srtRecvBufferTotalBytes"] = serde_json::json!(total);
        details["srtRecvBufferPercent"] = serde_json::json!((pct * 10.0).round() / 10.0);
    }
    if let Some(status) = health_status {
        details["healthStatus"] = serde_json::json!(status);
    }
    if let Some(reason) = health_reason {
        details["healthReason"] = serde_json::json!(reason);
    }
    details
}

fn srt_recv_buffer_occupancy(quality: &PublisherQuality) -> Option<(u64, u64, f64)> {
    let recv = quality.srt_recv_buf_bytes?.max(0) as u64;
    let avail = quality.srt_recv_buf_avail_bytes?.max(0) as u64;
    let total = recv.saturating_add(avail);
    if total == 0 {
        return None;
    }
    Some((recv, total, recv as f64 / total as f64 * 100.0))
}

fn human_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        return format!("{bytes} B");
    }
    if bytes < 1024 * 1024 {
        return format!("{:.1} KiB", bytes as f64 / 1024.0);
    }
    format!("{:.1} MiB", bytes as f64 / (1024.0 * 1024.0))
}

fn human_duration_ms(ms: u64) -> String {
    if ms < 1000 {
        return format!("{ms} ms");
    }
    if ms < 60_000 {
        return format!("{:.1} s", ms as f64 / 1000.0);
    }
    format!("{:.1} min", ms as f64 / 60_000.0)
}

pub(super) fn processing_graph_demux_details(ingest: &ActiveIngest) -> serde_json::Value {
    let metadata = ingest.metadata();
    serde_json::json!({
        "protocol": ingest.protocol,
        "video": metadata.video,
        "videoTrackSelection": ingest_video_track_selection_json(ingest),
        "audio": metadata.audio,
        "audioTracks": ingest
            .audio_tracks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect::<Vec<_>>(),
    })
}

pub(super) fn processing_graph_source_ring_details(
    fill: usize,
    capacity: usize,
    payload_stats: serde_json::Value,
    format: impl Into<String>,
    readers: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "fill": fill,
        "capacity": capacity,
        "fillPercent": (fill * 100).checked_div(capacity).unwrap_or(0),
        "payloadStats": payload_stats,
        "format": format.into(),
        "readers": readers,
    })
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn processing_graph_stage_node(
    id: impl Into<String>,
    node_type: &'static str,
    label: impl Into<String>,
    stage_key: impl Into<String>,
    lifecycle: Option<&crate::runtime::stage::StageRuntimeSnapshot>,
    active: bool,
    metrics: Option<serde_json::Value>,
    queue_metrics: Option<serde_json::Value>,
    pipe_metrics: Option<serde_json::Value>,
    payload_stats: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "id": id.into(),
        "type": node_type,
        "label": label.into(),
        "stageKey": stage_key.into(),
        "active": active,
        "details": lifecycle.map(super::stage_projection::stage_runtime_snapshot_json),
        "metrics": metrics,
        "queueMetrics": queue_metrics,
        "pipeMetrics": pipe_metrics,
        "payloadStats": payload_stats,
    })
}

pub(super) fn processing_graph_egress_details(
    egress: &ActiveEgress,
    has_ingest: bool,
) -> serde_json::Value {
    let bytes = egress.bytes_sent.load(Ordering::Relaxed);
    let mut details = egress_runtime_json(egress, true, has_ingest, None);
    details["totalSize"] = serde_json::json!(bytes);
    details["bitrateKbps"] = serde_json::json!(
        *egress
            .bitrate_kbps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
    );
    details["startedAt"] = serde_json::Value::String(egress.started_at.clone());
    details
}

pub(super) fn processing_graph_packetizer_details(
    protocol: &'static str,
    encoding: &str,
    stage_key: String,
    payload_stats: Option<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "protocol": protocol,
        "encoding": encoding,
        "stageKey": stage_key,
        "payloadStats": payload_stats,
    })
}

pub(super) fn processing_graph_recirculation_target_details(
    pipeline_id: &str,
    input_id: &str,
) -> serde_json::Value {
    serde_json::json!({
        "pipelineId": pipeline_id,
        "inputId": input_id,
    })
}

pub(super) fn processing_graph_json(
    generated_at: String,
    pipeline_id: &str,
    nodes: Vec<serde_json::Value>,
    edges: Vec<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "generatedAt": generated_at,
        "pipelineId": pipeline_id,
        "nodes": nodes,
        "edges": edges,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::input_gate::InputPacketGate;
    use crate::media::stage_metrics::StageMetrics;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::{Duration, Instant};

    #[test]
    fn graph_helpers_wrap_node_edge_and_root_shape() {
        let node = processing_graph_node(
            "pipe_ingest",
            "ingest",
            "RTMP ingest",
            true,
            Some(serde_json::json!({"protocol": "rtmp"})),
            Some(serde_json::json!({"packetsIn": 1})),
        );
        let edge = processing_graph_edge("pipe_ingest", "pipe_demux", "RTMP");
        let graph = processing_graph_json(
            "2026-06-30T12:00:00Z".to_string(),
            "pipe",
            vec![node.clone()],
            vec![edge.clone()],
        );

        assert_eq!(node["type"], "ingest");
        assert_eq!(node["details"]["protocol"], "rtmp");
        assert_eq!(edge["label"], "RTMP");
        assert_eq!(graph["pipelineId"], "pipe");
        assert_eq!(graph["nodes"][0]["id"], "pipe_ingest");
        assert_eq!(graph["edges"][0]["to"], "pipe_demux");
    }

    #[test]
    fn source_ring_details_report_fill_percent() {
        let details = processing_graph_source_ring_details(
            2,
            8,
            serde_json::json!({"payloadBytes": 512}),
            "mpegts".to_string(),
            vec![serde_json::json!({"name": "preview"})],
        );

        assert_eq!(details["fill"], 2);
        assert_eq!(details["capacity"], 8);
        assert_eq!(details["fillPercent"], 25);
        assert_eq!(details["payloadStats"]["payloadBytes"], 512);
        assert_eq!(details["readers"][0]["name"], "preview");
    }

    #[test]
    fn human_units_preserve_existing_boundaries() {
        assert_eq!(human_bytes(0), "0 B");
        assert_eq!(human_bytes(1023), "1023 B");
        assert_eq!(human_bytes(1024), "1.0 KiB");
        assert_eq!(human_bytes(1024 * 1024), "1.0 MiB");
        assert_eq!(human_bytes(1024 * 1024 - 1), "1024.0 KiB");
        let rendered_bytes = human_bytes(u64::MAX);
        assert!(rendered_bytes.ends_with(" MiB"));
        assert!(!rendered_bytes.contains("GiB"));
        assert_eq!(human_duration_ms(0), "0 ms");
        assert_eq!(human_duration_ms(999), "999 ms");
        assert_eq!(human_duration_ms(1000), "1.0 s");
        assert_eq!(human_duration_ms(60_000), "1.0 min");
        assert_eq!(human_duration_ms(59_999), "60.0 s");
        let rendered_duration = human_duration_ms(u64::MAX);
        assert!(rendered_duration.ends_with(" min"));
        assert!(!rendered_duration.contains("hour"));
    }

    fn quality_with_srt_recv_buf(recv: Option<i32>, avail: Option<i32>) -> PublisherQuality {
        PublisherQuality {
            srt_recv_buf_bytes: recv,
            srt_recv_buf_avail_bytes: avail,
            ..Default::default()
        }
    }

    #[test]
    fn srt_recv_buffer_occupancy_handles_missing_zero_and_negative_values() {
        assert_eq!(
            srt_recv_buffer_occupancy(&quality_with_srt_recv_buf(None, Some(10))),
            None
        );
        assert_eq!(
            srt_recv_buffer_occupancy(&quality_with_srt_recv_buf(Some(10), None)),
            None
        );
        assert_eq!(
            srt_recv_buffer_occupancy(&quality_with_srt_recv_buf(None, None)),
            None
        );
        assert_eq!(
            srt_recv_buffer_occupancy(&quality_with_srt_recv_buf(Some(0), Some(0))),
            None
        );
        assert_eq!(
            srt_recv_buffer_occupancy(&quality_with_srt_recv_buf(Some(-1), Some(100))),
            Some((0, 100, 0.0))
        );
        assert_eq!(
            srt_recv_buffer_occupancy(&quality_with_srt_recv_buf(Some(-1), Some(-1))),
            None
        );
    }

    #[test]
    fn srt_recv_buffer_occupancy_computes_percentage_without_overflow() {
        assert_eq!(
            srt_recv_buffer_occupancy(&quality_with_srt_recv_buf(Some(250), Some(750))),
            Some((250, 1000, 25.0))
        );
        let (recv, total, pct) =
            srt_recv_buffer_occupancy(&quality_with_srt_recv_buf(Some(i32::MAX), Some(i32::MAX)))
                .expect("both fields are non-negative and nonzero");
        assert_eq!(recv, i32::MAX as u64);
        assert_eq!(total, 2 * i32::MAX as u64);
        assert!((pct - 50.0).abs() < f64::EPSILON);
    }

    #[test]
    fn ingest_details_warn_when_srt_recv_buffer_is_saturated() {
        let now_ms = MediaEngine::now_epoch_ms();
        let ingest = ActiveIngest {
            attempt_id: 1,
            pipeline_id: "pipeline".to_string(),
            input_id: "input".to_string(),
            stream_key: "stream".to_string(),
            gate: Arc::new(InputPacketGate::active()),
            start_time: Instant::now() - Duration::from_secs(10),
            protocol: "srt".to_string(),
            bytes_received: Arc::new(AtomicU64::new(4096)),
            metrics: Arc::new(StageMetrics::new()),
            last_progress_ms: Arc::new(AtomicU64::new(now_ms.saturating_sub(30_000))),
            metadata: RwLock::new(crate::media::engine::IngestMetadata {
                remote_addr: Some("127.0.0.1:9000".to_string()),
                quality: PublisherQuality {
                    srt_recv_buf_bytes: Some(8_218_796),
                    srt_recv_buf_avail_bytes: Some(1_500),
                    ..Default::default()
                },
                ..Default::default()
            }),
            audio_tracks: Mutex::new(Arc::new(Vec::new())),
            keyframe_times: Arc::new(Mutex::new(Vec::new())),
            video_sequence_header: Mutex::new(None),
            audio_sequence_header: Mutex::new(None),
            prev_bytes_received: AtomicU64::new(4096),
            prev_sample_time: Mutex::new(Instant::now() - Duration::from_secs(2)),
            bitrate_kbps: Mutex::new(Some(1300.0)),
        };

        let value = processing_graph_ingest_details(&ingest);

        assert_eq!(value["bytesReceived"], 4096);
        assert_eq!(value["bitrateKbps"], 0.0);
        assert_eq!(value["bytesReceivedPerSec"], 0);
        assert_eq!(value["healthStatus"], "warning");
        assert!(
            value["healthReason"]
                .as_str()
                .unwrap_or("")
                .contains("SRT receive buffer")
        );
        assert!(value["srtRecvBufferPercent"].as_f64().unwrap_or(0.0) >= 99.0);
    }
}
