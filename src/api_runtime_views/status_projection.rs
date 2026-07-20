use std::sync::atomic::Ordering;

use crate::media::engine::{ActiveIngest, MediaEngine, RecentIngestOutcome};

use super::common::ingest_video_track_selection_json;
pub(super) use super::common::{
    apply_egress_retry_state_json, apply_recent_egress_instability_json, egress_runtime_json,
    output_runtime_explanation_json, reader_snapshot_json, recent_egress_runtime_json,
};

pub(super) fn active_pipeline_input_json(
    ingest: &ActiveIngest,
    recent: Option<&RecentIngestOutcome>,
    total_bytes_sent: u64,
    readers_count: usize,
    reader_metrics: Vec<serde_json::Value>,
) -> serde_json::Value {
    let metadata = ingest.metadata();
    let elapsed_secs = ingest.start_time.elapsed().as_secs_f64();
    let bytes_received = ingest.bytes_received.load(Ordering::Relaxed);
    let bitrate_kbps = MediaEngine::sample_ingest_bitrate_kbps(ingest);
    let last_progress_ms = ingest.last_progress_ms.load(Ordering::Relaxed);
    let last_progress_age_ms = if last_progress_ms > 0 {
        Some(MediaEngine::now_epoch_ms().saturating_sub(last_progress_ms))
    } else {
        None
    };
    let publish_started_at = {
        let ts = chrono::Utc::now() - chrono::Duration::seconds(elapsed_secs as i64);
        ts.to_rfc3339()
    };

    let publisher_json = serde_json::json!({
        "protocol": ingest.protocol,
        "remoteAddr": metadata.remote_addr,
        "quality": metadata.quality,
    });
    let audio_tracks = ingest
        .audio_tracks
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .iter()
        .cloned()
        .collect::<Vec<_>>();
    let probe_ready = metadata.video.is_some() || !audio_tracks.is_empty();
    let probe_status = if probe_ready { "ready" } else { "pending" };
    let probe_pending_ms = (!probe_ready).then_some((elapsed_secs * 1000.0).round() as u64);
    let (recent_disconnect_count, flapping) = MediaEngine::recent_ingest_flap_state(recent);
    let video_track_selection = ingest_video_track_selection_json(ingest);

    serde_json::json!({
        "status": "on",
        "publishStartedAt": publish_started_at,
        "probeReady": probe_ready,
        "probeStatus": probe_status,
        "probePendingMs": probe_pending_ms,
        "bytesReceived": bytes_received,
        "bytesSent": total_bytes_sent,
        "readers": readers_count,
        "readerMetrics": reader_metrics,
        "bitrateKbps": bitrate_kbps,
        "lastProgressAgeMs": last_progress_age_ms,
        "video": metadata.video,
        "videoTrackSelection": video_track_selection,
        "audio": metadata.audio,
        "audioTracks": audio_tracks,
        "publisher": publisher_json,
        "unexpectedReaders": { "count": 0 },
        "lastSessionProtocol": null,
        "lastDisconnectAt": null,
        "lastDisconnectAgeMs": null,
        "lastDisconnectReason": null,
        "lastFailurePhase": null,
        "recentDisconnectError": false,
        "recentDisconnectCount": recent_disconnect_count,
        "flapping": flapping,
        "disconnectGraceActive": false,
        "disconnectGraceRemainingMs": null,
        "lastRemoteAddr": null,
        "lastSessionBytesReceived": null
    })
}

