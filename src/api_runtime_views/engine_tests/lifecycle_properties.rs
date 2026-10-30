use super::*;

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
