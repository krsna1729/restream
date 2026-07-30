use std::sync::atomic::Ordering;

use crate::media::engine::{
    ActiveEgress, ActiveIngest, EgressRetryState, MediaEngine, RecentEgressOutcome,
};
use crate::media::ring_buffer::RingBuffer;

pub(super) fn egress_runtime_json(
    egress: &ActiveEgress,
    include_target_url: bool,
    has_ingest: bool,
    blocked_by: Option<&crate::runtime::stage::StageRuntimeSnapshot>,
) -> serde_json::Value {
    let last_progress_ms = egress.last_progress_ms.load(Ordering::Relaxed);
    let last_error_ms = egress.last_error_ms.load(Ordering::Relaxed);
    let now_ms = MediaEngine::now_epoch_ms();
    let status = MediaEngine::egress_effective_status(egress, has_ingest);
    let mut value = serde_json::json!({
        "outputId": egress.output_id.clone(),
        "outputName": egress.output_name.clone(),
        "encoding": egress.encoding.clone(),
        "pipelineId": egress.pipeline_id.clone(),
        "protocol": egress.protocol.clone(),
        "targetAddr": egress.target_addr.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        "status": status,
        "rawStatus": egress.status.as_str(),
        "phase": egress.phase.lock().unwrap_or_else(|e| e.into_inner()).as_str(),
        "terminalStage": egress.terminal_stage_key.as_ref().map(|k| k.to_string()),
        "uptimeSecs": egress.start_instant.elapsed().as_secs_f64(),
        "bytesOut": egress.bytes_sent.load(Ordering::Relaxed),
        "resyncCount": egress.resync_count.load(Ordering::Relaxed),
        "lastProgressAt": MediaEngine::epoch_ms_to_rfc3339(last_progress_ms),
        "lastProgressAgeMs": (last_progress_ms > 0).then(|| now_ms.saturating_sub(last_progress_ms)),
        "lastError": egress.last_error.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        "lastErrorAt": MediaEngine::epoch_ms_to_rfc3339(last_error_ms),
        "failurePhase": egress.failure_phase.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        "blockedBy": blocked_by.map(super::stage_projection::stage_runtime_snapshot_json),
        "recentFailureCount": 0,
        "flapping": false,
        "retrying": false,
        "retryAttempts": serde_json::Value::Null,
        "retryBackoffMs": serde_json::Value::Null,
        "nextRetryAt": serde_json::Value::Null,
        "retryRemainingMs": serde_json::Value::Null,
        "quality": egress.quality.lock().unwrap_or_else(|e| e.into_inner()).clone(),
        "metrics": egress.metrics.snapshot(),
        "fabric": egress.is_fabric,
        "shardId": egress.shard_id,
    });
    if include_target_url {
        value["targetUrl"] = serde_json::Value::String(egress.target_url.clone());
    }
    value
}

pub(super) fn output_runtime_explanation_json(
    explanation: &crate::runtime::output::OutputRuntimeExplanation,
) -> serde_json::Value {
    serde_json::json!({
        "outputId": explanation.output_id.to_string(),
        "outputName": explanation.output_name,
        "encoding": explanation.encoding,
        "url": explanation.url,
        "phase": explanation.phase.as_str(),
        "terminalStage": explanation.terminal_stage.as_ref().map(|k| k.to_string()),
        "blockedBy": explanation.blocked_by.as_ref().map(|k| k.to_string()),
    })
}

pub(super) fn recent_egress_runtime_json(
    outcome: &RecentEgressOutcome,
    include_target_url: bool,
) -> serde_json::Value {
    let now_ms = MediaEngine::now_epoch_ms();
    let mut value = serde_json::json!({
        "outputId": outcome.output_id,
        "pipelineId": outcome.pipeline_id,
        "protocol": outcome.protocol,
        "targetAddr": outcome.target_addr,
        "status": outcome.status.as_str(),
        "rawStatus": outcome.raw_status.as_str(),
        "phase": outcome.phase.as_str(),
        "uptimeSecs": outcome.uptime_secs,
        "bytesOut": outcome.bytes_sent,
        "resyncCount": outcome.resync_count,
        "lastProgressAt": MediaEngine::epoch_ms_to_rfc3339(outcome.last_progress_ms),
        "lastProgressAgeMs": (outcome.last_progress_ms > 0).then(|| now_ms.saturating_sub(outcome.last_progress_ms)),
        "lastError": outcome.last_error,
        "lastErrorAt": MediaEngine::epoch_ms_to_rfc3339(outcome.last_error_ms),
        "failurePhase": outcome.failure_phase,
        "recentFailureCount": 0,
        "flapping": false,
        "retrying": false,
        "retryAttempts": serde_json::Value::Null,
        "retryBackoffMs": serde_json::Value::Null,
        "nextRetryAt": serde_json::Value::Null,
        "retryRemainingMs": serde_json::Value::Null,
        "quality": outcome.quality,
        "metrics": outcome.metrics,
        "endedAt": MediaEngine::epoch_ms_to_rfc3339(outcome.ended_at_ms),
        "endedAgeMs": now_ms.saturating_sub(outcome.ended_at_ms),
    });
    if include_target_url {
        value["targetUrl"] = serde_json::Value::String(outcome.target_url.clone());
    }
    value
}

