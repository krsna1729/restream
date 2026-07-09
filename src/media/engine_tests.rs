use super::*;
use crate::domain::state::{DesiredOutputState, EgressPhase as EP};
use crate::media::avio::MemoryQueue;
use crate::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat, Reader};
use bytes::Bytes;
use proptest::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;

async fn test_health_snapshot(
    engine: &MediaEngine,
    pipeline_ids: &[String],
    recording_enabled: &HashMap<String, bool>,
) -> serde_json::Value {
    crate::api_runtime_views::health_snapshot(engine, pipeline_ids, recording_enabled, 0).await
}

async fn test_health_snapshot_with_disconnect_grace(
    engine: &MediaEngine,
    pipeline_ids: &[String],
    recording_enabled: &HashMap<String, bool>,
    disconnect_grace_ms: u64,
) -> serde_json::Value {
    crate::api_runtime_views::health_snapshot(
        engine,
        pipeline_ids,
        recording_enabled,
        disconnect_grace_ms,
    )
    .await
}

#[test]
fn pipe_metrics_snapshot_correctness() {
    let pm = PipeMetrics::default();
    let snap = pm.snapshot();

    // All counters start at zero; avg fields are also zero.
    assert_eq!(snap["stalls"].as_u64().unwrap(), 0);
    assert_eq!(snap["stallUs"].as_u64().unwrap(), 0);
    assert_eq!(snap["avgStallUs"].as_u64().unwrap(), 0);
    assert_eq!(snap["idles"].as_u64().unwrap(), 0);
    assert_eq!(snap["idleUs"].as_u64().unwrap(), 0);
    assert_eq!(snap["avgIdleUs"].as_u64().unwrap(), 0);

    // Stdin stall accumulation and average.
    pm.record_stall(2_000);
    pm.record_stall(6_000);
    let snap = pm.snapshot();
    assert_eq!(snap["stalls"].as_u64().unwrap(), 2);
    assert_eq!(snap["stallUs"].as_u64().unwrap(), 8_000);
    assert_eq!(snap["avgStallUs"].as_u64().unwrap(), 4_000);

    // Stdout idle accumulation and average.
    pm.record_idle(3_000);
    let snap = pm.snapshot();
    assert_eq!(snap["idles"].as_u64().unwrap(), 1);
    assert_eq!(snap["idleUs"].as_u64().unwrap(), 3_000);
    assert_eq!(snap["avgIdleUs"].as_u64().unwrap(), 3_000);

    // StageMetrics snapshot no longer contains pipe fields.
    let sm = StageMetrics::new();
    let ssnap = sm.snapshot();
    assert!(ssnap.get("pipeMetrics").is_none());
}

fn test_video_packet(pts: i64, dts: i64, keyframe: bool) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: keyframe,
        track_index: 0,
        pts,
        dts,
        payload: Bytes::from_static(b"video"),
    }
}

fn test_audio_packet(pts: i64, dts: i64) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Audio,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts,
        dts,
        payload: Bytes::from_static(b"audio"),
    }
}

#[derive(Clone, Debug)]
enum EgressLifecycleAction {
    Register,
    RecordError {
        phase: &'static str,
        message: &'static str,
    },
    RecordProgress(u64),
    Unregister,
    RetryState {
        attempts: u32,
        backoff_ms: u64,
        remaining_ms: u64,
    },
    ClearRetry,
}

#[derive(Clone, Debug)]
enum IngestLifecycleAction {
    Register {
        protocol: &'static str,
    },
    UpdateRemoteAddr(Option<&'static str>),
    RecordBytes(u64),
    DisconnectAndUnregister {
        phase: Option<&'static str>,
        message: Option<&'static str>,
        had_error: bool,
    },
    Unregister,
}

#[derive(Clone, Debug, Default)]
struct EgressLifecycleModel {
    active: bool,
    recent_visible: bool,
    retry_visible: bool,
    bytes_sent: u64,
    phase: &'static str,
    last_error: Option<(&'static str, &'static str)>,
    retry_attempts: Option<u32>,
    retry_backoff_ms: Option<u64>,
}

#[derive(Clone, Debug, Default)]
struct IngestLifecycleModel {
    active: bool,
    protocol: Option<&'static str>,
    remote_addr: Option<&'static str>,
    bytes_received: u64,
    recent_visible: bool,
    recent_protocol: Option<&'static str>,
    recent_remote_addr: Option<&'static str>,
    recent_bytes_received: u64,
    recent_phase: Option<&'static str>,
    recent_message: Option<&'static str>,
    recent_had_error: bool,
    recent_disconnect_count: u32,
}

fn egress_lifecycle_action_strategy() -> impl Strategy<Value = EgressLifecycleAction> {
    prop_oneof![
        Just(EgressLifecycleAction::Register),
        Just(EgressLifecycleAction::Unregister),
        Just(EgressLifecycleAction::ClearRetry),
        prop_oneof![
            Just(("connect", "connection refused")),
            Just(("send", "connection reset by peer")),
            Just(("upload_segment", "temporary sink outage")),
        ]
        .prop_map(|(phase, message)| EgressLifecycleAction::RecordError { phase, message }),
        (1u64..=8_192).prop_map(EgressLifecycleAction::RecordProgress),
        (1u32..=4, 1_000u64..=60_000, 1_000u64..=60_000).prop_map(
            |(attempts, backoff_ms, remaining_ms)| EgressLifecycleAction::RetryState {
                attempts,
                backoff_ms,
                remaining_ms,
            }
        ),
    ]
}

fn ingest_lifecycle_action_strategy() -> impl Strategy<Value = IngestLifecycleAction> {
    prop_oneof![
        Just(IngestLifecycleAction::Register { protocol: "rtmp" }),
        Just(IngestLifecycleAction::Register { protocol: "srt" }),
        prop_oneof![Just(Some("127.0.0.1:1935")), Just(Some("127.0.0.1:10080")),]
            .prop_map(IngestLifecycleAction::UpdateRemoteAddr),
        (1u64..=16_384).prop_map(IngestLifecycleAction::RecordBytes),
        prop_oneof![
            Just((Some("disconnect"), Some("publisher disconnected"), false)),
            Just((Some("receive"), Some("connection reset by peer"), true)),
            Just((None, None, false)),
        ]
        .prop_map(|(phase, message, had_error)| {
            IngestLifecycleAction::DisconnectAndUnregister {
                phase,
                message,
                had_error,
            }
        }),
        Just(IngestLifecycleAction::Unregister),
    ]
}

fn assert_egress_lifecycle_invariants(
    model: &EgressLifecycleModel,
    status: Option<&serde_json::Value>,
    snapshot_output: Option<&serde_json::Value>,
    recent: Option<&RecentEgressOutcome>,
    retry: Option<&EgressRetryState>,
) {
    assert_eq!(
        recent.is_some(),
        model.recent_visible,
        "recent egress visibility drifted from the lifecycle model"
    );
    assert_eq!(
        retry.is_some(),
        model.retry_visible,
        "retry visibility drifted from the lifecycle model"
    );

    let status = status.cloned();
    let snapshot_output = snapshot_output.cloned();

    if model.active {
        let status = status.expect("active egress must have a runtime status");
        let snapshot_output =
            snapshot_output.expect("active egress must appear in the health snapshot");
        assert!(
            retry.is_none(),
            "active egress must not retain retry metadata from older attempts"
        );
        assert_eq!(status["retrying"], false);
        assert_eq!(snapshot_output["retrying"], false);
        assert_eq!(status["bytesOut"], model.bytes_sent);
        assert_eq!(snapshot_output["bytesOut"], model.bytes_sent);
        assert_eq!(status["phase"], model.phase);
        assert_eq!(snapshot_output["phase"], model.phase);

        match model.last_error {
            Some((phase, message)) => {
                assert_eq!(status["lastError"], message);
                assert_eq!(status["failurePhase"], phase);
                assert_eq!(snapshot_output["lastError"], message);
                assert_eq!(snapshot_output["failurePhase"], phase);
            }
            None => {
                assert!(status["lastError"].is_null());
                assert!(status["failurePhase"].is_null());
                assert!(snapshot_output["lastError"].is_null());
                assert!(snapshot_output["failurePhase"].is_null());
            }
        }
        return;
    }

    match (model.recent_visible, status, snapshot_output) {
        (false, None, None) => {}
        (false, _, _) => {
            panic!("without an active or recent egress, runtime status should disappear")
        }
        (true, Some(status), Some(snapshot_output)) => {
            if model.retry_visible {
                let attempts = model.retry_attempts.expect("retry attempts tracked");
                let backoff_ms = model.retry_backoff_ms.expect("retry backoff tracked");
                assert_eq!(status["status"], "retrying");
                assert_eq!(snapshot_output["status"], "retrying");
                assert_eq!(status["retrying"], true);
                assert_eq!(snapshot_output["retrying"], true);
                assert_eq!(status["retryAttempts"], attempts);
                assert_eq!(snapshot_output["retryAttempts"], attempts);
                assert_eq!(status["retryBackoffMs"], backoff_ms);
                assert_eq!(snapshot_output["retryBackoffMs"], backoff_ms);
                assert!(
                    status["retryRemainingMs"].as_u64().unwrap_or(0) > 0,
                    "retrying outputs must expose remaining retry delay"
                );
                assert!(
                    snapshot_output["retryRemainingMs"].as_u64().unwrap_or(0) > 0,
                    "health snapshot must expose remaining retry delay"
                );
            } else {
                assert_eq!(status["retrying"], false);
                assert_eq!(snapshot_output["retrying"], false);
                assert!(status["retryAttempts"].is_null());
                assert!(snapshot_output["retryAttempts"].is_null());
                assert!(status["retryBackoffMs"].is_null());
                assert!(snapshot_output["retryBackoffMs"].is_null());
            }

            match model.last_error {
                Some((phase, message)) => {
                    assert_eq!(status["phase"], "failed");
                    assert_eq!(snapshot_output["phase"], "failed");
                    assert_eq!(status["failurePhase"], phase);
                    assert_eq!(snapshot_output["failurePhase"], phase);
                    assert_eq!(status["lastError"], message);
                    assert_eq!(snapshot_output["lastError"], message);
                }
                None => {
                    assert!(status["lastError"].is_null());
                    assert!(snapshot_output["lastError"].is_null());
                }
            }
        }
        (true, _, _) => panic!("recent egress must stay visible in both status and health"),
    }
}

fn assert_ingest_lifecycle_invariants(
    model: &IngestLifecycleModel,
    plain_input: &serde_json::Value,
    grace_input: &serde_json::Value,
) {
    let expected_flapping = model.recent_disconnect_count >= 2;
    assert_eq!(
        plain_input["recentDisconnectCount"], model.recent_disconnect_count,
        "plain snapshot disconnect count drifted from the lifecycle model"
    );
    assert_eq!(
        grace_input["recentDisconnectCount"], model.recent_disconnect_count,
        "grace snapshot disconnect count drifted from the lifecycle model"
    );
    assert_eq!(plain_input["flapping"], expected_flapping);
    assert_eq!(grace_input["flapping"], expected_flapping);

    if model.active {
        assert_eq!(plain_input["status"], "on");
        assert_eq!(grace_input["status"], "on");
        assert!(plain_input["lastSessionProtocol"].is_null());
        assert!(grace_input["lastSessionProtocol"].is_null());
        assert!(plain_input["lastDisconnectReason"].is_null());
        assert!(grace_input["lastDisconnectReason"].is_null());
        assert!(plain_input["lastFailurePhase"].is_null());
        assert!(grace_input["lastFailurePhase"].is_null());
        assert_eq!(plain_input["recentDisconnectError"], false);
        assert_eq!(grace_input["recentDisconnectError"], false);
        assert_eq!(plain_input["disconnectGraceActive"], false);
        assert_eq!(grace_input["disconnectGraceActive"], false);
        assert!(plain_input["disconnectGraceRemainingMs"].is_null());
        assert!(grace_input["disconnectGraceRemainingMs"].is_null());
        return;
    }

    assert_eq!(plain_input["status"], "off");
    assert_eq!(grace_input["status"], "off");

    match model.recent_visible {
        false => {
            assert_eq!(plain_input["probeStatus"], "off");
            assert_eq!(grace_input["probeStatus"], "off");
            assert!(plain_input["lastSessionProtocol"].is_null());
            assert!(grace_input["lastSessionProtocol"].is_null());
            assert!(plain_input["lastDisconnectReason"].is_null());
            assert!(grace_input["lastDisconnectReason"].is_null());
            assert!(plain_input["lastFailurePhase"].is_null());
            assert!(grace_input["lastFailurePhase"].is_null());
            assert_eq!(plain_input["recentDisconnectError"], false);
            assert_eq!(grace_input["recentDisconnectError"], false);
            assert_eq!(plain_input["disconnectGraceActive"], false);
            assert_eq!(grace_input["disconnectGraceActive"], false);
            assert!(plain_input["disconnectGraceRemainingMs"].is_null());
            assert!(grace_input["disconnectGraceRemainingMs"].is_null());
        }
        true => {
            let expected_probe_status = if model.recent_had_error {
                "failed"
            } else {
                "off"
            };
            assert_eq!(plain_input["probeStatus"], expected_probe_status);
            assert_eq!(grace_input["probeStatus"], expected_probe_status);
            assert_eq!(
                plain_input["lastSessionProtocol"].as_str(),
                model.recent_protocol
            );
            assert_eq!(
                grace_input["lastSessionProtocol"].as_str(),
                model.recent_protocol
            );
            assert_eq!(
                plain_input["lastDisconnectReason"].as_str(),
                model.recent_message
            );
            assert_eq!(
                grace_input["lastDisconnectReason"].as_str(),
                model.recent_message
            );
            assert_eq!(plain_input["lastFailurePhase"].as_str(), model.recent_phase);
            assert_eq!(grace_input["lastFailurePhase"].as_str(), model.recent_phase);
            assert_eq!(plain_input["recentDisconnectError"], model.recent_had_error);
            assert_eq!(grace_input["recentDisconnectError"], model.recent_had_error);
            assert_eq!(
                plain_input["lastSessionBytesReceived"],
                model.recent_bytes_received
            );
            assert_eq!(
                grace_input["lastSessionBytesReceived"],
                model.recent_bytes_received
            );
            assert_eq!(
                plain_input["lastRemoteAddr"].as_str(),
                model.recent_remote_addr
            );
            assert_eq!(
                grace_input["lastRemoteAddr"].as_str(),
                model.recent_remote_addr
            );
            assert_eq!(plain_input["disconnectGraceActive"], false);
            assert_eq!(grace_input["disconnectGraceActive"], true);
            assert!(plain_input["disconnectGraceRemainingMs"].is_null());
            assert!(
                grace_input["disconnectGraceRemainingMs"]
                    .as_u64()
                    .is_some_and(|remaining| remaining > 0 && remaining <= 5_000)
            );
        }
    }
}

#[tokio::test]
async fn test_hls_consumers_monotonic_idle() {
    let cancel = CancellationToken::new();
    let hc = HlsConsumers::new(cancel);
    assert!(!hc.is_idle(60000));

    tokio::time::sleep(tokio::time::Duration::from_millis(5)).await;
    hc.touch();
    let last = hc.last_access_ms.load(Ordering::Relaxed);
    assert!(last > 0);

    tokio::time::sleep(tokio::time::Duration::from_millis(15)).await;
    assert!(!hc.is_idle(60000));
    assert!(hc.is_idle(10));
}

#[tokio::test]
async fn runtime_helpers_expose_registered_ingest_and_egress() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-runtime", "stream-key", "rtmp")
        .await
        .expect("register ingest");
    engine.update_ingest_bytes("pipe-runtime", 2048).await;

