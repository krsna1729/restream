use super::*;
use crate::domain::state::{DesiredOutputState, EgressPhase as EP};
use crate::media::avio::MemoryQueue;
use crate::media::engine::MediaEngine;
use crate::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat, Reader};
use bytes::Bytes;
use proptest::prelude::*;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::process::Command;

#[path = "engine_lifecycle_tests.rs"]
mod engine_lifecycle_tests;
#[path = "engine_poison_recovery_tests.rs"]
mod engine_poison_recovery_tests;
#[path = "engine_stage_tests.rs"]
mod engine_stage_tests;

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

async fn test_health_summary_snapshot(engine: &MediaEngine) -> serde_json::Value {
    crate::api_runtime_views::health_summary_snapshot(engine, &[], &HashMap::new(), 0).await
}

#[test]
fn pipe_metrics_snapshot_correctness() {
    let pm = PipeMetrics::default();
    let snap = pm.snapshot();

    // All counters start at zero; avg fields are also zero.
    assert_eq!(snap.stalls, 0);
    assert_eq!(snap.stall_us, 0);
    assert_eq!(snap.avg_stall_us, 0);
    assert_eq!(snap.idles, 0);
    assert_eq!(snap.idle_us, 0);
    assert_eq!(snap.avg_idle_us, 0);

    // Stdin stall accumulation and average.
    pm.record_stall(2_000);
    pm.record_stall(6_000);
    let snap = pm.snapshot();
    assert_eq!(snap.stalls, 2);
    assert_eq!(snap.stall_us, 8_000);
    assert_eq!(snap.avg_stall_us, 4_000);

    // Stdout idle accumulation and average.
    pm.record_idle(3_000);
    let snap = pm.snapshot();
    assert_eq!(snap.idles, 1);
    assert_eq!(snap.idle_us, 3_000);
    assert_eq!(snap.avg_idle_us, 3_000);

    // StageMetricsSnapshot is a fixed typed struct with no pipe-metrics
    // fields, so the two counter families can no longer be conflated at the
    // type level.
    let sm = StageMetrics::new();
    sm.record_in(64);
    let ssnap = sm.snapshot();
    assert_eq!(ssnap.packets_in, 1);
    assert_eq!(ssnap.bytes_in, 64);
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
    let (store, already_running, _cancel_token) = engine
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
async fn health_snapshot_exposes_runtime_limit_and_rtmp_listener_errors() {
    let engine = MediaEngine::new();
    engine
        .runtime
        .rtmp_listener_stats
        .rtmp_accept_errors
        .store(7, Ordering::Relaxed);
    engine
        .runtime
        .rtmp_listener_stats
        .rtmp_fd_exhaustion_errors
        .store(3, Ordering::Relaxed);

    let snapshot = test_health_snapshot(&engine, &[], &HashMap::new()).await;

    assert_eq!(snapshot["rtmpListener"]["acceptErrors"], 7);
    assert_eq!(snapshot["rtmpListener"]["fdExhaustionErrors"], 3);
    assert_eq!(
        snapshot["runtimeLimits"]["nofile"]["configured"],
        engine.config.tuning.nofile_limit
    );
    let host_settings = snapshot["hostSettings"]
        .as_array()
        .expect("host settings should be an array");
    assert!(
        host_settings
            .iter()
            .any(|setting| setting["key"] == "runtime.nofile"),
        "host settings should expose the process nofile row"
    );
    assert!(
        host_settings
            .iter()
            .any(|setting| setting["key"] == "net.core.rmem_max"),
        "host settings should expose the SRT receive buffer ceiling row"
    );
    assert!(
        host_settings
            .iter()
            .any(|setting| setting["key"] == "net.core.wmem_max"),
        "host settings should expose the SRT send buffer ceiling row"
    );
    assert!(
        host_settings
            .iter()
            .any(|setting| setting["key"] == "runtime.tokio.worker_threads"),
        "host settings should expose Tokio worker sizing"
    );
    assert!(
        host_settings
            .iter()
            .any(|setting| setting["key"] == "runtime.tokio.max_blocking_threads"),
        "host settings should expose Tokio blocking-pool sizing"
    );
    assert!(
        !host_settings
            .iter()
            .any(|setting| setting["key"] == "kernel.perf_event_paranoid"),
        "host settings should not expose profiling-only settings"
    );
    assert!(
        snapshot["runtimeLimits"]["nofile"]
            .get("satisfied")
            .and_then(|value| value.as_bool())
            .is_some(),
        "nofile limit snapshot should expose whether the configured target is satisfied"
    );
}

#[tokio::test]
async fn health_summary_includes_runtime_host_settings() {
    let engine = MediaEngine::new();

    let summary = test_health_summary_snapshot(&engine).await;
    assert!(
        summary["hostSettings"]
            .as_array()
            .is_some_and(|settings| !settings.is_empty()),
        "summary health should expose runtime host settings"
    );
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
async fn health_snapshot_drops_egress_registry_before_stage_blocked_lookup() {
    let engine = Arc::new(MediaEngine::new());
    let key = StageKey::new("pipe-1", StageKind::video_preset("720p"));
    engine
        .register_egress_attempt("out-1", "pipe-1", "rtmp://example.com/live/key", Some(key))
        .await;

    let stage_write = engine.stages.runtimes.write().await;
    let health = {
        let engine = engine.clone();
        tokio::spawn(async move {
            test_health_snapshot(&engine, &[String::from("pipe-1")], &HashMap::new()).await
        })
    };

    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    let active_write = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        engine.egresses.active.write(),
    )
    .await
    .expect("health snapshot must not hold egresses.active while awaiting stage registries");
    drop(active_write);

    drop(stage_write);
    health.await.expect("health snapshot task should not panic");
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
    let output = crate::application::models::Output {
        id: "out-srt".to_string(),
        pipeline_id: pipeline_id.to_string(),
        name: "SRT Target".to_string(),
        url: "srt://example.com:9000?streamid=publish:test".to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        config: crate::domain::output_spec::OutputConfig::source(),
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

    let output = crate::application::models::Output {
        id: "out-failed".to_string(),
        pipeline_id: pipeline_id.to_string(),
        name: "Failed Target".to_string(),
        url: "rtmp://example/live/test".to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        config: crate::domain::output_spec::OutputConfig::source(),
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

    let output = crate::application::models::Output {
        id: "out-graph-stale-codec".to_string(),
        pipeline_id: pipeline_id.to_string(),
        name: "Graph RTMP".to_string(),
        url: "rtmp://example/live/test".to_string(),
        monitoring_url: None,
        desired_state: DesiredOutputState::Running,
        config: crate::domain::output_spec::OutputConfig::preset("h264").with_audio(
            crate::domain::audio_routing::AudioRouting::SelectTracks { tracks: vec![1] },
        ),
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
    let stale_plain = StageKind::codec_edge("hevc_to_h264", StageKind::video_preset("h264"));
    let stale_qualified = StageKind::codec_edge(
        "hevc_to_h264",
        StageKind::video_preset_with_codec("h264", "hevc"),
    );
    assert!(
        stages
            .iter()
            .any(|(stage, live)| *live && (*stage == stale_plain || *stage == stale_qualified)),
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