pub(super) fn active_pipeline_input_summary_json(
    ingest: &ActiveIngest,
    total_bytes_sent: u64,
    readers_count: usize,
) -> serde_json::Value {
    let metadata = ingest.metadata();
    let elapsed_secs = ingest.start_time.elapsed().as_secs_f64();
    let bytes_received = ingest.bytes_received.load(Ordering::Relaxed);
    let bitrate_kbps = if elapsed_secs > 1.0 {
        Some((bytes_received as f64 * 8.0) / (elapsed_secs * 1000.0))
    } else {
        None
    };
    let publish_started_at = {
        let ts = chrono::Utc::now() - chrono::Duration::seconds(elapsed_secs as i64);
        ts.to_rfc3339()
    };
    let audio_tracks = ingest
        .audio_tracks
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let probe_ready = metadata.video.is_some() || !audio_tracks.is_empty();
    let probe_status = if probe_ready { "ready" } else { "pending" };
    let probe_pending_ms = (!probe_ready).then_some((elapsed_secs * 1000.0).round() as u64);

    serde_json::json!({
        "status": "on",
        "publishStartedAt": publish_started_at,
        "probeReady": probe_ready,
        "probeStatus": probe_status,
        "probePendingMs": probe_pending_ms,
        "bytesReceived": bytes_received,
        "bytesSent": total_bytes_sent,
        "readers": readers_count,
        "bitrateKbps": bitrate_kbps,
        "publisher": {
            "protocol": ingest.protocol,
            "remoteAddr": metadata.remote_addr,
        },
        "disconnectGraceActive": false,
        "disconnectGraceRemainingMs": null,
    })
}

pub(super) fn inactive_pipeline_input_json(
    recent: Option<&RecentIngestOutcome>,
    total_bytes_sent: u64,
    readers_count: usize,
    reader_metrics: Vec<serde_json::Value>,
    disconnect_grace_ms: u64,
) -> serde_json::Value {
    let last_disconnect_age_ms =
        recent.map(|recent| MediaEngine::now_epoch_ms().saturating_sub(recent.disconnected_at_ms));
    let disconnect_grace_remaining_ms = if disconnect_grace_ms == 0 {
        None
    } else {
        last_disconnect_age_ms.and_then(|age_ms| disconnect_grace_ms.checked_sub(age_ms))
    };
    let (recent_disconnect_count, flapping) = MediaEngine::recent_ingest_flap_state(recent);
    serde_json::json!({
        "status": "off",
        "probeReady": false,
        "probeStatus": if recent.is_some_and(|recent| recent.had_error) { "failed" } else { "off" },
        "probePendingMs": null,
        "bytesReceived": 0,
        "bytesSent": total_bytes_sent,
        "readers": readers_count,
        "readerMetrics": reader_metrics,
        "publisher": null,
        "unexpectedReaders": { "count": 0 },
        "lastSessionProtocol": recent.map(|recent| recent.protocol.clone()),
        "lastDisconnectAt": recent.and_then(|recent| MediaEngine::epoch_ms_to_rfc3339(recent.disconnected_at_ms)),
        "lastDisconnectAgeMs": last_disconnect_age_ms,
        "lastDisconnectReason": recent.and_then(|recent| recent.reason.clone()),
        "lastFailurePhase": recent.and_then(|recent| recent.failure_phase.clone()),
        "recentDisconnectError": recent.is_some_and(|recent| recent.had_error),
        "recentDisconnectCount": recent_disconnect_count,
        "flapping": flapping,
        "disconnectGraceActive": disconnect_grace_remaining_ms.is_some(),
        "disconnectGraceRemainingMs": disconnect_grace_remaining_ms,
        "lastRemoteAddr": recent.and_then(|recent| recent.remote_addr.clone()),
        "lastSessionBytesReceived": recent.map(|recent| recent.bytes_received)
    })
}

pub(super) fn inactive_pipeline_input_summary_json(
    recent: Option<&RecentIngestOutcome>,
    total_bytes_sent: u64,
    readers_count: usize,
    disconnect_grace_ms: u64,
) -> serde_json::Value {
    let last_disconnect_age_ms =
        recent.map(|recent| MediaEngine::now_epoch_ms().saturating_sub(recent.disconnected_at_ms));
    let disconnect_grace_remaining_ms = if disconnect_grace_ms == 0 {
        None
    } else {
        last_disconnect_age_ms.and_then(|age_ms| disconnect_grace_ms.checked_sub(age_ms))
    };

    serde_json::json!({
        "status": "off",
        "probeReady": false,
        "probeStatus": if recent.is_some_and(|recent| recent.had_error) { "failed" } else { "off" },
        "probePendingMs": null,
        "bytesReceived": 0,
        "bytesSent": total_bytes_sent,
        "readers": readers_count,
        "bitrateKbps": serde_json::Value::Null,
        "publisher": serde_json::Value::Null,
        "disconnectGraceActive": disconnect_grace_remaining_ms.is_some(),
        "disconnectGraceRemainingMs": disconnect_grace_remaining_ms,
    })
}