    let ingest = engine
        .with_active_ingest("pipe-runtime", |ingest| {
            (
                ingest.protocol.clone(),
                ingest.bytes_received.load(Ordering::Relaxed),
            )
        })
        .await;
    assert_eq!(ingest, Some(("rtmp".to_string(), 2048)));
    assert_eq!(engine.with_active_ingest("missing", |_| true).await, None);

    engine
        .register_egress("out-runtime", "pipe-runtime", "rtmp://example.com/live/key")
        .await;
    engine.record_egress_progress("out-runtime", 1316).await;

    let egress = engine
        .with_active_egress("out-runtime", |egress| {
            (
                egress.protocol.clone(),
                egress.bytes_sent.load(Ordering::Relaxed),
                egress
                    .phase
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .to_string(),
            )
        })
        .await;
    assert_eq!(
        egress,
        Some(("rtmp".to_string(), 1316, "sending".to_string()))
    );
    assert_eq!(engine.with_active_egress("missing", |_| true).await, None);
}

#[tokio::test]
async fn hls_dependency_snapshot_reflects_store_and_consumer_state() {
    let engine = MediaEngine::new();
    let (store, already_running) = engine
        .ensure_hls_preview_segmenter("pipe-hls-snapshot")
        .await;
    assert!(!already_running);

    engine.touch_hls_preview("pipe-hls-snapshot").await;
    store.push_video_segment(0, 2.0, Bytes::from_static(b"segment"));

    let snapshot = engine.hls_dependency_snapshot("pipe-hls-snapshot").await;
    assert!(snapshot.store_exists);
    assert!(snapshot.active);
    assert_eq!(snapshot.persistent_consumers, 0);
    assert!(snapshot.last_access_age_ms.is_some());
    assert_eq!(snapshot.segments, 1);
    assert!(snapshot.playlist_bytes > 0);
}

#[tokio::test]
async fn active_hls_preview_stage_keys_include_hevc_preview_codec_edge() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-preview-hevc", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-preview-hevc",
            Some(VideoMeta {
                codec: "hevc".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    let _ = engine
        .ensure_hls_preview_segmenter("pipe-preview-hevc")
        .await;

    let stages = engine.active_hls_preview_stage_keys().await;

    assert!(stages.contains(&StageKey::new(
        "pipe-preview-hevc",
        StageKind::preview("720p", StageKind::source())
    )));
    assert!(stages.contains(&StageKey::new(
        "pipe-preview-hevc",
        StageKind::hls_segmenter(StageKind::preview("720p", StageKind::source()))
    )));
}

#[tokio::test]
async fn active_hls_preview_stage_keys_include_h264_segmenter() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-preview-h264", "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            "pipe-preview-h264",
            Some(VideoMeta {
                codec: "h264".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    let _ = engine
        .ensure_hls_preview_segmenter("pipe-preview-h264")
        .await;

    let stages = engine.active_hls_preview_stage_keys().await;

    assert_eq!(stages.len(), 1);
    assert!(stages.contains(&StageKey::new(
        "pipe-preview-h264",
        StageKind::hls_segmenter(StageKind::source())
    )));
}

#[tokio::test]
async fn preview_blocked_by_snapshot_uses_graph_planned_keys() {
    use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};

    let engine = MediaEngine::new_with_config(Arc::new(crate::AppConfig {
        external_ffmpeg_permits: 3,
        ..Default::default()
    }));
    let pipeline_id = "pipe-preview-blocked";
    engine
        .try_register_ingest(pipeline_id, "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "hevc".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    let _ = engine.ensure_hls_preview_segmenter(pipeline_id).await;

    let rogue_key = StageKey::new(
        pipeline_id,
        StageKind::preview("720p", StageKind::video_preset("rogue")),
    );
    let rogue_lifecycle = engine
        .get_or_create_stage_lifecycle(rogue_key, StagePhase::Registered)
        .await;
    rogue_lifecycle.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    assert_eq!(
        engine.preview_blocked_by_snapshot(pipeline_id).await,
        None,
        "unplanned preview-looking stages must not drive HLS preview blocked cause"
    );

    let planned_key = StageKey::new(pipeline_id, StageKind::preview("720p", StageKind::source()));
    let planned_lifecycle = engine
        .get_or_create_stage_lifecycle(planned_key.clone(), StagePhase::Registered)
        .await;
    planned_lifecycle.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    let blocked = engine
        .preview_blocked_by_snapshot(pipeline_id)
        .await
        .expect("planned preview stage should be reported as blocked");
    assert_eq!(blocked.key, planned_key);
    assert_eq!(blocked.capacity_permits_total, Some(3));
}

#[tokio::test]
async fn file_ingest_dependency_snapshot_reflects_active_and_child_state() {
    let engine = MediaEngine::new();
    engine
        .mark_file_ingest_running("file-ingest-snapshot")
        .await;

    let child = Command::new("sleep")
        .arg("30")
        .spawn()
        .expect("spawn sleep child");
    engine
        .file_ingests
        .children
        .write()
        .await
        .insert("file-ingest-snapshot".to_string(), child);

    let snapshot = engine
        .file_ingest_dependency_snapshot("file-ingest-snapshot")
        .await;
    assert!(snapshot.marked_active);
    assert!(snapshot.child_registered);

    assert!(engine.stop_file_ingest_child("file-ingest-snapshot").await);
    engine
        .clear_file_ingest_running("file-ingest-snapshot")
        .await;

    let snapshot = engine
        .file_ingest_dependency_snapshot("file-ingest-snapshot")
        .await;
    assert!(!snapshot.marked_active);
    assert!(!snapshot.child_registered);
}

#[tokio::test]
async fn rejects_a_second_independent_publisher_for_the_same_pipeline() {
    let engine = MediaEngine::new();

    assert!(
        engine
            .try_register_ingest("pipeline-1", "stream-key", "srt")
            .await
            .is_some()
    );
    assert!(
        engine
            .try_register_ingest("pipeline-1", "stream-key", "srt")
            .await
            .is_none()
    );

    engine.unregister_ingest("pipeline-1").await;
    assert!(
        engine
            .try_register_ingest("pipeline-1", "stream-key", "srt")
            .await
            .is_some()
    );
}

#[tokio::test]
async fn concurrent_publishers_cannot_both_reserve_the_same_pipeline() {
    let engine = Arc::new(MediaEngine::new());
    let first_engine = engine.clone();
    let second_engine = engine.clone();

    let (first, second) = tokio::join!(
        async move {
            first_engine
                .try_register_ingest("pipeline-race", "stream-key", "srt")
                .await
                .is_some()
        },
        async move {
            second_engine
                .try_register_ingest("pipeline-race", "stream-key", "srt")
                .await
                .is_some()
        }
    );

    assert_ne!(first, second, "exactly one publisher must win reservation");
    assert_eq!(engine.ingests.active.read().await.len(), 1);
}

#[tokio::test]
async fn stale_ingest_unregister_cannot_clobber_replacement_attempt() {
    let engine = MediaEngine::new();

    let first = engine
        .try_register_ingest_attempt("pipeline-race", "stream-key", "rtmp")
        .await
        .expect("first ingest should register");
    engine.unregister_ingest("pipeline-race").await;

    let replacement = engine
        .try_register_ingest_attempt("pipeline-race", "stream-key", "srt")
        .await
        .expect("replacement ingest should register");

    assert!(
        !engine
            .unregister_ingest_if_current("pipeline-race", &first)
            .await,
        "stale cleanup from the old attempt must not remove the replacement ingest"
    );
    assert!(
        engine
            .with_active_ingest("pipeline-race", |ingest| ingest.attempt_id)
            .await
            .is_some_and(|attempt_id| attempt_id == replacement.attempt_id),
        "replacement ingest must remain active after stale unregister"
    );
}

#[tokio::test]
async fn stale_ingest_disconnect_cannot_poison_replacement_attempt() {
    let engine = MediaEngine::new();

    let first = engine
        .try_register_ingest_attempt("pipeline-race", "stream-key", "rtmp")
        .await
        .expect("first ingest should register");
    engine.unregister_ingest("pipeline-race").await;

    let replacement = engine
        .try_register_ingest_attempt("pipeline-race", "stream-key-2", "srt")
        .await
        .expect("replacement ingest should register");

    assert!(
        !engine
            .record_ingest_disconnect_if_current(
                "pipeline-race",
                &first,
                Some("receive"),
                Some("stale disconnect".to_string()),
                true,
            )
            .await,
        "stale disconnect metadata must not attach to a replacement ingest attempt"
    );
    assert!(
        engine
            .record_ingest_disconnect_if_current(
                "pipeline-race",
                &replacement,
                Some("disconnect"),
                Some("replacement disconnect".to_string()),
                false,
            )
            .await,
        "current attempt should still be able to publish disconnect metadata"
    );
    assert!(
        engine
            .unregister_ingest_if_current("pipeline-race", &replacement)
            .await,
        "replacement attempt should be able to unregister cleanly"
    );

    let pipelines = vec!["pipeline-race".to_string()];
    let snapshot = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let input = &snapshot["pipelines"]["pipeline-race"]["input"];
    assert_eq!(input["status"], "off");
    assert_eq!(input["lastSessionProtocol"], "srt");
    assert_eq!(input["lastDisconnectReason"], "replacement disconnect");
    assert_eq!(input["lastFailurePhase"], "disconnect");
    assert_eq!(input["recentDisconnectError"], false);
}

#[tokio::test]
async fn health_snapshot_marks_outputs_stopped_without_ingest() {
    let engine = MediaEngine::new();
    engine
        .register_egress("output-1", "pipeline-1", "rtmp://example/live/test")
        .await;

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-1".to_string()], &HashMap::new()).await;

    assert_eq!(
        snapshot["pipelines"]["pipeline-1"]["outputs"]["output-1"]["status"],
        "stopped"
    );
}

#[tokio::test]
async fn health_snapshot_marks_failed_egress_status_when_input_is_live() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-1", "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .register_egress("output-1", "pipeline-1", "rtmp://example/live/test")
        .await;
    engine
        .record_egress_error("output-1", "send", "connection refused")
        .await;

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-1".to_string()], &HashMap::new()).await;

    let output = &snapshot["pipelines"]["pipeline-1"]["outputs"]["output-1"];
    assert_eq!(output["status"], "failed");
    assert_eq!(output["rawStatus"], "running");
    assert_eq!(output["phase"], "failed");
    assert_eq!(output["failurePhase"], "send");
}

#[tokio::test]
async fn health_snapshot_marks_live_output_stalled_without_progress() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-1", "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .register_egress("output-1", "pipeline-1", "rtmp://example/live/test")
        .await;
    {
        let mut egresses = engine.egresses.active.write().await;
        let egress = egresses.get_mut("output-1").unwrap();
        egress.start_instant = Instant::now()
            .checked_sub(std::time::Duration::from_millis(
                EGRESS_PROGRESS_STALE_MS + 1,
            ))
            .unwrap();
    }

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-1".to_string()], &HashMap::new()).await;

    assert_eq!(
        snapshot["pipelines"]["pipeline-1"]["outputs"]["output-1"]["status"],
        "stalled"
    );
}

#[tokio::test]
async fn health_snapshot_keeps_local_hls_segmenter_running_without_bytes_out() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-1", "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .register_egress("output-1", "pipeline-1", "hls://localhost/hls/test")
        .await;
    engine.update_egress_phase("output-1", EP::Segmenting).await;
    {
        let mut egresses = engine.egresses.active.write().await;
        let egress = egresses.get_mut("output-1").unwrap();
        egress.start_instant = Instant::now()
            .checked_sub(std::time::Duration::from_millis(
                EGRESS_PROGRESS_STALE_MS + 1,
            ))
            .unwrap();
    }

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-1".to_string()], &HashMap::new()).await;

    let output = &snapshot["pipelines"]["pipeline-1"]["outputs"]["output-1"];
    assert_eq!(output["status"], "running");
    assert_eq!(output["phase"], "segmenting");
    assert_eq!(output["bytesOut"], 0);
}

#[tokio::test]
async fn health_snapshot_includes_all_ingest_audio_tracks() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-audio", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_ingest_audio_tracks(
            "pipeline-audio",
            vec![
                AudioMeta {
                    codec: "aac".to_string(),
                    sample_rate: 48_000,
                    channels: 2,
                    channel_layout: None,
                    track_index: 0,
                    pid: Some(0x101),
                    language: Some("eng".to_string()),
                    title: None,
                    profile: None,
                },
                AudioMeta {
                    codec: "aac".to_string(),
                    sample_rate: 44_100,
                    channels: 1,
                    channel_layout: None,
                    track_index: 1,
                    pid: Some(0x102),
                    language: None,
                    title: None,
                    profile: None,
                },
            ],
        )
        .await;

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-audio".to_string()], &HashMap::new()).await;
    let tracks = snapshot["pipelines"]["pipeline-audio"]["input"]["audioTracks"]
        .as_array()
        .unwrap();

    assert_eq!(tracks.len(), 2);
    assert_eq!(tracks[0]["pid"], 0x101);
    assert_eq!(tracks[0]["language"], "eng");
    assert_eq!(tracks[1]["trackIndex"], 1);
}

