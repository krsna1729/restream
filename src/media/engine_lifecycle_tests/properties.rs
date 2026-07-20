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