pub(super) fn hls_preview_json(
    active: bool,
    persistent_consumers: u64,
    last_access_age_ms: Option<u64>,
    segments: usize,
    playlist_bytes: usize,
) -> serde_json::Value {
    serde_json::json!({
        "active": active,
        "persistentConsumers": persistent_consumers,
        "lastAccessAgeMs": last_access_age_ms,
        "segments": segments,
        "playlistBytes": playlist_bytes,
    })
}

pub(super) fn pipeline_health_json(
    input: serde_json::Value,
    outputs: serde_json::Map<String, serde_json::Value>,
    recording_enabled: bool,
    recording_active: bool,
    hls_preview: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "input": input,
        "outputs": serde_json::Value::Object(outputs),
        "recording": { "enabled": recording_enabled, "active": recording_active },
        "hlsPreview": hls_preview,
    })
}

pub(super) fn pipeline_health_summary_json(
    input: serde_json::Value,
    outputs: serde_json::Map<String, serde_json::Value>,
    recording_enabled: bool,
    recording_active: bool,
) -> serde_json::Value {
    serde_json::json!({
        "input": input,
        "outputs": serde_json::Value::Object(outputs),
        "recording": { "enabled": recording_enabled, "active": recording_active },
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::engine::RecentIngestOutcome;
    use crate::media::input_gate::InputPacketGate;
    use crate::media::stage_metrics::StageMetrics;
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex, RwLock};
    use std::time::{Duration, Instant};

    fn active_ingest(
        protocol: &str,
        bytes_received: u64,
        previous_bytes_received: u64,
        last_progress_ms: u64,
    ) -> ActiveIngest {
        ActiveIngest {
            attempt_id: 1,
            pipeline_id: "pipeline".to_string(),
            input_id: "input".to_string(),
            stream_key: "stream".to_string(),
            gate: Arc::new(InputPacketGate::active()),
            start_time: Instant::now() - Duration::from_secs(10),
            protocol: protocol.to_string(),
            bytes_received: Arc::new(AtomicU64::new(bytes_received)),
            metrics: Arc::new(StageMetrics::new()),
            last_progress_ms: Arc::new(AtomicU64::new(last_progress_ms)),
            metadata: RwLock::new(crate::media::engine::IngestMetadata {
                remote_addr: Some("127.0.0.1:9000".to_string()),
                ..Default::default()
            }),
            audio_tracks: Mutex::new(Arc::new(Vec::new())),
            keyframe_times: Arc::new(Mutex::new(Vec::new())),
            video_sequence_header: Mutex::new(None),
            audio_sequence_header: Mutex::new(None),
            prev_bytes_received: AtomicU64::new(previous_bytes_received),
            prev_sample_time: Mutex::new(Instant::now() - Duration::from_secs(2)),
            bitrate_kbps: Mutex::new(Some(1300.0)),
        }
    }

    #[test]
    fn inactive_input_reports_disconnect_grace() {
        let recent = RecentIngestOutcome {
            protocol: "srt".to_string(),
            disconnected_at_ms: MediaEngine::now_epoch_ms() - 2_000,
            first_disconnect_at_ms: MediaEngine::now_epoch_ms() - 2_000,
            disconnect_count: 1,
            reason: Some("socket closed".to_string()),
            failure_phase: Some("ingest".to_string()),
            had_error: true,
            remote_addr: Some("10.0.0.1:9000".to_string()),
            bytes_received: 1234,
        };

        let value = inactive_pipeline_input_json(
            Some(&recent),
            5678,
            2,
            vec![serde_json::json!({"name": "preview"})],
            5_000,
        );

        assert_eq!(value["status"], "off");
        assert_eq!(value["probeStatus"], "failed");
        assert_eq!(value["bytesSent"], 5678);
        assert_eq!(value["disconnectGraceActive"], true);
        assert!(value["disconnectGraceRemainingMs"].as_u64().unwrap_or(0) > 0);
        assert_eq!(value["recentDisconnectCount"], 1);
        assert_eq!(value["flapping"], false);
        assert_eq!(value["lastSessionProtocol"], "srt");
    }

    #[test]
    fn active_input_surfaces_recent_flapping_without_old_disconnect_fields() {
        let ingest = active_ingest("rtmp", 0, 0, 0);
        let recent = RecentIngestOutcome {
            protocol: "rtmp".to_string(),
            disconnected_at_ms: MediaEngine::now_epoch_ms(),
            first_disconnect_at_ms: MediaEngine::now_epoch_ms() - 3_000,
            disconnect_count: 2,
            reason: Some("publisher disconnected".to_string()),
            failure_phase: Some("disconnect".to_string()),
            had_error: false,
            remote_addr: Some("127.0.0.1:1935".to_string()),
            bytes_received: 2048,
        };

        let value = active_pipeline_input_json(&ingest, Some(&recent), 0, 0, Vec::new());

        assert_eq!(value["status"], "on");
        assert_eq!(value["recentDisconnectCount"], 2);
        assert_eq!(value["flapping"], true);
        assert!(value["lastSessionProtocol"].is_null());
        assert!(value["lastDisconnectReason"].is_null());
        assert!(value["lastFailurePhase"].is_null());
        assert!(value["lastDisconnectAt"].is_null());
    }

    #[test]
    fn active_input_reports_zero_rate_when_total_bytes_stop_advancing() {
        let now_ms = MediaEngine::now_epoch_ms();
        let ingest = active_ingest("srt", 4096, 4096, now_ms.saturating_sub(5_000));

        let value = active_pipeline_input_json(&ingest, None, 0, 0, Vec::new());

        assert_eq!(value["bytesReceived"], 4096);
        assert_eq!(value["bitrateKbps"], 0.0);
        assert!(value["lastProgressAgeMs"].as_u64().unwrap_or(0) >= 5_000);
    }

    #[test]
    fn active_input_surfaces_single_video_selection_policy() {
        let ingest = active_ingest("srt", 0, 0, 0);
        {
            let mut metadata = ingest.metadata.write().unwrap_or_else(|e| e.into_inner());
            metadata.video = Some(crate::media::metadata::VideoMeta {
                codec: "h264".to_string(),
                ..Default::default()
            });
            metadata.selected_video_track_index = Some(0);
            metadata.video_track_count = 2;
        }

        let value = active_pipeline_input_json(&ingest, None, 0, 0, Vec::new());

        assert_eq!(value["videoTrackSelection"]["mode"], "firstVideoOnly");
        assert_eq!(value["videoTrackSelection"]["selectedTrackIndex"], 0);
        assert_eq!(value["videoTrackSelection"]["availableTrackCount"], 2);
        assert_eq!(value["videoTrackSelection"]["ignoredTrackCount"], 1);
    }

    #[test]
    fn health_view_wraps_input_outputs_recording_and_hls() {
        let mut outputs = serde_json::Map::new();
        outputs.insert(
            "out-1".to_string(),
            serde_json::json!({"status": "running"}),
        );

        let value = pipeline_health_json(
            serde_json::json!({"status": "on"}),
            outputs,
            true,
            false,
            hls_preview_json(true, 1, Some(25), 3, 1024),
        );

        assert_eq!(value["input"]["status"], "on");
        assert_eq!(value["outputs"]["out-1"]["status"], "running");
        assert_eq!(value["recording"]["enabled"], true);
        assert_eq!(value["hlsPreview"]["segments"], 3);
    }
}