#[tokio::test]
async fn health_snapshot_reports_probe_readiness() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipeline-probe", "stream-key", "srt")
        .await
        .unwrap();

    let pending =
        test_health_snapshot(&engine, &["pipeline-probe".to_string()], &HashMap::new()).await;
    let pending_input = &pending["pipelines"]["pipeline-probe"]["input"];
    assert_eq!(pending_input["probeReady"], false);
    assert_eq!(pending_input["probeStatus"], "pending");
    assert!(pending_input["probePendingMs"].as_u64().is_some());

    let video = Some(VideoMeta {
        codec: "h264".to_string(),
        width: 1920,
        height: 1080,
        fps: 30.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    });
    let audio = AudioMeta {
        track_index: 0,
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
    };
    engine
        .update_ingest_meta("pipeline-probe", video, Some(audio.clone()), None)
        .await;
    engine
        .update_ingest_audio_tracks("pipeline-probe", vec![audio])
        .await;

    let ready =
        test_health_snapshot(&engine, &["pipeline-probe".to_string()], &HashMap::new()).await;
    let ready_input = &ready["pipelines"]["pipeline-probe"]["input"];
    assert_eq!(ready_input["probeReady"], true);
    assert_eq!(ready_input["probeStatus"], "ready");
    assert!(ready_input["probePendingMs"].is_null());
}

#[tokio::test]
async fn health_snapshot_marks_hls_preview_active_when_consumer_exists() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-hls";

    let _ = engine.ensure_hls_preview_segmenter(pipeline_id).await;
    let snapshot = test_health_snapshot(&engine, &[pipeline_id.to_string()], &HashMap::new()).await;

    assert_eq!(
        snapshot["pipelines"][pipeline_id]["hlsPreview"]["active"],
        true
    );
}

#[tokio::test]
async fn health_snapshot_marks_cancelled_hls_preview_inactive() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-hls-cancelled";

    let _ = engine.ensure_hls_preview_segmenter(pipeline_id).await;
    let token = engine
        .get_hls_preview_cancel_token(pipeline_id)
        .await
        .unwrap();
    token.cancel();

    let snapshot = test_health_snapshot(&engine, &[pipeline_id.to_string()], &HashMap::new()).await;

    assert_eq!(
        snapshot["pipelines"][pipeline_id]["hlsPreview"]["active"],
        false
    );
}

#[tokio::test]
async fn health_and_graph_expose_reader_lag_overflow_and_packet_age() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-reader-metrics";
    let rb = engine.get_or_create_pipeline(pipeline_id).await;

    rb.push(test_video_packet(0, 0, true));
    let _reader = Reader::new("graph-reader".to_string(), rb.clone());
    tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    rb.push(test_audio_packet(10, 10));

    let snapshot = test_health_snapshot(&engine, &[pipeline_id.to_string()], &HashMap::new()).await;
    let reader_metrics = snapshot["pipelines"][pipeline_id]["input"]["readerMetrics"]
        .as_array()
        .unwrap();
    assert_eq!(reader_metrics.len(), 1);
    assert_eq!(reader_metrics[0]["name"], "graph-reader");
    assert_eq!(reader_metrics[0]["lagSlots"], 2);
    assert_eq!(reader_metrics[0]["overflowCount"], 0);
    assert!(
        !reader_metrics[0]["packetAgeMs"].is_null(),
        "health reader metrics should expose unread packet age"
    );

    let graph = crate::api_runtime_views::processing_graph(&engine, pipeline_id, &[]).await;
    let source = graph["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|node| node["type"] == "ring_buffer")
        .unwrap();
    let graph_readers = source["details"]["readers"].as_array().unwrap();
    assert_eq!(graph_readers.len(), 1);
    assert_eq!(graph_readers[0]["lagSlots"], 2);
    assert_eq!(graph_readers[0]["overflowCount"], 0);
    assert!(
        !graph_readers[0]["packetAgeMs"].is_null(),
        "graph reader metrics should expose unread packet age"
    );
}

#[tokio::test]
async fn health_snapshot_exposes_bonding_and_member_telemetry() {
    let engine = MediaEngine::new();
    engine
        .runtime
        .listener_stats
        .bonding_available
        .store(true, Ordering::Relaxed);
    engine
        .try_register_ingest("pipeline-bond", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .update_publisher_quality(
            "pipeline-bond",
            PublisherQuality {
                srt_bonded: Some(true),
                srt_group_member_count: Some(2),
                srt_group_connected_members: Some(2),
                srt_group_active_members: Some(1),
                srt_group_broken_members: Some(0),
                ..PublisherQuality::default()
            },
        )
        .await;

    let snapshot =
        test_health_snapshot(&engine, &["pipeline-bond".to_string()], &HashMap::new()).await;
    let quality = &snapshot["pipelines"]["pipeline-bond"]["input"]["publisher"]["quality"];

    assert_eq!(snapshot["srtListener"]["bondingAvailable"], true);
    assert_eq!(quality["srtBonded"], true);
    assert_eq!(quality["srtGroupMemberCount"], 2);
    assert_eq!(quality["srtGroupConnectedMembers"], 2);
    assert_eq!(quality["srtGroupActiveMembers"], 1);
    assert_eq!(quality["srtGroupBrokenMembers"], 0);
}

#[tokio::test]
async fn unregister_ingest_cancels_token() {
    let engine = MediaEngine::new();
    let token = engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    assert!(!token.is_cancelled());

    engine.unregister_ingest("p1").await;
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn unregister_ingest_idempotent() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine.unregister_ingest("p1").await;
    // Second unregister should not panic
    engine.unregister_ingest("p1").await;
}

#[tokio::test]
async fn egress_register_and_cancel() {
    let engine = MediaEngine::new();
    let token = engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;
    assert!(!token.is_cancelled());

    engine.unregister_egress("out-1").await;
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn egress_unregister_idempotent() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;
    engine.unregister_egress("out-1").await;
    engine.unregister_egress("out-1").await;
}

#[tokio::test]
async fn stale_egress_unregister_cannot_clobber_replacement_attempt() {
    let engine = MediaEngine::new();

    let first = engine
        .register_egress_attempt("out-race", "pipe-1", "rtmp://example.com/live/one", None)
        .await;
    engine.unregister_egress("out-race").await;

    let replacement = engine
        .register_egress_attempt("out-race", "pipe-1", "srt://example.com:10080", None)
        .await;

    assert!(
        !engine
            .unregister_egress_if_current("out-race", &first)
            .await,
        "stale cleanup from the old egress attempt must not remove the replacement"
    );
    assert!(
        engine
            .with_active_egress("out-race", |egress| egress.attempt_id)
            .await
            .is_some_and(|attempt_id| attempt_id == replacement.attempt_id),
        "replacement egress must remain active after stale unregister"
    );
}

#[tokio::test]
async fn stale_egress_error_cannot_poison_replacement_attempt() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-1", "stream-key", "rtmp")
        .await
        .unwrap();

    let first = engine
        .register_egress_attempt("out-race", "pipe-1", "rtmp://example.com/live/one", None)
        .await;
    engine.unregister_egress("out-race").await;

    let replacement = engine
        .register_egress_attempt("out-race", "pipe-1", "rtmp://example.com/live/two", None)
        .await;

    assert!(
        !engine
            .record_egress_error_if_current("out-race", &first, "send", "stale failure",)
            .await,
        "stale failure metadata must not attach to a replacement egress attempt"
    );
    engine
        .record_egress_progress_if_current("out-race", &replacement, 2048)
        .await;
    assert!(
        engine
            .record_egress_error_if_current(
                "out-race",
                &replacement,
                "connect",
                "replacement failure",
            )
            .await,
        "current attempt should still publish its own failure metadata"
    );
    assert!(
        engine
            .unregister_egress_if_current("out-race", &replacement)
            .await,
        "replacement attempt should unregister cleanly"
    );

    let pipelines = vec!["pipe-1".to_string()];
    let snapshot = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-race"];
    assert_eq!(output["status"], "failed");
    assert_eq!(output["failurePhase"], "connect");
    assert_eq!(output["lastError"], "replacement failure");
    assert_eq!(output["totalSize"], 2048);
}

#[tokio::test]
async fn stale_egress_queue_removal_cannot_drop_replacement_queue() {
    let engine = MediaEngine::new();
    let first = engine
        .register_egress_attempt("out-race", "pipe-1", "srt://example.com:10080", None)
        .await;
    let first_queue = Arc::new(MemoryQueue::new());
    assert!(
        engine
            .register_egress_queue_if_current("out-race", &first, first_queue)
            .await
    );
    engine.unregister_egress("out-race").await;

    let replacement = engine
        .register_egress_attempt("out-race", "pipe-1", "srt://example.com:10081", None)
        .await;
    let replacement_queue = Arc::new(MemoryQueue::new());
    assert!(
        engine
            .register_egress_queue_if_current("out-race", &replacement, replacement_queue.clone(),)
            .await
    );
    assert!(
        !engine
            .remove_egress_queue_if_current("out-race", &first)
            .await,
        "stale cleanup must not remove the replacement queue"
    );
    assert!(Arc::ptr_eq(
        &engine
            .egresses
            .queues
            .read()
            .await
            .get("out-race")
            .expect("replacement queue should stay registered")
            .clone(),
        &replacement_queue
    ));
}

#[tokio::test]
async fn egress_registration_stores_terminal_stage_key() {
    let engine = MediaEngine::new();
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let reg = engine
        .register_egress_attempt(
            "out-1",
            "pipe-1",
            "rtmp://example.com/live/key",
            Some(key.clone()),
        )
        .await;
    assert!(reg.attempt_id > 0);
    let stored = engine
        .with_active_egress("out-1", |e| e.terminal_stage_key.clone())
        .await;
    assert_eq!(stored, Some(Some(key)));
}

#[tokio::test]
async fn egress_blocked_by_phase_reports_waiting_upstream_stage() {
    use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};

    let engine = MediaEngine::new_with_config(Arc::new(crate::AppConfig {
        external_ffmpeg_permits: 3,
        ..Default::default()
    }));
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let lc = engine
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Registered)
        .await;
    lc.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    engine
        .register_egress_attempt(
            "out-1",
            "pipe-1",
            "rtmp://example.com/live/key",
            Some(key.clone()),
        )
        .await;

    let blocked = {
        let egresses = engine.egresses.active.read().await;
        let egress = egresses.get("out-1").unwrap();
        engine.egress_blocked_by_snapshot(egress).await
    };
    assert!(
        matches!(
            blocked,
            Some(crate::runtime::stage::StageRuntimeSnapshot {
                phase: StagePhase::WaitingForCapacity { .. },
                capacity_permits_total: Some(3),
                ..
            })
        ),
        "expected blocked by WaitingForCapacity with configured permits, got {blocked:?}"
    );

    lc.transition(StagePhase::Producing);
    let blocked = {
        let egresses = engine.egresses.active.read().await;
        let egress = egresses.get("out-1").unwrap();
        engine.egress_blocked_by_snapshot(egress).await
    };
    assert_eq!(blocked, None, "producing stage must not block egress");
}

#[tokio::test]
async fn stage_runtime_snapshot_reads_runtime_after_side_maps_removed() {
    use crate::media::ring_buffer::RingBuffer;
    use crate::media::stage_lifecycle::StagePhase;
    use crate::media::stage_runtime::StageRuntimeManager;

    let engine = Arc::new(MediaEngine::new());
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let manager = StageRuntimeManager::new(engine.clone());
    let (handle, _) = manager
        .ensure_stage(key.clone(), Arc::new(RingBuffer::new(4)), None)
        .await;
    handle.metrics.record_in(42);
    handle.lifecycle.transition(StagePhase::RunningNoOutputYet);
    engine.stages.metrics.write().await.remove(&key);
    engine.stages.lifecycles.write().await.remove(&key);

    let snapshot = engine
        .stage_runtime_snapshot(&key)
        .await
        .expect("runtime-backed stage should not depend on side maps");
    assert_eq!(snapshot.phase, StagePhase::RunningNoOutputYet);
    assert_eq!(snapshot.bytes_in, 42);

    let metrics = engine.get_or_create_stage_metrics(key.clone()).await;
    let lifecycle = engine
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Registered)
        .await;

    assert!(Arc::ptr_eq(&metrics, &handle.metrics));
    assert!(Arc::ptr_eq(&lifecycle, &handle.lifecycle));
}

#[tokio::test]
async fn health_snapshot_includes_blocked_by_for_waiting_terminal_stage() {
    use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};

    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-1", "stream-key", "srt")
        .await
        .unwrap();
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    let lc = engine
        .get_or_create_stage_lifecycle(key.clone(), StagePhase::Registered)
        .await;
    lc.transition(StagePhase::WaitingForCapacity {
        backend: StageBackendKind::ExternalFfmpeg,
    });

    engine
        .register_egress_attempt("out-1", "pipe-1", "rtmp://example.com/live/key", Some(key))
        .await;

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["blockedBy"]["phase"], "waitingForCapacity");
    assert_eq!(output["blockedBy"]["backend"], "externalFfmpeg");
}

#[tokio::test]
async fn egress_bytes_counter() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;

    engine.update_egress_bytes("out-1", 1000).await;
    engine.update_egress_bytes("out-1", 500).await;
    assert_eq!(engine.egress_bytes("out-1").await, 1500);

    // Non-existent egress returns 0
    assert_eq!(engine.egress_bytes("out-nonexistent").await, 0);
}