pub(super) fn apply_recent_egress_instability_json(
    value: &mut serde_json::Value,
    recent: Option<&RecentEgressOutcome>,
) {
    let (recent_failure_count, flapping) = MediaEngine::recent_egress_flap_state(recent);
    value["recentFailureCount"] = serde_json::json!(recent_failure_count);
    value["flapping"] = serde_json::Value::Bool(flapping);
}

pub(super) fn apply_egress_retry_state_json(
    value: &mut serde_json::Value,
    retry: Option<&EgressRetryState>,
) {
    let Some(retry) = retry else {
        return;
    };

    let remaining_ms = retry
        .next_retry_at_ms
        .saturating_sub(MediaEngine::now_epoch_ms());
    value["status"] = serde_json::Value::String("retrying".to_string());
    value["retrying"] = serde_json::Value::Bool(true);
    value["retryAttempts"] = serde_json::json!(retry.attempts);
    value["retryBackoffMs"] = serde_json::json!(retry.backoff_ms);
    value["nextRetryAt"] = MediaEngine::epoch_ms_to_rfc3339(retry.next_retry_at_ms)
        .map(serde_json::Value::String)
        .unwrap_or(serde_json::Value::Null);
    value["retryRemainingMs"] = serde_json::json!(remaining_ms);
}

pub(crate) fn probe_snapshot(pipeline_id: &str, ingest: &ActiveIngest) -> serde_json::Value {
    let metadata = ingest.metadata();
    let elapsed = ingest.start_time.elapsed().as_secs_f64();
    let bytes = ingest.bytes_received.load(Ordering::Relaxed);
    let bitrate_kbps = if elapsed > 1.0 {
        Some((bytes as f64 * 8.0) / (elapsed * 1000.0))
    } else {
        None
    };

    let audio_tracks: Vec<serde_json::Value> = {
        let tracks = ingest
            .audio_tracks
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if tracks.is_empty() {
            metadata
                .audio
                .as_ref()
                .map(|a| vec![serde_json::to_value(a).unwrap_or_default()])
                .unwrap_or_default()
        } else {
            tracks
                .iter()
                .map(|a| serde_json::to_value(a).unwrap_or_default())
                .collect()
        }
    };

    let gop = {
        let times = ingest
            .keyframe_times
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if times.len() >= 2 {
            let intervals: Vec<f64> = times
                .windows(2)
                .map(|w| ((w[1] - w[0]) as f64 / 1000.0).max(0.0))
                .collect();
            let avg = intervals.iter().sum::<f64>() / intervals.len() as f64;
            Some(serde_json::json!({
                "averageIntervalSec": (avg * 100.0).round() / 100.0,
                "keyframeCount": times.len(),
            }))
        } else {
            None
        }
    };

    let video_track_selection = ingest_video_track_selection_json(ingest);

    serde_json::json!({
        "pipelineId": pipeline_id,
        "ingest": {
            "protocol": ingest.protocol,
            "remoteAddr": metadata.remote_addr,
            "uptimeSeconds": (elapsed * 10.0).round() / 10.0,
            "bytesReceived": bytes,
            "bitrateKbps": bitrate_kbps.map(|b| (b * 10.0).round() / 10.0),
        },
        "video": metadata.video,
        "videoTrackSelection": video_track_selection,
        "audioTracks": audio_tracks,
        "gop": gop,
    })
}

pub(super) fn ingest_video_track_selection_json(ingest: &ActiveIngest) -> serde_json::Value {
    let metadata = ingest.metadata();
    if metadata.video_track_count == 0 {
        return serde_json::Value::Null;
    }

    serde_json::json!({
        "mode": "firstVideoOnly",
        "selectedTrackIndex": metadata.selected_video_track_index,
        "availableTrackCount": metadata.video_track_count,
        "ignoredTrackCount": metadata.video_track_count.saturating_sub(1),
    })
}

pub(super) fn ring_payload_stats_json(ring: &RingBuffer) -> serde_json::Value {
    let stats = ring.payload_stats();
    serde_json::json!({
        "slots": stats.slots,
        "payloadBytes": stats.payload_bytes,
        "videoBytes": stats.video_bytes,
        "audioBytes": stats.audio_bytes,
        "minPayloadBytes": stats.min_payload_bytes,
        "maxPayloadBytes": stats.max_payload_bytes,
        "avgPayloadBytes": if stats.slots > 0 {
            stats.payload_bytes as f64 / stats.slots as f64
        } else {
            0.0
        },
    })
}

