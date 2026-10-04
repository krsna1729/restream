use std::sync::atomic::Ordering;

use crate::media::engine::{
    ActiveEgress, ActiveIngest, EgressRetryState, MediaEngine, RecentEgressOutcome,
};
use crate::media::ring_buffer::RingBuffer;

/// Retry state as an output view reports it; computed once per output.
struct RetryView {
    attempts: u32,
    backoff_ms: u64,
    next_retry_at: Option<String>,
    remaining_ms: u64,
}

impl RetryView {
    fn new(retry: &EgressRetryState) -> Self {
        Self {
            attempts: retry.attempts,
            backoff_ms: retry.backoff_ms,
            next_retry_at: MediaEngine::epoch_ms_to_rfc3339(retry.next_retry_at_ms),
            remaining_ms: retry
                .next_retry_at_ms
                .saturating_sub(MediaEngine::now_epoch_ms()),
        }
    }
}

/// Generates the status/retry/totals setters shared by the active and recent
/// output views (same JSON keys on both).
macro_rules! output_view_patches {
    ($view:ty) => {
        impl $view {
            pub(super) fn apply_recent_instability(
                &mut self,
                recent: Option<&RecentEgressOutcome>,
            ) {
                let (count, flapping) = MediaEngine::recent_egress_flap_state(recent);
                self.recent_failure_count = count;
                self.flapping = flapping;
            }

            pub(super) fn apply_retry_state(&mut self, retry: Option<&EgressRetryState>) {
                let Some(retry) = retry.map(RetryView::new) else {
                    return;
                };
                self.status = "retrying".to_string();
                self.retrying = true;
                self.retry_attempts = Some(retry.attempts);
                self.retry_backoff_ms = Some(retry.backoff_ms);
                self.next_retry_at = retry.next_retry_at;
                self.retry_remaining_ms = Some(retry.remaining_ms);
            }

            /// `totalSize`, `bitrateKbps` and `startedAt`, which the output
            /// status and health views add to the runtime fields.
            pub(super) fn with_totals(
                mut self,
                total_size: u64,
                bitrate_kbps: Option<f64>,
                started_at: &str,
            ) -> Self {
                self.total_size = Some(total_size);
                self.bitrate_kbps = Some(bitrate_kbps);
                self.started_at = Some(started_at.to_string());
                self
            }
        }
    };
}

/// Runtime fields of an active egress, serialized directly (no JSON tree).
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct EgressRuntimeView {
    output_id: String,
    output_name: String,
    encoding: String,
    pipeline_id: String,
    protocol: String,
    target_addr: Option<String>,
    status: String,
    raw_status: &'static str,
    phase: &'static str,
    terminal_stage: Option<String>,
    uptime_secs: f64,
    bytes_out: u64,
    resync_count: u64,
    feed_lag_units: u64,
    backpressure_reason: Option<&'static str>,
    last_progress_at: Option<String>,
    last_progress_age_ms: Option<u64>,
    last_error: Option<String>,
    last_error_at: Option<String>,
    failure_phase: Option<String>,
    pub(super) blocked_by: Option<serde_json::Value>,
    recent_failure_count: u32,
    flapping: bool,
    retrying: bool,
    retry_attempts: Option<u32>,
    retry_backoff_ms: Option<u64>,
    next_retry_at: Option<String>,
    retry_remaining_ms: Option<u64>,
    quality: crate::media::snapshots::PublisherQuality,
    metrics: crate::media::stage_metrics::StageMetricsSnapshot,
    fabric: bool,
    shard_id: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bitrate_kbps: Option<Option<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
}

output_view_patches!(EgressRuntimeView);