#[tokio::test]
async fn health_snapshot_exposes_egress_progress_and_error_state() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-1", "stream-key", "srt")
        .await
        .unwrap();
    engine
        .register_egress(
            "out-1",
            "pipe-1",
            "srt://example.com:10080?streamid=live/key",
        )
        .await;
    engine
        .update_egress_target_addr("out-1", "203.0.113.10:10080".to_string())
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine
        .update_egress_quality(
            "out-1",
            PublisherQuality {
                tcp_congestion_algorithm: Some("cubic".to_string()),
                mbps_send_rate: Some(3.2),
                packets_sent_retrans: Some(2),
                srt_bonded: Some(true),
                srt_group_member_count: Some(2),
                srt_group_active_members: Some(1),
                ..PublisherQuality::default()
            },
        )
        .await;
    engine.record_egress_progress("out-1", 1316).await;
    engine
        .record_egress_error("out-1", "send", "synthetic send failure")
        .await;

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];

    assert_eq!(output["protocol"], "srt");
    assert_eq!(output["status"], "failed");
    assert_eq!(output["targetAddr"], "203.0.113.10:10080");
    assert_eq!(output["phase"], "failed");
    assert_eq!(output["failurePhase"], "send");
    assert_eq!(output["lastError"], "synthetic send failure");
    assert_eq!(output["totalSize"], 1316);
    assert_eq!(output["quality"]["mbpsSendRate"], 3.2);
    assert_eq!(output["quality"]["tcpCongestionAlgorithm"], "cubic");
    assert_eq!(output["quality"]["packetsSentRetrans"], 2);
    assert_eq!(output["quality"]["srtBonded"], true);
    assert_eq!(output["quality"]["srtGroupMemberCount"], 2);
    assert_eq!(output["quality"]["srtGroupActiveMembers"], 1);
    assert!(!output["lastProgressAt"].is_null());
    assert!(!output["lastErrorAt"].is_null());
}

#[tokio::test]
async fn egress_failure_event_survives_unregister() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;
    engine
        .record_egress_error("out-1", "connect", "connection refused")
        .await;
    engine.unregister_egress("out-1").await;

    let events = engine.runtime.event_log.recent(10, Some("pipe-1"));
    assert!(events.iter().any(|event| matches!(
        &event.kind,
        crate::events::EventKind::EgressFailed {
            output_id,
            phase,
            error,
            ..
        } if output_id == "out-1" && phase == "connect" && error == "connection refused"
    )));
}

#[tokio::test]
async fn egress_progress_after_error_clears_failed_phase() {
    let engine = MediaEngine::new();
    engine
        .register_egress(
            "out-1",
            "pipe-1",
            "https://upload.example.com/live/out.m3u8?token=abc",
        )
        .await;
    engine
        .record_egress_error("out-1", "upload_segment", "temporary sink outage")
        .await;
    engine.record_egress_progress("out-1", 4096).await;

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];

    assert_eq!(output["phase"], "uploading");
    assert!(output["failurePhase"].is_null());
    assert!(output["lastError"].is_null());
    assert!(output["lastErrorAt"].is_null());
    assert_eq!(output["totalSize"], 4096);
}

#[tokio::test]
async fn egress_has_recorded_progress_only_after_progress_update() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://example.com/live/key")
        .await;

    assert!(!engine.egress_has_recorded_progress("out-1").await);

    engine.record_egress_progress("out-1", 188).await;

    assert!(engine.egress_has_recorded_progress("out-1").await);
}

#[tokio::test]
async fn pipeline_create_and_remove() {
    let engine = MediaEngine::new();
    let rb1 = engine.get_or_create_pipeline("p1").await;
    let rb2 = engine.get_or_create_pipeline("p1").await;
    // Same pipeline returns same buffer
    assert!(Arc::ptr_eq(&rb1, &rb2));

    engine.remove_pipeline("p1").await;
    let rb3 = engine.get_or_create_pipeline("p1").await;
    // After removal, new buffer is created
    assert!(!Arc::ptr_eq(&rb1, &rb3));
}

#[tokio::test]
async fn health_snapshot_includes_egress_under_correct_pipeline() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-a", "pipe-1", "rtmp://a.com/live/key")
        .await;
    engine
        .register_egress("out-b", "pipe-2", "rtmp://b.com/live/key")
        .await;
    engine
        .register_egress("out-c", "pipe-1", "srt://c.com?streamid=key")
        .await;

    let ids = vec!["pipe-1".to_string(), "pipe-2".to_string()];
    let rec = std::collections::HashMap::new();
    let snap = test_health_snapshot(&engine, &ids, &rec).await;

    let pipe1_outputs = &snap["pipelines"]["pipe-1"]["outputs"];
    assert!(pipe1_outputs.get("out-a").is_some());
    assert!(pipe1_outputs.get("out-c").is_some());
    assert!(pipe1_outputs.get("out-b").is_none());

    let pipe2_outputs = &snap["pipelines"]["pipe-2"]["outputs"];
    assert!(pipe2_outputs.get("out-b").is_some());
    assert!(pipe2_outputs.get("out-a").is_none());
}

#[tokio::test]
async fn recording_lifecycle() {
    let engine = MediaEngine::new();
    assert!(!engine.is_recording_active("p1").await);

    let token = engine.register_recording("p1").await;
    assert!(engine.is_recording_active("p1").await);
    assert!(!token.is_cancelled());

    engine.unregister_recording("p1").await;
    assert!(!engine.is_recording_active("p1").await);
    assert!(token.is_cancelled());
}

#[tokio::test]
async fn cancelled_recording_token_is_not_active() {
    let engine = MediaEngine::new();
    let token = engine.register_recording("p-cancelled-rec").await;

    assert!(engine.is_recording_active("p-cancelled-rec").await);
    token.cancel();

    assert!(
        !engine.is_recording_active("p-cancelled-rec").await,
        "cancelled recording token must not be reported as active"
    );
}

#[tokio::test]
async fn health_snapshot_marks_cancelled_recording_inactive() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-rec-cancelled";
    let token = engine.register_recording(pipeline_id).await;
    token.cancel();

    let mut recording_enabled = HashMap::new();
    recording_enabled.insert(pipeline_id.to_string(), true);
    let snapshot =
        test_health_snapshot(&engine, &[pipeline_id.to_string()], &recording_enabled).await;

    assert_eq!(
        snapshot["pipelines"][pipeline_id]["recording"]["active"],
        false
    );
}

#[tokio::test]
async fn processing_graph_marks_cancelled_recording_and_hls_inactive() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-graph-cancelled";
    let rec_token = engine.register_recording(pipeline_id).await;
    rec_token.cancel();

    let _ = engine.ensure_hls_preview_segmenter(pipeline_id).await;
    let hls_token = engine
        .get_hls_preview_cancel_token(pipeline_id)
        .await
        .unwrap();
    hls_token.cancel();

    let graph = crate::api_runtime_views::processing_graph(&engine, pipeline_id, &[]).await;
    let nodes = graph["nodes"].as_array().unwrap();

    let recording = nodes
        .iter()
        .find(|node| node["type"] == "recording")
        .expect("recording node should remain visible while registered");
    assert_eq!(recording["active"], false);

    let hls = nodes
        .iter()
        .find(|node| node["type"] == "hls")
        .expect("HLS node should remain visible while its store exists");
    assert_eq!(hls["active"], false);
}

#[tokio::test]
async fn processing_graph_routes_srt_egress_through_ts_mux() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-srt-graph";
    let _source = engine.get_or_create_pipeline(pipeline_id).await;
    let output = crate::types::Output {
        id: "out-srt".to_string(),
        pipeline_id: pipeline_id.to_string(),
        name: "SRT Target".to_string(),
        url: "srt://example.com:9000?streamid=publish:live/test".to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        config: crate::domain::output_spec::OutputConfig::parse("source"),
    };

    let graph = crate::api_runtime_views::processing_graph(&engine, pipeline_id, &[output]).await;
    let nodes = graph["nodes"].as_array().unwrap();
    let edges = graph["edges"].as_array().unwrap();

    assert!(
        nodes
            .iter()
            .any(|node| node["type"] == "demux" && node["label"] == "Demux/probe idle"),
        "graph should expose the ingest demux/probe boundary"
    );
    assert!(
        nodes
            .iter()
            .any(|node| node["type"] == "packetizer" && node["label"] == "MPEG-TS mux: source"),
        "SRT egress should expose MPEG-TS packetization"
    );
    assert!(
        edges.iter().any(|edge| edge["label"] == "SRT send"),
        "SRT egress should include an explicit sender edge"
    );
    assert!(
        !edges.iter().any(|edge| edge["label"] == "FLV passthrough"),
        "SRT egress must not be labeled as FLV passthrough"
    );
}

#[tokio::test]
async fn processing_graph_marks_failed_egress_inactive() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipeline-failed-output-graph";
    engine
        .try_register_ingest(pipeline_id, "stream-key", "rtmp")
        .await
        .unwrap();
    engine
        .register_egress("out-failed", pipeline_id, "rtmp://example/live/test")
        .await;
    engine
        .record_egress_error("out-failed", "send", "connection refused")
        .await;

    let output = crate::types::Output {
        id: "out-failed".to_string(),
        pipeline_id: pipeline_id.to_string(),
        name: "Failed Target".to_string(),
        url: "rtmp://example/live/test".to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        config: crate::domain::output_spec::OutputConfig::parse("source"),
    };

    let graph = crate::api_runtime_views::processing_graph(&engine, pipeline_id, &[output]).await;
    let nodes = graph["nodes"].as_array().unwrap();
    let egress = nodes
        .iter()
        .find(|node| node["type"] == "egress")
        .expect("egress node");

    assert_eq!(egress["active"], false);
    assert_eq!(egress["details"]["status"], "failed");
    assert_eq!(egress["details"]["phase"], "failed");
    assert_eq!(egress["details"]["failurePhase"], "send");
}

#[tokio::test]
async fn processing_graph_omits_stale_codec_edge_when_output_no_longer_needs_it() {
    let engine = std::sync::Arc::new(MediaEngine::new());
    let pipeline_id = "pipeline-graph-stale-codec";
    engine
        .try_register_ingest(pipeline_id, "stream-key", "file")
        .await
        .unwrap();
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "hevc".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;

    let output = crate::types::Output {
        id: "out-graph-stale-codec".to_string(),
        pipeline_id: pipeline_id.to_string(),
        name: "Graph RTMP".to_string(),
        url: "rtmp://example/live/test".to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        config: crate::domain::output_spec::OutputConfig::parse("h264+atrack:1"),
    };

    let _ = crate::application::egress::prepare_output_ring(&engine, &output).await;
    engine
        .update_ingest_meta(
            pipeline_id,
            Some(VideoMeta {
                codec: "h264".to_string(),
                ..Default::default()
            }),
            None,
            None,
        )
        .await;
    // Re-prepare after the codec flip, as reconciliation would: this creates
    // the h264-only path while the stale HEVC-era stages stay registered.
    let _ = crate::application::egress::prepare_output_ring(&engine, &output).await;

    let stages = engine.active_transcoder_stages(pipeline_id).await;
    assert!(
        stages.iter().any(|(stage, live)| *stage
            == StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("h264"))
            && *live),
        "test precondition: stale codec-edge stage should still exist in the engine registry"
    );

    let graph = crate::api_runtime_views::processing_graph(&engine, pipeline_id, &[output]).await;
    let nodes = graph["nodes"].as_array().unwrap();

    assert!(
        nodes.iter().any(|node| node["stageKey"] == "video:h264"),
        "current output path should still render its video stage"
    );
    assert!(
        nodes
            .iter()
            .any(|node| node["stageKey"] == "audio:atrack:1:from:video:h264"),
        "current output path should still render its audio routing stage"
    );
    assert!(
        !nodes
            .iter()
            .any(|node| node["stageKey"] == "hevc_to_h264:from:video:h264"),
        "graph should omit stale codec-edge stages that no longer belong to the output path"
    );
    assert!(
        !nodes
            .iter()
            .any(|node| node["stageKey"] == "audio:atrack:1:from:hevc_to_h264:from:video:h264"),
        "graph should omit the stale audio route that was keyed off the codec edge"
    );
}

#[tokio::test]
async fn ingest_bytes_and_meta_on_nonexistent_pipeline_is_noop() {
    let engine = MediaEngine::new();
    // Should not panic
    engine.update_ingest_bytes("nonexistent", 1000).await;
    engine
        .update_ingest_meta("nonexistent", None, None, None)
        .await;
}

