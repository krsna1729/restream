use super::*;

#[path = "engine_lifecycle_tests/egress.rs"]
mod egress_tests;
#[path = "engine_lifecycle_tests/ingest.rs"]
mod ingest_tests;
#[path = "engine_lifecycle_tests/properties.rs"]
mod property_tests;
#[path = "engine_lifecycle_tests/ring.rs"]
mod ring_tests;

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