pub(super) fn egress_runtime_view(
    egress: &ActiveEgress,
    include_target_url: bool,
    has_ingest: bool,
    blocked_by: Option<&crate::runtime::stage::StageRuntimeSnapshot>,
) -> EgressRuntimeView {
    let last_progress_ms = egress.last_progress_ms.load(Ordering::Relaxed);
    let last_error_ms = egress.last_error_ms.load(Ordering::Relaxed);
    let now_ms = MediaEngine::now_epoch_ms();
    EgressRuntimeView {
        output_id: egress.output_id.clone(),
        output_name: egress.output_name.clone(),
        encoding: egress.encoding.clone(),
        pipeline_id: egress.pipeline_id.clone(),
        protocol: egress.protocol.clone(),
        target_addr: crate::sync::lock(&egress.target_addr).clone(),
        status: MediaEngine::egress_effective_status(egress, has_ingest),
        raw_status: egress.status.as_str(),
        phase: crate::sync::lock(&egress.phase).as_str(),
        terminal_stage: egress.terminal_stage_key.as_ref().map(|k| k.to_string()),
        uptime_secs: egress.start_instant.elapsed().as_secs_f64(),
        bytes_out: egress.bytes_sent.load(Ordering::Relaxed),
        resync_count: egress.resync_count.load(Ordering::Relaxed),
        feed_lag_units: egress.feed_lag_units.load(Ordering::Relaxed),
        backpressure_reason: *crate::sync::lock(&egress.backpressure_reason),
        last_progress_at: MediaEngine::epoch_ms_to_rfc3339(last_progress_ms),
        last_progress_age_ms: (last_progress_ms > 0)
            .then(|| now_ms.saturating_sub(last_progress_ms)),
        last_error: crate::sync::lock(&egress.last_error).clone(),
        last_error_at: MediaEngine::epoch_ms_to_rfc3339(last_error_ms),
        failure_phase: crate::sync::lock(&egress.failure_phase).clone(),
        blocked_by: blocked_by.map(super::stage_projection::stage_runtime_snapshot_json),
        recent_failure_count: 0,
        flapping: false,
        retrying: false,
        retry_attempts: None,
        retry_backoff_ms: None,
        next_retry_at: None,
        retry_remaining_ms: None,
        quality: crate::sync::lock(&egress.quality).clone(),
        metrics: egress.metrics.snapshot(),
        fabric: egress.is_fabric,
        shard_id: egress.shard_id,
        target_url: include_target_url.then(|| egress.target_url.clone()),
        total_size: None,
        bitrate_kbps: None,
        started_at: None,
    }
}

/// The runtime fields as a JSON value, for views that extend them further.
pub(super) fn egress_runtime_json(
    egress: &ActiveEgress,
    include_target_url: bool,
    has_ingest: bool,
    blocked_by: Option<&crate::runtime::stage::StageRuntimeSnapshot>,
) -> serde_json::Value {
    to_json(&egress_runtime_view(
        egress,
        include_target_url,
        has_ingest,
        blocked_by,
    ))
}

pub(super) fn to_json(view: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(view).unwrap_or(serde_json::Value::Null)
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

/// Runtime fields of an egress that recently ended, serialized directly.
#[derive(serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct RecentEgressRuntimeView {
    output_id: String,
    pipeline_id: String,
    protocol: String,
    target_addr: Option<String>,
    status: String,
    raw_status: &'static str,
    phase: &'static str,
    uptime_secs: f64,
    bytes_out: u64,
    resync_count: u64,
    feed_lag_units: u64,
    backpressure_reason: Option<&'static str>,
    last_progress_at: Option<String>,
    last_progress_age_ms: Option<u64>,
    last_error: Option<String>,
    last_error_at: Option<String>,
    failure_phase: Option<String>,
    recent_failure_count: u32,
    flapping: bool,
    retrying: bool,
    retry_attempts: Option<u32>,
    retry_backoff_ms: Option<u64>,
    next_retry_at: Option<String>,
    retry_remaining_ms: Option<u64>,
    quality: crate::media::snapshots::PublisherQuality,
    metrics: crate::media::stage_metrics::StageMetricsSnapshot,
    ended_at: Option<String>,
    ended_age_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    target_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    total_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    bitrate_kbps: Option<Option<f64>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    started_at: Option<String>,
}

output_view_patches!(RecentEgressRuntimeView);