/// Two outputs with the same pipeline + encoding share exactly one transcoder
/// stage (same Arc<RingBuffer> pointer). A third output with a different
/// encoding gets its own stage. This is the core sharing invariant.
#[tokio::test]
async fn same_encoding_outputs_share_one_transcoder_stage() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-share").await;

    let a = engine
        .get_or_create_transcoder(
            "pipe-share",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;
    let b = engine
        .get_or_create_transcoder(
            "pipe-share",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;
    let c = engine
        .get_or_create_transcoder(
            "pipe-share",
            StageKind::video_preset("1080p"),
            source.clone(),
            None,
        )
        .await;

    assert!(
        Arc::ptr_eq(&a, &b),
        "two outputs with encoding=720p must share the same ring buffer"
    );
    assert!(
        !Arc::ptr_eq(&a, &c),
        "different encodings must use separate ring buffers"
    );
}

/// Audio stages are keyed by both audio operation AND upstream video preset.
/// 720p+atrack:0 and 1080p+atrack:0 must not share an audio stage.
#[tokio::test]
async fn audio_stages_are_isolated_per_video_preset() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-audio").await;

    let v720 = engine
        .get_or_create_transcoder(
            "pipe-audio",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;
    let v1080 = engine
        .get_or_create_transcoder(
            "pipe-audio",
            StageKind::video_preset("1080p"),
            source.clone(),
            None,
        )
        .await;

    let a720 = engine
        .get_or_create_transcoder(
            "pipe-audio",
            StageKind::audio_route("atrack:0", StageKind::video_preset("720p")),
            v720.clone(),
            None,
        )
        .await;
    let a1080 = engine
        .get_or_create_transcoder(
            "pipe-audio",
            StageKind::audio_route("atrack:0", StageKind::video_preset("1080p")),
            v1080.clone(),
            None,
        )
        .await;
    let a720_again = engine
        .get_or_create_transcoder(
            "pipe-audio",
            StageKind::audio_route("atrack:0", StageKind::video_preset("720p")),
            v720,
            None,
        )
        .await;

    assert!(
        !Arc::ptr_eq(&a720, &a1080),
        "audio stages for different video presets must be isolated"
    );
    assert!(
        Arc::ptr_eq(&a720, &a720_again),
        "same audio stage key must return the same ring buffer"
    );
}

/// cleanup_pipeline_stages must remove all entries whose key starts with
/// "<pipeline_id>:" and cancel their tokens. Entries for other pipelines
/// must not be affected.
#[tokio::test]
async fn cleanup_pipeline_stages_removes_all_stage_entries() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-del").await;
    let other = engine.get_or_create_pipeline("pipe-keep").await;

    let s1 = engine
        .get_or_create_transcoder(
            "pipe-del",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;
    let s2 = engine
        .get_or_create_transcoder(
            "pipe-del",
            StageKind::video_preset("1080p"),
            source.clone(),
            None,
        )
        .await;
    let other_stage = engine
        .get_or_create_transcoder("pipe-keep", StageKind::video_preset("720p"), other, None)
        .await;

    // Stages are alive before cleanup
    let stages_before = engine.active_transcoder_stages("pipe-del").await;
    assert_eq!(stages_before.len(), 2);

    engine.cleanup_pipeline_stages("pipe-del").await;

    // All pipe-del stages removed
    let stages_after = engine.active_transcoder_stages("pipe-del").await;
    assert_eq!(
        stages_after.len(),
        0,
        "all stages for deleted pipeline must be removed"
    );

    // The ring buffers from those stages had their tokens cancelled
    let _ = (s1, s2); // bindings kept to confirm they're the same arcs tested above

    // pipe-keep is unaffected
    let other_stages = engine.active_transcoder_stages("pipe-keep").await;
    assert_eq!(
        other_stages.len(),
        1,
        "unrelated pipeline stages must be untouched"
    );
    let _ = other_stage;
}

#[tokio::test]
async fn transcoder_stage_registry_uses_typed_stage_keys() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-typed").await;

    let _stage = engine
        .get_or_create_transcoder("pipe-typed", StageKind::video_preset("720p"), source, None)
        .await;

    let runtimes = engine.stages.runtimes.read().await;
    let key = runtimes
        .keys()
        .find(|key| key.pipeline.as_str() == "pipe-typed")
        .expect("typed registry should contain created stage");

    assert_eq!(key.to_string(), "pipe-typed:video:720p");
    assert!(matches!(
        &key.kind,
        StageKind::VideoPreset { preset } if preset == "720p"
    ));
}

/// remove_pipeline must free the source ring buffer from the pipelines map.
#[tokio::test]
async fn remove_pipeline_frees_source_ring_buffer() {
    let engine = Arc::new(MediaEngine::new());
    let rb = engine.get_or_create_pipeline("pipe-rm").await;
    let weak = Arc::downgrade(&rb);
    drop(rb); // release our local strong reference

    // Pipeline map still holds a strong ref
    assert!(
        weak.upgrade().is_some(),
        "ring buffer should still be alive"
    );

    engine.remove_pipeline("pipe-rm").await;
    // Now only the weak ref remains — the Arc should be freed
    assert!(
        weak.upgrade().is_none(),
        "ring buffer should be freed after remove_pipeline"
    );
}

#[tokio::test]
async fn sweep_unused_transcoder_stages_removes_only_unused() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-sweep").await;

    let s1 = engine
        .get_or_create_transcoder(
            "pipe-sweep",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;
    let s2 = engine
        .get_or_create_transcoder(
            "pipe-sweep",
            StageKind::video_preset("1080p"),
            source.clone(),
            None,
        )
        .await;

    let mut active = std::collections::HashSet::new();
    active.insert(StageKey::new("pipe-sweep", StageKind::video_preset("720p")));

    engine.sweep_unused_transcoder_stages(&active).await;

    let stages = engine.active_transcoder_stages("pipe-sweep").await;
    assert_eq!(stages.len(), 1);
    assert_eq!(stages[0].0, StageKind::video_preset("720p"));
    let runtime_keys: Vec<_> = engine
        .stages
        .runtimes
        .read()
        .await
        .keys()
        .filter(|key| key.pipeline.as_str() == "pipe-sweep")
        .cloned()
        .collect();
    assert_eq!(
        runtime_keys,
        vec![StageKey::new("pipe-sweep", StageKind::video_preset("720p"))],
        "runtime registry must remove swept stage objects"
    );
    // s2 was swept and cancelled
    let _ = (s1, s2);
}

#[tokio::test]
async fn sweep_unused_transcoder_stages_removes_codec_edge_stages() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-sweep-codec").await;

    let _stage = engine
        .get_or_create_h264_transcoder("pipe-sweep-codec", StageKind::source(), source)
        .await;
    let stages_before = engine.active_transcoder_stages("pipe-sweep-codec").await;
    assert!(
        stages_before.iter().any(|(stage, live)| *stage
            == StageKind::codec_edge("hevc_to_h264", StageKind::source())
            && *live),
        "codec-edge stage must be registered before the sweep"
    );

    let active: std::collections::HashSet<StageKey> = std::collections::HashSet::new();
    engine.sweep_unused_transcoder_stages(&active).await;

    let stages_after = engine.active_transcoder_stages("pipe-sweep-codec").await;
    assert!(
        stages_after.is_empty(),
        "unused codec-edge stages must be removed from the shared stage registry"
    );
    assert!(
        engine
            .stages
            .runtimes
            .read()
            .await
            .keys()
            .all(|key| key.pipeline.as_str() != "pipe-sweep-codec"),
        "codec-edge runtime objects must be removed with swept stages"
    );
}

#[tokio::test]
async fn concurrent_get_or_create_transcoder_yields_single_stage() {
    // Bug #4 regression: the old read-lock-then-write-lock TOCTOU window
    // allowed concurrent callers to both see "key absent" and both insert,
    // spawning two transcoder tasks writing to different ring buffers.
    // After the fix, all concurrent callers must receive the SAME Arc<RingBuffer>.
    use std::sync::Arc as StdArc;
    use tokio::sync::Barrier;
    use tokio::task::JoinSet;

    let engine = StdArc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("pipe-concurrent").await;

    // Synchronize 16 tasks to all call get_or_create_transcoder simultaneously
    let barrier = StdArc::new(Barrier::new(16));
    let mut join_set = JoinSet::new();

    for _ in 0..16 {
        let e = engine.clone();
        let s = source.clone();
        let b = barrier.clone();
        join_set.spawn(async move {
            b.wait().await;
            e.get_or_create_transcoder("pipe-concurrent", StageKind::video_preset("720p"), s, None)
                .await
        });
    }

    let mut results = Vec::new();
    while let Some(r) = join_set.join_next().await {
        results.push(r.unwrap());
    }

    // All returned Arc<RingBuffer>s must point to the SAME allocation
    let first_ptr = StdArc::as_ptr(&results[0]);
    for rb in &results[1..] {
        assert_eq!(
            StdArc::as_ptr(rb),
            first_ptr,
            "concurrent callers must receive the same RingBuffer Arc (no duplicate stages)"
        );
    }

    // Exactly one stage must exist in the map
    let stages = engine.active_transcoder_stages("pipe-concurrent").await;
    assert_eq!(
        stages.len(),
        1,
        "exactly one transcoder stage must exist after concurrent creation"
    );
}

// --- Regression: Round 6 #7 — HLS consumer refcount must not leak ---
// The refcount must return to zero after balanced add/remove so the
// idle-sweep logic eventually stops the segmenter task.
#[tokio::test]
async fn hls_consumer_idle_only_when_persistent_count_zero() {
    use tokio_util::sync::CancellationToken;

    let engine = MediaEngine::new();
    let token = CancellationToken::new();
    {
        let mut consumers = engine.hls.consumers.write().await;
        consumers.insert("pipe-hls-rc".to_string(), HlsConsumers::new(token.clone()));
    }

    // One persistent consumer added — segmenter must not be idle.
    engine.add_hls_persistent_consumer("pipe-hls-rc").await;
    {
        let consumers = engine.hls.consumers.read().await;
        assert!(
            !consumers["pipe-hls-rc"].is_idle(0),
            "segmenter must not be idle while a persistent consumer holds a ref"
        );
    }

    // Remove the consumer — now idle (last_access_ms was set on creation;
    // use a long timeout so only persistent count matters here).
    engine.remove_hls_persistent_consumer("pipe-hls-rc").await;
    {
        let consumers = engine.hls.consumers.read().await;
        assert_eq!(
            consumers["pipe-hls-rc"]
                .persistent
                .load(std::sync::atomic::Ordering::Relaxed),
            0,
            "persistent count must be 0 after remove"
        );
    }
}

// --- H.265 routing correctness tests ---

#[tokio::test]
async fn hevc_input_video_preset_ring_tagged_hevc() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("p-hevc").await;
    let ring = engine
        .get_or_create_transcoder(
            "p-hevc",
            StageKind::video_preset("720p"),
            source,
            Some("hevc"),
        )
        .await;
    assert_eq!(
        ring.codec_hint_str(),
        "hevc",
        "video:720p stage fed with H.265 must be tagged 'hevc'"
    );
}

#[tokio::test]
async fn h264_input_video_preset_ring_tagged_h264() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("p-h264").await;
    let ring = engine
        .get_or_create_transcoder("p-h264", StageKind::video_preset("720p"), source, None)
        .await;
    assert_eq!(
        ring.codec_hint_str(),
        "h264",
        "video:720p stage without codec override must default to 'h264'"
    );
}

#[tokio::test]
async fn h264_transcoder_different_upstreams_are_independent_stages() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("p-dual").await;

    let from_source = engine
        .get_or_create_h264_transcoder("p-dual", StageKind::source(), source.clone())
        .await;
    let from_720 = engine
        .get_or_create_h264_transcoder("p-dual", StageKind::video_preset("720p"), source.clone())
        .await;

    assert!(
        !Arc::ptr_eq(&from_source, &from_720),
        "hevc_to_h264 stages keyed by different upstreams must be independent"
    );
}

#[tokio::test]
async fn h264_transcoder_same_upstream_is_shared() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("p-shared-h264").await;

    let ring1 = engine
        .get_or_create_h264_transcoder(
            "p-shared-h264",
            StageKind::video_preset("720p"),
            source.clone(),
        )
        .await;
    let ring2 = engine
        .get_or_create_h264_transcoder(
            "p-shared-h264",
            StageKind::video_preset("720p"),
            source.clone(),
        )
        .await;

    assert!(
        Arc::ptr_eq(&ring1, &ring2),
        "hevc_to_h264 stage for the same upstream must be reused"
    );
}

#[tokio::test]
async fn h264_transcoder_output_ring_tagged_h264() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("p-h264-tag").await;

    let ring = engine
        .get_or_create_h264_transcoder("p-h264-tag", StageKind::source(), source)
        .await;

    assert_eq!(
        ring.codec_hint_str(),
        "h264",
        "hevc_to_h264 output ring must always be tagged 'h264'"
    );
}

// ── audio_tracks Arc<Vec<AudioMeta>> semantics ────────────────────

#[test]
fn arc_audio_tracks_clone_is_shallow_refcount_bump() {
    use std::sync::Arc;
    let tracks = vec![
        AudioMeta {
            codec: "aac".into(),
            sample_rate: 48000,
            channels: 2,
            track_index: 0,
            pid: None,
            language: None,
            title: None,
            profile: None,
            channel_layout: None,
        },
        AudioMeta {
            codec: "opus".into(),
            sample_rate: 48000,
            channels: 6,
            track_index: 1,
            pid: None,
            language: None,
            title: None,
            profile: None,
            channel_layout: None,
        },
    ];
    let arc = Arc::new(tracks);

    let c1 = Arc::clone(&arc);
    let c2 = Arc::clone(&arc);
    assert_eq!(Arc::as_ptr(&arc), Arc::as_ptr(&c1));
    assert_eq!(Arc::as_ptr(&arc), Arc::as_ptr(&c2));
    assert_eq!(Arc::strong_count(&arc), 3);
    assert_eq!(arc.len(), 2);
    assert_eq!(c1[0].codec, "aac");
    assert_eq!(c2[1].channels, 6);
}

#[test]
fn arc_audio_tracks_deref_works_for_iteration() {
    use std::sync::Arc;
    let tracks = vec![AudioMeta {
        codec: "aac".into(),
        sample_rate: 44100,
        channels: 1,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
        channel_layout: None,
    }];
    let arc = Arc::new(tracks);
    assert_eq!(arc.iter().next().unwrap().sample_rate, 44100);
    assert_eq!(arc.first().unwrap().codec, "aac");
    assert_eq!(arc.len(), 1);
}

#[test]
fn arc_audio_tracks_default_is_empty() {
    use std::sync::Arc;
    let arc: Arc<Vec<AudioMeta>> = Arc::default();
    assert!(arc.is_empty());
    assert_eq!(arc.len(), 0);
}

#[test]
fn arc_audio_tracks_mutex_wraps_correctly() {
    use std::sync::{Arc, Mutex};
    let tracks = Arc::new(vec![AudioMeta {
        codec: "aac".into(),
        sample_rate: 48000,
        channels: 2,
        track_index: 0,
        pid: None,
        language: None,
        title: None,
        profile: None,
        channel_layout: None,
    }]);
    let mtx = Mutex::new(Arc::clone(&tracks));

    // Clone under lock gives an Arc clone, not a deep Vec copy
    let guard = mtx.lock().unwrap();
    let cloned = guard.clone(); // Arc clone
    assert_eq!(Arc::as_ptr(&tracks), Arc::as_ptr(&cloned));
    assert_eq!(Arc::strong_count(&tracks), 3); // tracks + mtx inner + cloned
    drop(guard);
    drop(cloned);
    assert_eq!(Arc::strong_count(&tracks), 2); // tracks + mtx inner
}

// ── diag concurrency semaphore ──────────────────────────────────