pub(super) fn reader_snapshot_json(
    reader: &crate::media::ring_buffer::ReaderSnapshot,
) -> serde_json::Value {
    serde_json::json!({
        "name": reader.name,
        "readIndex": reader.read_idx,
        "writeIndex": reader.write_idx,
        "lagSlots": reader.lag_slots,
        "overflowCount": reader.overflow_count,
        "overflows": reader.overflow_count,
        "packetAgeMs": reader.packet_age_ms,
        "burstCount": reader.burst_count,
        "avgBurstSize": (reader.avg_burst_size * 10.0).round() / 10.0,
        "medianBurstSize": reader.median_burst_size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::state::{EgressPhase, EgressRuntimeStatus, EgressStatus};

    #[test]
    fn retry_state_marks_runtime_value_as_retrying() {
        let mut value = serde_json::json!({
            "status": "running",
            "retrying": false,
            "retryAttempts": serde_json::Value::Null,
            "retryBackoffMs": serde_json::Value::Null,
            "nextRetryAt": serde_json::Value::Null,
            "retryRemainingMs": serde_json::Value::Null,
        });
        let retry = EgressRetryState {
            attempts: 3,
            backoff_ms: 5_000,
            next_retry_at_ms: MediaEngine::now_epoch_ms() + 5_000,
        };

        apply_egress_retry_state_json(&mut value, Some(&retry));

        assert_eq!(value["status"], "retrying");
        assert_eq!(value["retrying"], true);
        assert_eq!(value["retryAttempts"], 3);
        assert_eq!(value["retryBackoffMs"], 5_000);
        assert!(value["retryRemainingMs"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    fn recent_egress_instability_surfaces_flapping_window() {
        let mut value = serde_json::json!({
            "status": "running",
            "recentFailureCount": 0,
            "flapping": false,
        });
        let recent = RecentEgressOutcome {
            output_id: "out-1".to_string(),
            pipeline_id: "pipe-1".to_string(),
            protocol: "rtmp".to_string(),
            target_url: "rtmp://example/live/key".to_string(),
            target_addr: None,
            status: EgressRuntimeStatus::Failed,
            raw_status: EgressStatus::Running,
            phase: EgressPhase::Failed,
            started_at: chrono::Utc::now().to_rfc3339(),
            uptime_secs: 1.5,
            bytes_sent: 2048,
            last_progress_ms: 0,
            resync_count: 0,
            last_error: Some("connection reset by peer".to_string()),
            last_error_ms: MediaEngine::now_epoch_ms(),
            failure_phase: Some("send".to_string()),
            first_failure_at_ms: MediaEngine::now_epoch_ms() - 2_000,
            failure_count: 2,
            quality: Default::default(),
            metrics: Default::default(),
            ended_at_ms: MediaEngine::now_epoch_ms() - 1_000,
        };

        apply_recent_egress_instability_json(&mut value, Some(&recent));

        assert_eq!(value["recentFailureCount"], 2);
        assert_eq!(value["flapping"], true);
    }

    #[test]
    fn ring_payload_stats_reports_zero_average_for_empty_ring() {
        let ring = RingBuffer::new(8);

        let stats = ring_payload_stats_json(&ring);

        assert_eq!(stats["slots"], 0);
        assert_eq!(stats["avgPayloadBytes"], 0.0);
    }

    #[test]
    fn egress_runtime_json_preserves_fabric_shard_and_failure_details() {
        use std::sync::Arc;
        use std::sync::atomic::AtomicU64;
        use std::time::Instant;

        let egress = ActiveEgress {
            attempt_id: 1,
            output_id: "out-fabric-1".to_string(),
            pipeline_id: "pipe-1".to_string(),
            protocol: "rtmp".to_string(),
            target_url: "rtmp://example/live/key".to_string(),
            target_addr: Arc::new(std::sync::Mutex::new(Some("127.0.0.1:1935".to_string()))),
            status: EgressStatus::Running,
            phase: Arc::new(std::sync::Mutex::new(EgressPhase::Sending)),
            started_at: chrono::Utc::now().to_rfc3339(),
            start_instant: Instant::now(),
            bytes_sent: Arc::new(AtomicU64::new(1024)),
            metrics: Arc::new(Default::default()),
            last_progress_ms: Arc::new(AtomicU64::new(MediaEngine::now_epoch_ms())),
            last_error: Arc::new(std::sync::Mutex::new(Some(
                "rtmp fabric leaf rejected".to_string(),
            ))),
            last_error_ms: Arc::new(AtomicU64::new(MediaEngine::now_epoch_ms())),
            failure_phase: Arc::new(std::sync::Mutex::new(Some(
                "rtmp_fabric_ensure".to_string(),
            ))),
            quality: Arc::new(std::sync::Mutex::new(Default::default())),
            prev_bytes_sent: AtomicU64::new(0),
            prev_sample_time: std::sync::Mutex::new(Instant::now()),
            bitrate_kbps: std::sync::Mutex::new(None),
            terminal_stage_key: None,
            output_name: "out-fabric-1".to_string(),
            encoding: "source".to_string(),
            is_fabric: true,
            shard_id: Some(2),
            resync_count: Arc::new(AtomicU64::new(3)),
        };

        let json = egress_runtime_json(&egress, true, true, None);
        assert_eq!(json["fabric"], true);
        assert_eq!(json["shardId"], 2);
        assert_eq!(json["lastError"], "rtmp fabric leaf rejected");
        assert_eq!(json["failurePhase"], "rtmp_fabric_ensure");
        assert_eq!(json["resyncCount"], 3);
        assert!(json["lastProgressAgeMs"].as_u64().is_some());
    }
}