pub(super) fn recent_egress_runtime_view(
    outcome: &RecentEgressOutcome,
    include_target_url: bool,
) -> RecentEgressRuntimeView {
    let now_ms = MediaEngine::now_epoch_ms();
    RecentEgressRuntimeView {
        output_id: outcome.output_id.clone(),
        pipeline_id: outcome.pipeline_id.clone(),
        protocol: outcome.protocol.clone(),
        target_addr: outcome.target_addr.clone(),
        status: outcome.status.as_str().to_string(),
        raw_status: outcome.raw_status.as_str(),
        phase: outcome.phase.as_str(),
        uptime_secs: outcome.uptime_secs,
        bytes_out: outcome.bytes_sent,
        resync_count: outcome.resync_count,
        feed_lag_units: outcome.feed_lag_units,
        backpressure_reason: outcome.backpressure_reason,
        last_progress_at: MediaEngine::epoch_ms_to_rfc3339(outcome.last_progress_ms),
        last_progress_age_ms: (outcome.last_progress_ms > 0)
            .then(|| now_ms.saturating_sub(outcome.last_progress_ms)),
        last_error: outcome.last_error.clone(),
        last_error_at: MediaEngine::epoch_ms_to_rfc3339(outcome.last_error_ms),
        failure_phase: outcome.failure_phase.clone(),
        recent_failure_count: 0,
        flapping: false,
        retrying: false,
        retry_attempts: None,
        retry_backoff_ms: None,
        next_retry_at: None,
        retry_remaining_ms: None,
        quality: outcome.quality.clone(),
        metrics: outcome.metrics,
        ended_at: MediaEngine::epoch_ms_to_rfc3339(outcome.ended_at_ms),
        ended_age_ms: now_ms.saturating_sub(outcome.ended_at_ms),
        target_url: include_target_url.then(|| outcome.target_url.clone()),
        total_size: None,
        bitrate_kbps: None,
        started_at: None,
    }
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
        let tracks = crate::sync::lock(&ingest.audio_tracks);
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
        let times = crate::sync::lock(&ingest.keyframe_times);
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

    fn failed_recent_outcome() -> RecentEgressOutcome {
        RecentEgressOutcome {
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
            feed_lag_units: 0,
            backpressure_reason: None,
            last_error: Some("connection reset by peer".to_string()),
            last_error_ms: MediaEngine::now_epoch_ms(),
            failure_phase: Some("send".to_string()),
            first_failure_at_ms: MediaEngine::now_epoch_ms() - 2_000,
            failure_count: 2,
            quality: Default::default(),
            metrics: Default::default(),
            ended_at_ms: MediaEngine::now_epoch_ms() - 1_000,
        }
    }

    #[test]
    fn retry_state_marks_runtime_view_as_retrying() {
        let mut view = recent_egress_runtime_view(&failed_recent_outcome(), false);
        let retry = EgressRetryState {
            attempts: 3,
            backoff_ms: 5_000,
            next_retry_at_ms: MediaEngine::now_epoch_ms() + 5_000,
        };

        view.apply_retry_state(Some(&retry));
        let value = to_json(&view);

        assert_eq!(value["status"], "retrying");
        assert_eq!(value["retrying"], true);
        assert_eq!(value["retryAttempts"], 3);
        assert_eq!(value["retryBackoffMs"], 5_000);
        assert!(value["nextRetryAt"].is_string());
        assert!(value["retryRemainingMs"].as_u64().unwrap_or(0) > 0);
    }

    #[test]
    fn recent_egress_instability_surfaces_flapping_window() {
        let recent = failed_recent_outcome();
        let mut view = recent_egress_runtime_view(&recent, false);

        view.apply_recent_instability(Some(&recent));
        let value = to_json(&view);

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
            feed_lag_units: Arc::new(AtomicU64::new(7)),
            backpressure_reason: Arc::new(std::sync::Mutex::new(Some("backpressured"))),
        };

        let json = egress_runtime_json(&egress, true, true, None);
        assert_eq!(json["fabric"], true);
        assert_eq!(json["shardId"], 2);
        assert_eq!(json["lastError"], "rtmp fabric leaf rejected");
        assert_eq!(json["failurePhase"], "rtmp_fabric_ensure");
        assert_eq!(json["resyncCount"], 3);
        assert_eq!(json["feedLagUnits"], 7);
        assert_eq!(json["backpressureReason"], "backpressured");
        assert!(json["lastProgressAgeMs"].as_u64().is_some());
    }
}