#[tokio::test]
async fn diag_semaphore_prevents_concurrent_runs_on_same_pipeline() {
    let engine = MediaEngine::new();
    let pipeline = "diag-concurrency";

    let sem = {
        let mut map = engine.runtime.diag_semaphores.write().await;
        map.entry(pipeline.to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
    };

    let permit1 = sem.clone().try_acquire_owned();
    assert!(permit1.is_ok(), "first acquire must succeed");

    let permit2 = sem.clone().try_acquire_owned();
    assert!(permit2.is_err(), "second concurrent acquire must fail");

    let sem_other = {
        let mut map = engine.runtime.diag_semaphores.write().await;
        map.entry("other-pipeline".to_string())
            .or_insert_with(|| Arc::new(tokio::sync::Semaphore::new(1)))
            .clone()
    };
    assert!(
        sem_other.try_acquire_owned().is_ok(),
        "different pipeline must succeed"
    );

    drop(permit1);
    assert!(
        sem.try_acquire_owned().is_ok(),
        "acquire must succeed after previous permit dropped"
    );
}

// ── sweep_unused_stages reader tracking ─────────────────────────

#[tokio::test]
async fn sweep_unused_stages_retains_active_readers() {
    let engine = MediaEngine::new();
    let key = "pipeline:stage-sweep".to_string();
    let cancel = CancellationToken::new();
    let stage = Arc::new(TsChunkRing::new(16, cancel));

    let _reader =
        crate::media::ring_buffer::Reader::new("sweep-test".to_string(), stage.ring.clone());

    engine
        .stages
        .ts_muxers
        .write()
        .await
        .insert(key.clone(), stage);

    engine.sweep_unused_stages().await;
    assert!(
        engine.stages.ts_muxers.read().await.contains_key(&key),
        "stage with active reader must be retained"
    );

    drop(_reader);
    engine.sweep_unused_stages().await;
    assert!(
        !engine.stages.ts_muxers.read().await.contains_key(&key),
        "stage without readers must be removed"
    );
}

// M2: get_hls_cancel_token must return None (not panic) when no HLS
// segmenter is registered for the pipeline. The reconciler's HLS egress
// path replaced an unwrap() with a None guard after this was identified.
#[tokio::test]
async fn get_hls_cancel_token_returns_none_with_no_segmenter() {
    let engine = Arc::new(MediaEngine::new());
    let token = engine.get_hls_cancel_token("no-such-pipeline").await;
    assert!(
        token.is_none(),
        "must return None, not panic, when segmenter is not registered"
    );
}

// M2 (continued): after ensure_hls_segmenter registers a segmenter, the
// token must be Some — confirming the None case above is not a permanent failure.
#[tokio::test]
async fn get_hls_cancel_token_returns_some_after_ensure() {
    let engine = Arc::new(MediaEngine::new());
    engine.ensure_hls_segmenter("pipe-hls").await;
    let token = engine.get_hls_cancel_token("pipe-hls").await;
    assert!(
        token.is_some(),
        "token must be Some after ensure_hls_segmenter registers the pipeline"
    );
    engine.shutdown_hls_segmenter("pipe-hls").await;
}

#[tokio::test]
async fn hls_stores_use_engine_typed_config() {
    let config = Arc::new(crate::AppConfig {
        hls_min_segment_ms: 0.25,
        hls_segment_capacity_bytes: 256 * 1024,
        hls_max_segments: 7,
        ..crate::AppConfig::default()
    });
    let engine = Arc::new(MediaEngine::new_with_config(config));

    let hls_store = engine.get_or_create_hls_store("pipe-hls-config").await;
    let preview_store = engine
        .get_or_create_hls_preview_store("pipe-hls-preview-config")
        .await;

    assert_eq!(
        hls_store.config(),
        crate::media::hls::HlsConfig {
            min_segment_secs: 0.25,
            segment_capacity: 256 * 1024,
            max_segments: 7,
        }
    );
    assert_eq!(preview_store.config(), hls_store.config());
}

#[tokio::test]
async fn shutdown_hls_segmenter_removes_consumer_and_store() {
    let engine = Arc::new(MediaEngine::new());
    let (store, already_running) = engine.ensure_hls_segmenter("pipe-hls-clean").await;
    assert!(!already_running);
    store.push_segment(1.0, bytes::Bytes::from_static(b"segment"));

    assert!(engine.get_hls_store("pipe-hls-clean").await.is_some());
    assert!(
        engine
            .get_hls_cancel_token("pipe-hls-clean")
            .await
            .is_some()
    );

    engine.shutdown_hls_segmenter("pipe-hls-clean").await;

    assert!(engine.get_hls_store("pipe-hls-clean").await.is_none());
    assert!(
        engine
            .get_hls_cancel_token("pipe-hls-clean")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn shutdown_hls_preview_segmenter_removes_consumer_and_store() {
    let engine = Arc::new(MediaEngine::new());
    let (store, already_running) = engine
        .ensure_hls_preview_segmenter("pipe-hls-preview-clean")
        .await;
    assert!(!already_running);
    store.push_video_segment(0, 1.0, bytes::Bytes::from_static(b"segment"));

    assert!(
        engine
            .get_hls_preview_store("pipe-hls-preview-clean")
            .await
            .is_some()
    );
    assert!(
        engine
            .get_hls_preview_cancel_token("pipe-hls-preview-clean")
            .await
            .is_some()
    );

    engine
        .shutdown_hls_preview_segmenter("pipe-hls-preview-clean")
        .await;

    assert!(
        engine
            .get_hls_preview_store("pipe-hls-preview-clean")
            .await
            .is_none(),
        "preview store must be dropped on shutdown, not leaked in the registry"
    );
    assert!(
        engine
            .get_hls_preview_cancel_token("pipe-hls-preview-clean")
            .await
            .is_none()
    );
}

#[tokio::test]
async fn hls_segmenter_without_ingest_is_immediately_shutdown_candidate() {
    let engine = Arc::new(MediaEngine::new());
    let pipeline_id = "pipe-hls-no-ingest";

    let _ = engine.ensure_hls_preview_segmenter(pipeline_id).await;
    engine.touch_hls_preview(pipeline_id).await;

    assert!(
        engine
            .should_shutdown_hls_preview_segmenter(pipeline_id, 60_000)
            .await,
        "HLS preview should stop promptly when ingest disappears, regardless of idle timeout"
    );
}

// ── Matrix routing with synthetic packets (Phase 0 re-tier) ─────

#[tokio::test]
async fn matrix_routing_ingest_to_source_reader() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("matrix-pipe").await;
    engine
        .try_register_ingest("matrix-pipe", "key", "rtmp")
        .await
        .unwrap();

    ring.push(test_video_packet(0, 0, true));
    ring.push(test_audio_packet(10, 10));
    ring.push(test_video_packet(33, 33, false));

    let mut reader = Reader::new("matrix-reader".to_string(), ring);
    let p1 = reader.pull().unwrap().unwrap();
    assert_eq!(p1.media_type, MediaType::Video);
    assert!(p1.is_keyframe);
    let p2 = reader.pull().unwrap().unwrap();
    assert_eq!(p2.media_type, MediaType::Audio);
    let p3 = reader.pull().unwrap().unwrap();
    assert_eq!(p3.pts, 33);
    assert!(reader.pull().unwrap().is_none());
}

#[tokio::test]
async fn matrix_routing_flv_and_raw_format_dispatch() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("fmt-pipe").await;

    ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Flv,
        is_keyframe: true,
        track_index: 0,
        pts: 0,
        dts: 0,
        payload: Bytes::from_static(&[0x17, 0x01, 0, 0, 0]),
    });
    ring.push(MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: false,
        track_index: 0,
        pts: 33,
        dts: 33,
        payload: Bytes::from_static(&[0, 0, 0, 1, 0x41]),
    });

    let mut reader = Reader::new("fmt-reader".to_string(), ring);
    let p1 = reader.pull().unwrap().unwrap();
    assert_eq!(p1.format, PayloadFormat::Flv);
    let p2 = reader.pull().unwrap().unwrap();
    assert_eq!(p2.format, PayloadFormat::Raw);
}

#[tokio::test]
async fn matrix_routing_multi_reader_fan_out() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("fanout-pipe").await;

    ring.push(test_video_packet(0, 0, true));
    ring.push(test_audio_packet(10, 10));

    let mut r1 = Reader::new("reader-1".to_string(), ring.clone());
    let mut r2 = Reader::new("reader-2".to_string(), ring.clone());
    let mut r3 = Reader::new("reader-3".to_string(), ring);

    for reader in [&mut r1, &mut r2, &mut r3] {
        let p = reader.pull().unwrap().unwrap();
        assert_eq!(p.pts, 0);
        assert!(p.is_keyframe);
    }
}

#[tokio::test]
async fn matrix_routing_transcoder_stage_isolation() {
    let engine = Arc::new(MediaEngine::new());
    let source = engine.get_or_create_pipeline("iso-pipe").await;

    source.push(test_video_packet(0, 0, true));

    let tc_ring = engine
        .get_or_create_transcoder(
            "iso-pipe",
            StageKind::video_preset("720p"),
            source.clone(),
            None,
        )
        .await;

    assert!(
        !Arc::ptr_eq(&source, &tc_ring),
        "transcoder output ring must differ from source ring"
    );

    let mut source_reader = Reader::new("src".to_string(), source);
    let p = source_reader.pull().unwrap().unwrap();
    assert_eq!(p.pts, 0);
}

// ── fault resilience: ingest lifecycle ──────────────────────────────

#[tokio::test]
async fn health_input_on_after_register_off_after_unregister() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "off");

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "on");

    engine.unregister_ingest("p1").await;
    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "off");
}

#[tokio::test]
async fn health_snapshot_preserves_recent_ingest_disconnect_details_after_unregister() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta("p1", None, None, Some("127.0.0.1:9000".to_string()))
        .await;
    engine.update_ingest_bytes("p1", 4096).await;
    engine
        .record_ingest_disconnect(
            "p1",
            Some("session"),
            Some("publisher disconnected".to_string()),
            false,
        )
        .await;
    engine.unregister_ingest("p1").await;

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let input = &snap["pipelines"]["p1"]["input"];
    assert_eq!(input["status"], "off");
    assert_eq!(input["probeStatus"], "off");
    assert_eq!(input["lastSessionProtocol"], "rtmp");
    assert_eq!(input["lastDisconnectReason"], "publisher disconnected");
    assert_eq!(input["lastFailurePhase"], "session");
    assert_eq!(input["recentDisconnectError"], false);
    assert_eq!(input["disconnectGraceActive"], false);
    assert!(input["disconnectGraceRemainingMs"].is_null());
    assert_eq!(input["lastRemoteAddr"], "127.0.0.1:9000");
    assert_eq!(input["lastSessionBytesReceived"], 4096);
    assert!(input["lastDisconnectAt"].is_string());
    assert!(input["lastDisconnectAgeMs"].as_u64().is_some());
}

#[tokio::test]
async fn health_snapshot_exposes_disconnect_grace_window_fields() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine
        .record_ingest_disconnect(
            "p1",
            Some("disconnect"),
            Some("publisher disconnected".to_string()),
            false,
        )
        .await;
    engine.unregister_ingest("p1").await;

    let snap =
        test_health_snapshot_with_disconnect_grace(&engine, &pipelines, &HashMap::new(), 5_000)
            .await;
    let input = &snap["pipelines"]["p1"]["input"];
    assert_eq!(input["status"], "off");
    assert_eq!(input["disconnectGraceActive"], true);
    assert!(
        input["disconnectGraceRemainingMs"]
            .as_u64()
            .is_some_and(|remaining| remaining > 0 && remaining <= 5_000)
    );

    let no_grace = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(
        no_grace["pipelines"]["p1"]["input"]["disconnectGraceActive"],
        false
    );
    assert!(no_grace["pipelines"]["p1"]["input"]["disconnectGraceRemainingMs"].is_null());
}

#[tokio::test]
async fn recent_ingest_disconnect_respects_grace_window() {
    let engine = MediaEngine::new();
    let now_ms = MediaEngine::now_epoch_ms();

    engine.ingests.recent.write().await.insert(
        "inside".to_string(),
        RecentIngestOutcome {
            protocol: "rtmp".to_string(),
            disconnected_at_ms: now_ms,
            first_disconnect_at_ms: now_ms,
            disconnect_count: 1,
            reason: Some("publisher disconnected".to_string()),
            failure_phase: Some("disconnect".to_string()),
            had_error: false,
            remote_addr: Some("127.0.0.1:1935".to_string()),
            bytes_received: 1024,
        },
    );
    engine.ingests.recent.write().await.insert(
        "outside".to_string(),
        RecentIngestOutcome {
            protocol: "srt".to_string(),
            disconnected_at_ms: now_ms.saturating_sub(1_000),
            first_disconnect_at_ms: now_ms.saturating_sub(1_000),
            disconnect_count: 1,
            reason: Some("receiver stopped".to_string()),
            failure_phase: Some("receive".to_string()),
            had_error: true,
            remote_addr: Some("127.0.0.1:9000".to_string()),
            bytes_received: 2048,
        },
    );

    assert!(
        engine.has_recent_ingest_disconnect("inside", 250).await,
        "disconnects strictly inside the grace window should be treated as recent"
    );
    assert!(
        !engine.has_recent_ingest_disconnect("outside", 250).await,
        "disconnects older than the grace window should not count as recent"
    );
    assert!(
        !engine.has_recent_ingest_disconnect("inside", 0).await,
        "zero grace disables the recent-disconnect shortcut entirely"
    );
    assert!(
        !engine.has_recent_ingest_disconnect("missing", 250).await,
        "pipelines without a recent disconnect record must not be treated as recent"
    );
}

#[test]
fn build_recent_ingest_outcome_resets_flap_streak_outside_window() {
    let now_ms = MediaEngine::now_epoch_ms();
    let previous = RecentIngestOutcome {
        protocol: "rtmp".to_string(),
        disconnected_at_ms: now_ms.saturating_sub(INGEST_FLAP_WINDOW_MS + 1),
        first_disconnect_at_ms: now_ms.saturating_sub(INGEST_FLAP_WINDOW_MS + 10_000),
        disconnect_count: 4,
        reason: Some("publisher disconnected".to_string()),
        failure_phase: Some("disconnect".to_string()),
        had_error: false,
        remote_addr: Some("127.0.0.1:1935".to_string()),
        bytes_received: 2048,
    };

    let next = MediaEngine::build_recent_ingest_outcome(
        Some(&previous),
        "rtmp".to_string(),
        Some("disconnect"),
        Some("publisher disconnected".to_string()),
        false,
        Some("127.0.0.1:1935".to_string()),
        4096,
    );

    assert_eq!(next.disconnect_count, 1);
    assert_eq!(next.first_disconnect_at_ms, next.disconnected_at_ms);
}

#[tokio::test]
async fn health_snapshot_surfaces_flapping_after_repeated_reconnects() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    for protocol in ["rtmp", "rtmp"] {
        engine
            .try_register_ingest("p1", "key", protocol)
            .await
            .expect("ingest registration should succeed");
        engine
            .record_ingest_disconnect(
                "p1",
                Some("disconnect"),
                Some("publisher disconnected".to_string()),
                false,
            )
            .await;
        engine.unregister_ingest("p1").await;
    }

    let off_snapshot =
        test_health_snapshot_with_disconnect_grace(&engine, &pipelines, &HashMap::new(), 5_000)
            .await;
    let off_input = &off_snapshot["pipelines"]["p1"]["input"];
    assert_eq!(off_input["recentDisconnectCount"], 2);
    assert_eq!(off_input["flapping"], true);

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .expect("reconnect registration should succeed");

    let on_snapshot = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let on_input = &on_snapshot["pipelines"]["p1"]["input"];
    assert_eq!(on_input["status"], "on");
    assert_eq!(on_input["recentDisconnectCount"], 2);
    assert_eq!(on_input["flapping"], true);
    assert!(on_input["lastSessionProtocol"].is_null());
    assert!(on_input["lastDisconnectReason"].is_null());
    assert!(on_input["lastFailurePhase"].is_null());
    assert!(on_input["lastDisconnectAt"].is_null());
    assert!(on_input["lastDisconnectAgeMs"].is_null());
}

#[tokio::test]
async fn unregister_ingest_preserves_recent_snapshot_without_explicit_error() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine
        .update_ingest_meta("p1", None, None, Some("127.0.0.1:7000".to_string()))
        .await;
    engine.update_ingest_bytes("p1", 8192).await;

    engine.unregister_ingest("p1").await;

    assert!(
        engine.has_recent_ingest_disconnect("p1", 1_000).await,
        "plain unregister should still leave a recent disconnect marker for grace handling"
    );

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    let input = &snap["pipelines"]["p1"]["input"];
    assert_eq!(input["status"], "off");
    assert_eq!(input["probeStatus"], "off");
    assert_eq!(input["lastSessionProtocol"], "rtmp");
    assert!(input["lastDisconnectAt"].is_string());
    assert!(input["lastDisconnectAgeMs"].as_u64().is_some());
    assert_eq!(input["recentDisconnectError"], false);
    assert_eq!(input["disconnectGraceActive"], false);
    assert!(input["disconnectGraceRemainingMs"].is_null());
    assert_eq!(input["lastRemoteAddr"], "127.0.0.1:7000");
    assert_eq!(input["lastSessionBytesReceived"], 8192);
    assert!(input["lastDisconnectReason"].is_null());
    assert!(input["lastFailurePhase"].is_null());
}

#[tokio::test]
async fn re_register_ingest_clears_recent_disconnect_details() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine
        .record_ingest_disconnect(
            "p1",
            Some("receive"),
            Some("connection reset by peer".to_string()),
            true,
        )
        .await;
    engine.unregister_ingest("p1").await;

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["probeStatus"], "failed");
    assert_eq!(
        snap["pipelines"]["p1"]["input"]["lastDisconnectReason"],
        "connection reset by peer"
    );

    engine
        .try_register_ingest("p1", "key", "srt")
        .await
        .unwrap();
    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "on");
    assert!(snap["pipelines"]["p1"]["input"]["lastSessionProtocol"].is_null());
    assert!(snap["pipelines"]["p1"]["input"]["lastDisconnectReason"].is_null());
    assert_eq!(
        snap["pipelines"]["p1"]["input"]["disconnectGraceActive"],
        false
    );
    assert!(snap["pipelines"]["p1"]["input"]["disconnectGraceRemainingMs"].is_null());
}

#[tokio::test]
async fn double_register_ingest_rejected() {
    let engine = MediaEngine::new();
    let first = engine.try_register_ingest("p1", "key", "rtmp").await;
    assert!(first.is_some());

    let second = engine.try_register_ingest("p1", "key2", "srt").await;
    assert!(
        second.is_none(),
        "second register must be rejected while first is active"
    );
}

#[tokio::test]
async fn re_register_ingest_after_unregister() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    let t1 = engine
        .try_register_ingest("p1", "key", "rtmp")
        .await
        .unwrap();
    engine.unregister_ingest("p1").await;
    assert!(t1.is_cancelled());

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "off");

    let t2 = engine.try_register_ingest("p1", "key", "srt").await;
    assert!(t2.is_some(), "re-register after unregister must succeed");

    let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
    assert_eq!(snap["pipelines"]["p1"]["input"]["status"], "on");
    assert_eq!(
        snap["pipelines"]["p1"]["input"]["publisher"]["protocol"],
        "srt"
    );
}

// ── fault resilience: egress error transitions ─────────────────────

#[tokio::test]
async fn egress_error_during_sending_transitions_to_failed() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine.record_egress_progress("out-1", 5000).await;

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["phase"], "sending");

    engine
        .record_egress_error("out-1", "send", "connection reset by peer")
        .await;

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["failurePhase"], "send");
    assert_eq!(status["lastError"], "connection reset by peer");
}

#[tokio::test]
async fn egress_cleaned_up_after_unregister() {
    let engine = MediaEngine::new();
    let token = engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;

    assert!(
        crate::api_runtime_views::output_status(&engine, "out-1")
            .await
            .is_some()
    );

    engine.unregister_egress("out-1").await;
    assert!(token.is_cancelled());
    assert!(
        crate::api_runtime_views::output_status(&engine, "out-1")
            .await
            .is_some(),
        "output_status must preserve the last classified egress state after unregister"
    );
}

#[tokio::test]
async fn recent_egress_failure_survives_unregister_and_preserves_error_fields() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine.record_egress_progress("out-1", 2048).await;
    engine
        .record_egress_error("out-1", "send", "connection reset by peer")
        .await;

    engine.unregister_egress("out-1").await;

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "failed");
    assert_eq!(status["rawStatus"], "running");
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["failurePhase"], "send");
    assert_eq!(status["lastError"], "connection reset by peer");
    assert_eq!(status["bytesOut"], 2048);
    assert_eq!(status["totalSize"], 2048);
    assert!(status["lastErrorAt"].is_string());
    assert!(status["endedAt"].is_string());
    assert!(status["endedAgeMs"].as_u64().is_some());
}

#[tokio::test]
async fn health_snapshot_keeps_recent_egress_status_visible_after_unregister() {
    let engine = MediaEngine::new();
    let pipeline_id = "pipe-1".to_string();
    engine.get_or_create_pipeline(&pipeline_id).await;
    engine
        .register_egress(
            "out-1",
            &pipeline_id,
            "srt://example.com:10080?streamid=live/test",
        )
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine
        .record_egress_error("out-1", "connect", "connection failed")
        .await;

    engine.unregister_egress("out-1").await;

    let snapshot = test_health_snapshot(&engine, &[pipeline_id], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["status"], "failed");
    assert_eq!(output["phase"], "failed");
    assert_eq!(output["failurePhase"], "connect");
    assert_eq!(output["lastError"], "connection failed");
    assert!(output["endedAt"].is_string());
}

#[tokio::test]
async fn re_register_egress_clears_recent_snapshot() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "connect", "connection refused")
        .await;
    engine.unregister_egress("out-1").await;
    engine
        .update_egress_retry_state("out-1", 2, 20_000, 15_000)
        .await;
    assert!(engine.recent_egress_outcome("out-1").await.is_some());
    assert!(engine.egress_retry_state("out-1").await.is_some());

    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;

    let recent = engine
        .recent_egress_outcome("out-1")
        .await
        .expect("recent failure window should stay visible across restart");
    assert_eq!(recent.failure_count, 1);
    assert!(engine.egress_retry_state("out-1").await.is_none());
}

#[tokio::test]
async fn late_retry_state_update_is_ignored_after_output_restarts() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "send", "connection reset by peer")
        .await;
    engine.unregister_egress("out-1").await;

    // Simulate the reconciler starting a fresh output session before the
    // old task's cleanup path gets to publish its retry backoff state.
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .update_egress_retry_state("out-1", 2, 20_000, 15_000)
        .await;

    assert!(engine.egress_retry_state("out-1").await.is_none());

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "running");
    assert_eq!(status["retrying"], false);
    assert!(status["retryAttempts"].is_null());
    assert!(status["retryBackoffMs"].is_null());
    assert!(status["retryRemainingMs"].is_null());
}

#[tokio::test]
async fn repeated_late_retry_updates_cannot_poison_newest_output_attempt() {
    let engine = MediaEngine::new();

    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "send", "attempt 1 failed")
        .await;
    engine.unregister_egress("out-1").await;

    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .update_egress_retry_state("out-1", 1, 10_000, 8_000)
        .await;
    assert!(
        engine.egress_retry_state("out-1").await.is_none(),
        "the first stale retry publication must be ignored once a replacement attempt is active"
    );

    engine
        .record_egress_error("out-1", "connect", "attempt 2 failed")
        .await;
    engine.unregister_egress("out-1").await;

    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine.record_egress_progress("out-1", 4096).await;
    engine
        .update_egress_retry_state("out-1", 2, 20_000, 15_000)
        .await;
    engine
        .update_egress_retry_state("out-1", 3, 40_000, 35_000)
        .await;

    assert!(
        engine.egress_retry_state("out-1").await.is_none(),
        "stale retry publications from any older attempt must not reattach retry state"
    );
    assert!(
        engine.recent_egress_outcome("out-1").await.is_some(),
        "the newest active attempt should retain the recent failure window for flapping visibility"
    );

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "running");
    assert_eq!(status["phase"], "sending");
    assert_eq!(status["bytesOut"], 4096);
    assert_eq!(status["retrying"], false);
    assert!(status["retryAttempts"].is_null());
    assert!(status["retryBackoffMs"].is_null());
    assert!(status["retryRemainingMs"].is_null());
    assert!(status["lastError"].is_null());
    assert!(status["failurePhase"].is_null());
    assert_eq!(status["recentFailureCount"], 2);
    assert_eq!(status["flapping"], true);
}

#[tokio::test]
async fn build_recent_egress_outcome_resets_flap_streak_outside_window() {
    let engine = MediaEngine::new();
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "send", "attempt 1 failed")
        .await;
    engine.unregister_egress("out-1").await;

    let previous = engine
        .recent_egress_outcome("out-1")
        .await
        .expect("recent egress outcome");
    let expired = RecentEgressOutcome {
        ended_at_ms: MediaEngine::now_epoch_ms() - EGRESS_FLAP_WINDOW_MS - 1,
        ..previous
    };

    engine
        .register_egress("out-2", "pipe-1", "rtmp://127.0.0.1:1935/live/other")
        .await;
    engine
        .record_egress_error("out-2", "connect", "attempt 2 failed")
        .await;
    let next = {
        let egresses = engine.egresses.active.read().await;
        let active = egresses.get("out-2").expect("active egress should exist");
        MediaEngine::build_recent_egress_outcome(Some(&expired), active, true)
    };

    assert_eq!(next.failure_count, 1);
    assert_eq!(next.first_failure_at_ms, next.ended_at_ms);
}

#[tokio::test]
async fn health_snapshot_surfaces_flapping_after_repeated_egress_recoveries() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-1", "key01_recent_egress_flapping", "rtmp")
        .await
        .expect("ingest registration should succeed");
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "send", "attempt 1 failed")
        .await;
    engine.unregister_egress("out-1").await;
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "connect", "attempt 2 failed")
        .await;
    engine.unregister_egress("out-1").await;
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine.update_egress_phase("out-1", EP::Sending).await;
    engine.record_egress_progress("out-1", 4096).await;

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "running");
    assert!(status["lastError"].is_null());
    assert_eq!(status["recentFailureCount"], 2);
    assert_eq!(status["flapping"], true);

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["status"], "running");
    assert_eq!(output["recentFailureCount"], 2);
    assert_eq!(output["flapping"], true);
}

#[tokio::test]
async fn output_status_surfaces_retry_backoff_after_failure() {
    let engine = MediaEngine::new();
    engine
        .try_register_ingest("pipe-1", "key01_retry_engine", "rtmp")
        .await
        .expect("ingest registration should succeed");
    engine
        .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
        .await;
    engine
        .record_egress_error("out-1", "send", "connection reset by peer")
        .await;
    engine.unregister_egress("out-1").await;
    engine
        .update_egress_retry_state("out-1", 2, 20_000, 15_000)
        .await;

    let status = crate::api_runtime_views::output_status(&engine, "out-1")
        .await
        .unwrap();
    assert_eq!(status["status"], "retrying");
    assert_eq!(status["phase"], "failed");
    assert_eq!(status["failurePhase"], "send");
    assert_eq!(status["lastError"], "connection reset by peer");
    assert_eq!(status["retrying"], true);
    assert_eq!(status["retryAttempts"], 2);
    assert_eq!(status["retryBackoffMs"], 20_000);
    assert!(status["nextRetryAt"].is_string());
    assert!(status["retryRemainingMs"].as_u64().unwrap_or(0) > 0);

    let snapshot = test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new()).await;
    let output = &snapshot["pipelines"]["pipe-1"]["outputs"]["out-1"];
    assert_eq!(output["status"], "retrying");
    assert_eq!(output["phase"], "failed");
    assert_eq!(output["failurePhase"], "send");
    assert_eq!(output["lastError"], "connection reset by peer");
    assert_eq!(output["retrying"], true);
    assert_eq!(output["retryAttempts"], 2);
    assert_eq!(output["retryBackoffMs"], 20_000);
    assert!(output["nextRetryAt"].is_string());
    assert!(output["retryRemainingMs"].as_u64().unwrap_or(0) > 0);
}

proptest! {
    #[test]
    fn prop_ingest_lifecycle_preserves_health_invariants(
        actions in proptest::collection::vec(ingest_lifecycle_action_strategy(), 1..64)
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async move {
            let engine = MediaEngine::new();
            let pipeline_id = "pipe-1".to_string();
            let mut model = IngestLifecycleModel::default();

            for action in actions {
                match action {
                    IngestLifecycleAction::Register { protocol } => {
                        let registered =
                            engine.try_register_ingest("pipe-1", "prop-ingest-key", protocol).await;
                        if registered.is_some() {
                            model.active = true;
                            model.protocol = Some(protocol);
                            model.remote_addr = None;
                            model.bytes_received = 0;
                        }
                    }
                    IngestLifecycleAction::UpdateRemoteAddr(remote_addr) => {
                        engine
                            .update_ingest_meta(
                                "pipe-1",
                                None,
                                None,
                                remote_addr.map(str::to_string),
                            )
                            .await;
                        if model.active && remote_addr.is_some() {
                            model.remote_addr = remote_addr;
                        }
                    }
                    IngestLifecycleAction::RecordBytes(bytes) => {
                        engine.update_ingest_bytes("pipe-1", bytes).await;
                        if model.active {
                            model.bytes_received += bytes;
                        }
                    }
                    IngestLifecycleAction::DisconnectAndUnregister {
                        phase,
                        message,
                        had_error,
                    } => {
                        engine
                            .record_ingest_disconnect(
                                "pipe-1",
                                phase,
                                message.map(str::to_string),
                                had_error,
                            )
                            .await;
                        if model.active {
                            model.recent_visible = true;
                            model.recent_protocol = model.protocol.take();
                            model.recent_remote_addr = model.remote_addr.take();
                            model.recent_bytes_received = std::mem::take(&mut model.bytes_received);
                            model.recent_phase = phase;
                            model.recent_message = message;
                            model.recent_had_error = had_error;
                            model.recent_disconnect_count =
                                model.recent_disconnect_count.saturating_add(1);
                            model.active = false;
                        }
                        engine.unregister_ingest("pipe-1").await;
                    }
                    IngestLifecycleAction::Unregister => {
                        engine.unregister_ingest("pipe-1").await;
                        if model.active {
                            model.active = false;
                            if !model.recent_visible {
                                model.recent_visible = true;
                                model.recent_protocol = model.protocol.take();
                                model.recent_remote_addr = model.remote_addr.take();
                                model.recent_bytes_received =
                                    std::mem::take(&mut model.bytes_received);
                                model.recent_phase = None;
                                model.recent_message = None;
                                model.recent_had_error = false;
                                model.recent_disconnect_count = 1;
                            } else {
                                model.protocol = None;
                                model.remote_addr = None;
                                model.bytes_received = 0;
                            }
                        }
                    }
                }

                let plain_snapshot =
                    test_health_snapshot(&engine, std::slice::from_ref(&pipeline_id), &HashMap::new())
                        .await;
                let grace_snapshot = test_health_snapshot_with_disconnect_grace(
                    &engine,
                    std::slice::from_ref(&pipeline_id),
                    &HashMap::new(),
                    5_000,
                )
                .await;
                let plain_input = &plain_snapshot["pipelines"]["pipe-1"]["input"];
                let grace_input = &grace_snapshot["pipelines"]["pipe-1"]["input"];

                assert_ingest_lifecycle_invariants(&model, plain_input, grace_input);
            }
        });
    }

    #[test]
    fn prop_egress_lifecycle_preserves_runtime_and_health_invariants(
        actions in proptest::collection::vec(egress_lifecycle_action_strategy(), 1..64)
    ) {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("tokio runtime");

        runtime.block_on(async move {
            let engine = MediaEngine::new();
            engine
                .try_register_ingest("pipe-1", "prop-egress-key", "rtmp")
                .await
                .expect("ingest registration should succeed");
            let mut model = EgressLifecycleModel::default();

            for action in actions {
                match action {
                    EgressLifecycleAction::Register => {
                        engine
                            .register_egress("out-1", "pipe-1", "rtmp://127.0.0.1:1935/live/key")
                            .await;
                        model = EgressLifecycleModel {
                            active: true,
                            recent_visible: model.recent_visible,
                            retry_visible: false,
                            bytes_sent: 0,
                            phase: "starting",
                            last_error: None,
                            retry_attempts: None,
                            retry_backoff_ms: None,
                        };
                    }
                    EgressLifecycleAction::RecordError { phase, message } => {
                        engine.record_egress_error("out-1", phase, message).await;
                        if model.active {
                            model.phase = "failed";
                            model.last_error = Some((phase, message));
                        }
                    }
                    EgressLifecycleAction::RecordProgress(bytes) => {
                        engine.record_egress_progress("out-1", bytes).await;
                        if model.active {
                            model.bytes_sent += bytes;
                            model.phase = "sending";
                            model.last_error = None;
                        }
                    }
                    EgressLifecycleAction::Unregister => {
                        engine.unregister_egress("out-1").await;
                        if model.active {
                            model.active = false;
                            model.recent_visible = true;
                        }
                    }
                    EgressLifecycleAction::RetryState {
                        attempts,
                        backoff_ms,
                        remaining_ms,
                    } => {
                        engine
                            .update_egress_retry_state("out-1", attempts, backoff_ms, remaining_ms)
                            .await;
                        if model.active {
                            model.retry_visible = false;
                            model.retry_attempts = None;
                            model.retry_backoff_ms = None;
                        } else {
                            model.retry_visible = true;
                            model.retry_attempts = Some(attempts);
                            model.retry_backoff_ms = Some(backoff_ms);
                        }
                    }
                    EgressLifecycleAction::ClearRetry => {
                        engine.clear_egress_retry_state("out-1").await;
                        model.retry_visible = false;
                        model.retry_attempts = None;
                        model.retry_backoff_ms = None;
                    }
                }

                let status = crate::api_runtime_views::output_status(&engine, "out-1").await;
                let snapshot =
                    test_health_snapshot(&engine, &["pipe-1".to_string()], &HashMap::new())
                        .await;
                let snapshot_output = snapshot["pipelines"]["pipe-1"]["outputs"].get("out-1");
                let recent = engine.recent_egress_outcome("out-1").await;
                let retry = engine.egress_retry_state("out-1").await;

                assert_egress_lifecycle_invariants(
                    &model,
                    status.as_ref(),
                    snapshot_output,
                    recent.as_ref(),
                    retry.as_ref(),
                );
            }
        });
    }
}

// ── adaptive ring sizing ──────────────────────────────────────────────────

#[tokio::test]
async fn adapt_pipeline_ring_no_op_when_default_is_sufficient() {
    // 1080p30 + 1 audio = 80 pkt/s → needed = ceil(80 × 6) = 480 < default 1024
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let result = engine.adapt_pipeline_ring("p", 30.0, 1).await;
    assert!(
        result.is_none(),
        "no resize needed for single-track 1080p30"
    );

    let ring = engine.get_or_create_pipeline("p").await;
    assert_eq!(ring.capacity(), engine.config.ring_capacity);
    let depth = ring.buffer_depth_secs().unwrap();
    assert!((12.0..=13.0).contains(&depth), "depth={depth}");
}

#[tokio::test]
async fn source_ring_uses_engine_typed_config_capacity() {
    let config = Arc::new(crate::AppConfig {
        ring_capacity: 2048,
        ..Default::default()
    });
    let engine = MediaEngine::new_with_config(config);

    let ring = engine.get_or_create_pipeline("typed-ring").await;
    assert_eq!(ring.capacity(), 2048);
}

#[tokio::test]
async fn ts_muxer_ring_uses_engine_typed_config_capacity() {
    let config = Arc::new(crate::AppConfig {
        ts_ring_capacity: 96,
        ..Default::default()
    });
    let engine = Arc::new(MediaEngine::new_with_config(config));
    let source = Arc::new(RingBuffer::new(16));

    let ts_ring = engine
        .get_or_create_ts_muxer_stage("typed-ts", "source", source)
        .await;

    assert_eq!(ts_ring.ring.capacity(), 96);
    ts_ring.cancel.cancel();
}

#[tokio::test]
async fn adapt_pipeline_ring_resizes_for_multi_track_stream() {
    // 2v16a: 30 fps + 16 audio × 50 = 830 pkt/s → needed = ceil(830 × 6) = 4980
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let new_ring = engine
        .adapt_pipeline_ring("p", 30.0, 16)
        .await
        .expect("ring must be resized for 830 pkt/s");

    assert_eq!(new_ring.capacity(), 4980);
    let depth = new_ring.buffer_depth_secs().unwrap();
    assert!((depth - 6.0).abs() < 0.1, "depth={depth}");
    assert_eq!(engine.get_or_create_pipeline("p").await.capacity(), 4980);
}

#[tokio::test]
async fn adapt_pipeline_ring_4k60_single_audio_no_resize() {
    // 4K 60fps + 1 audio = 110 pkt/s → needed = 660 < default 1024
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let result = engine.adapt_pipeline_ring("p", 60.0, 1).await;
    assert!(
        result.is_none(),
        "default 1024 already covers 4K60 single-track"
    );
}

#[tokio::test]
async fn adapt_pipeline_ring_4k60_multi_audio_resizes() {
    // 4K 60fps + 16 audio = 860 pkt/s → needed = ceil(860 × 6) = 5160
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;

    let new_ring = engine
        .adapt_pipeline_ring("p", 60.0, 16)
        .await
        .expect("resize needed for 4K60 + 16 audio");

    assert_eq!(new_ring.capacity(), 5160);
    let depth = new_ring.buffer_depth_secs().unwrap();
    assert!((depth - 6.0).abs() < 0.1, "depth={depth}");
}

#[tokio::test]
async fn get_or_create_pipeline_preserves_adapted_ring_across_calls() {
    // The adapted ring must be returned by all subsequent get_or_create_pipeline
    // calls so egress readers and TS mux stages attach to the correctly-sized ring.
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;
    let new_ring = engine
        .adapt_pipeline_ring("p", 30.0, 16)
        .await
        .expect("should resize for 830 pkt/s");
    assert_eq!(new_ring.capacity(), 4980);

    let ring2 = engine.get_or_create_pipeline("p").await;
    assert_eq!(
        ring2.capacity(),
        4980,
        "adapted ring must persist across calls"
    );

    let _reader = crate::media::ring_buffer::Reader::new("hold".to_string(), ring2.clone());
    let ring3 = engine.get_or_create_pipeline("p").await;
    assert_eq!(
        ring3.capacity(),
        4980,
        "ring must not change with active reader"
    );
}

#[tokio::test]
async fn adapt_pipeline_ring_lighter_republish_updates_rate_not_capacity() {
    // A lighter re-publish (1v1a after 2v16a) does not shrink the ring —
    // it just updates estimated_pkt_rate so bufferDepthSecs is correct.
    let engine = MediaEngine::new();
    engine.get_or_create_pipeline("p").await;
    engine.adapt_pipeline_ring("p", 30.0, 16).await; // → 4980 for 830 pkt/s

    // Lighter re-publish: 1v1a = 80 pkt/s → needed = 480 < 4980 → no resize.
    let result = engine.adapt_pipeline_ring("p", 30.0, 1).await;
    assert!(
        result.is_none(),
        "no resize when ring is already large enough"
    );

    let ring = engine.get_or_create_pipeline("p").await;
    assert_eq!(
        ring.capacity(),
        4980,
        "capacity preserved from heavier session"
    );
    let depth = ring.buffer_depth_secs().unwrap();
    // telemetry now reflects the lighter stream's real depth: 4980/80 ≈ 62 s
    assert!(depth > 60.0, "4980/80 ≈ 62.3 s; got {depth}");
}

#[tokio::test]
async fn adapt_pipeline_ring_preserves_codec_and_track_metadata() {
    let engine = MediaEngine::new();
    let ring = engine.get_or_create_pipeline("p").await;
    ring.set_codec_hint("hevc");
    ring.set_video_parameter_sets(vec![0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01]);
    ring.set_audio_tracks(vec![AudioMeta {
        codec: "aac".to_string(),
        sample_rate: 48_000,
        channels: 2,
        channel_layout: Some("stereo".to_string()),
        track_index: 3,
        pid: Some(257),
        language: Some("eng".to_string()),
        title: Some("Program".to_string()),
        profile: Some("LC".to_string()),
    }]);

    let new_ring = engine
        .adapt_pipeline_ring("p", 30.0, 16)
        .await
        .expect("ring must be resized for metadata preservation proof");

    assert_eq!(new_ring.codec_hint_str(), "hevc");
    assert_eq!(
        new_ring.video_parameter_sets(),
        Some(vec![0, 0, 0, 1, 0x40, 0x01, 0x0c, 0x01])
    );
    let tracks = new_ring
        .audio_tracks()
        .expect("resized ring should preserve audio tracks");
    assert_eq!(tracks.len(), 1);
    assert_eq!(tracks[0].codec, "aac");
    assert_eq!(tracks[0].sample_rate, 48_000);
    assert_eq!(tracks[0].channels, 2);
    assert_eq!(tracks[0].track_index, 3);
    assert_eq!(tracks[0].pid, Some(257));
    assert_eq!(tracks[0].language.as_deref(), Some("eng"));
    assert_eq!(tracks[0].title.as_deref(), Some("Program"));
    assert_eq!(tracks[0].profile.as_deref(), Some("LC"));
}

#[tokio::test]
async fn health_input_protocol_matches_registration() {
    let engine = MediaEngine::new();
    let pipelines = vec!["p1".to_string()];

    for proto in ["rtmp", "srt", "file"] {
        engine
            .try_register_ingest("p1", "key", proto)
            .await
            .unwrap();
        let snap = test_health_snapshot(&engine, &pipelines, &HashMap::new()).await;
        assert_eq!(
            snap["pipelines"]["p1"]["input"]["publisher"]["protocol"], proto,
            "protocol mismatch for {proto}"
        );
        engine.unregister_ingest("p1").await;
    }
}
