use std::sync::Mutex;

use super::{
    AppConfig, DEFAULT_MEDIA_DIR, EXTERNAL_FFMPEG_LIVE_LIVENESS_FLOOR, EgressFabricConfig,
    EgressRolloutMode, RuntimeTuning, ServerPorts, TokioRuntimeConfig, backend_policy_from_env,
    default_egress_fabric_shards, default_tokio_worker_threads, derive_external_ffmpeg_permits,
};
use crate::planner::BackendPolicy;

static ENV_LOCK: Mutex<()> = Mutex::new(());

fn with_env_vars(vars: &[(&str, &str)], f: impl FnOnce()) {
    let _guard = ENV_LOCK.lock().unwrap();
    let previous = vars
        .iter()
        .map(|(name, _)| ((*name).to_string(), std::env::var(name).ok()))
        .collect::<Vec<_>>();
    unsafe {
        for (name, value) in vars {
            std::env::set_var(name, value);
        }
    }
    f();
    unsafe {
        for (name, value) in previous {
            if let Some(value) = value {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }
}

fn with_env_overlay(vars: &[(&str, &str)], removed: &[&str], f: impl FnOnce()) {
    let _guard = ENV_LOCK.lock().unwrap();
    let previous_vars = vars
        .iter()
        .map(|(name, _)| ((*name).to_string(), std::env::var(name).ok()))
        .collect::<Vec<_>>();
    let previous_removed = removed
        .iter()
        .map(|name| ((*name).to_string(), std::env::var(name).ok()))
        .collect::<Vec<_>>();
    unsafe {
        for (name, value) in vars {
            std::env::set_var(name, value);
        }
        for name in removed {
            std::env::remove_var(name);
        }
    }
    f();
    unsafe {
        for (name, value) in previous_vars.into_iter().chain(previous_removed) {
            if let Some(value) = value {
                std::env::set_var(name, value);
            } else {
                std::env::remove_var(name);
            }
        }
    }
}

#[test]
fn server_ports_are_loaded_by_config_module() {
    with_env_vars(
        &[
            ("RESTREAM_HTTP_PORT", "4040"),
            ("RESTREAM_RTMP_PORT", "2935"),
            ("RESTREAM_SRT_PORT", "11080"),
        ],
        || {
            let ports = ServerPorts::from_env();
            assert_eq!(ports.http, 4040);
            assert_eq!(ports.rtmp, 2935);
            assert_eq!(ports.srt, 11080);
        },
    );
}

#[test]
fn server_ports_reject_zero_and_fall_back_to_defaults() {
    with_env_vars(
        &[
            ("RESTREAM_HTTP_PORT", "0"),
            ("RESTREAM_RTMP_PORT", "0"),
            ("RESTREAM_SRT_PORT", "0"),
        ],
        || {
            let ports = ServerPorts::from_env();
            assert_eq!(ports.http, 3030);
            assert_eq!(ports.rtmp, 1935);
            assert_eq!(ports.srt, 10080);
        },
    );
}

#[test]
fn http_bind_addr_defaults_to_loopback_and_can_be_overridden() {
    with_env_overlay(&[], &["RESTREAM_HTTP_BIND_ADDR"], || {
        assert_eq!(AppConfig::from_env().http_bind_addr, "127.0.0.1");
    });
    with_env_vars(&[("RESTREAM_HTTP_BIND_ADDR", "0.0.0.0")], || {
        assert_eq!(AppConfig::from_env().http_bind_addr, "0.0.0.0");
    });
}

#[test]
fn runtime_layout_is_owned_and_each_path_can_be_overridden() {
    with_env_overlay(
        &[],
        &["RESTREAM_DB_PATH", "RESTREAM_MEDIA_DIR", "RESTREAM_LOG_DIR"],
        || {
            let config = AppConfig::from_env();
            assert_eq!(config.db_path, ".restream/data/restream.db");
            assert_eq!(config.media_dir, DEFAULT_MEDIA_DIR);
            assert_eq!(config.log_dir, ".restream/logs");
        },
    );

    with_env_vars(
        &[
            ("RESTREAM_DB_PATH", "/state/custom.db"),
            ("RESTREAM_MEDIA_DIR", "/assets"),
            ("RESTREAM_LOG_DIR", "/var/log/restream"),
        ],
        || {
            let config = AppConfig::from_env();
            assert_eq!(config.db_path, "/state/custom.db");
            assert_eq!(config.media_dir, "/assets");
            assert_eq!(config.log_dir, "/var/log/restream");
        },
    );
}

#[test]
fn secure_session_cookie_flag_is_opt_in() {
    with_env_overlay(&[], &["RESTREAM_SECURE_SESSION_COOKIES"], || {
        assert!(!AppConfig::from_env().secure_session_cookies);
    });
    with_env_vars(&[("RESTREAM_SECURE_SESSION_COOKIES", "true")], || {
        assert!(AppConfig::from_env().secure_session_cookies);
    });
}

#[test]
fn external_ffmpeg_derivation_keeps_live_dependency_graph_moving() {
    assert_eq!(
        derive_external_ffmpeg_permits(6, 2, 2, usize::MAX),
        EXTERNAL_FFMPEG_LIVE_LIVENESS_FLOOR
    );
    assert_eq!(
        derive_external_ffmpeg_permits(2, 1, 2, usize::MAX),
        EXTERNAL_FFMPEG_LIVE_LIVENESS_FLOOR
    );
    assert_eq!(derive_external_ffmpeg_permits(64, 2, 2, usize::MAX), 31);
    assert_eq!(derive_external_ffmpeg_permits(6, 2, 2, 3), 3);
}

#[test]
fn external_ffmpeg_env_override_and_hard_cap_are_preserved() {
    with_env_overlay(
        &[("RESTREAM_EXTERNAL_FFMPEG_PERMITS", "2")],
        &[
            "RESTREAM_EXTERNAL_FFMPEG_MAX_CHILDREN",
            "RESTREAM_EXTERNAL_FFMPEG_CPU_RESERVE",
            "RESTREAM_EXTERNAL_FFMPEG_CPU_PER_CHILD",
        ],
        || {
            assert_eq!(AppConfig::from_env().external_ffmpeg_permits, 2);
        },
    );

    with_env_overlay(
        &[("RESTREAM_EXTERNAL_FFMPEG_MAX_CHILDREN", "3")],
        &[
            "RESTREAM_EXTERNAL_FFMPEG_PERMITS",
            "RESTREAM_EXTERNAL_FFMPEG_CPU_RESERVE",
            "RESTREAM_EXTERNAL_FFMPEG_CPU_PER_CHILD",
        ],
        || {
            assert_eq!(AppConfig::from_env().external_ffmpeg_permits, 3);
        },
    );
}

#[test]
fn rtmp_preauth_limits_are_loaded_and_clamped() {
    with_env_vars(
        &[
            ("RESTREAM_RTMP_MAX_CONNECTIONS", "0"),
            ("RESTREAM_RTMP_HANDSHAKE_TIMEOUT_MS", "10"),
            ("RESTREAM_RTMP_PREAUTH_BUFFER_BYTES", "1024"),
            ("RESTREAM_RTMP_STREAM_BUFFER_BYTES", "65536"),
            ("RESTREAM_RTMP_EGRESS_CHUNK_SIZE", "32"),
            ("RESTREAM_SRT_EGRESS_MUXER_MAX_OUTPUTS_PER_SHARD", "12000"),
            ("RESTREAM_SRT_EGRESS_MUXER_MAX_SHARDS", "128"),
        ],
        || {
            let config = AppConfig::from_env();
            assert_eq!(config.rtmp_max_connections, 1);
            assert_eq!(config.rtmp_handshake_timeout_ms, 100);
            assert_eq!(config.rtmp_preauth_buffer_bytes, 16 * 1024);
            assert_eq!(config.rtmp_stream_buffer_bytes, 128 * 1024);
            assert_eq!(config.rtmp_egress_chunk_size, 128);
            assert_eq!(config.srt_egress_muxer_max_outputs_per_shard, 10_000);
            assert_eq!(config.srt_egress_muxer_max_shards, 64);
        },
    );
}

#[test]
fn runtime_tuning_is_loaded_by_config_module() {
    with_env_vars(
        &[
            ("RESTREAM_NOFILE_LIMIT", "1234"),
            ("RESTREAM_RECONCILE_INTERVAL_MS", "5"),
            ("RESTREAM_OUTPUT_RETRY_BASE_MS", "0"),
            ("RESTREAM_HLS_IDLE_TIMEOUT_MS", "90000"),
        ],
        || {
            let tuning = RuntimeTuning::from_env();
            assert_eq!(tuning.nofile_limit, 1234);
            assert_eq!(tuning.reconciler_interval_ms, 100);
            assert_eq!(tuning.output_retry_base_ms, 1);
            assert_eq!(tuning.hls_idle_timeout_ms, 90000);
        },
    );
}

#[test]
fn egress_fabric_config_defaults_disabled_and_builds_runtime_values() {
    with_env_overlay(
        &[],
        &[
            "RESTREAM_EGRESS_FABRIC",
            "RESTREAM_EGRESS_SHARDS",
            "RESTREAM_EGRESS_COMMAND_CAPACITY",
            "RESTREAM_EGRESS_COMMAND_BATCH",
            "RESTREAM_EGRESS_READY_BATCH",
            "RESTREAM_EGRESS_TIMER_BATCH",
            "RESTREAM_EGRESS_IDLE_WAIT_MS",
            "RESTREAM_EGRESS_SRT_POLLER_MAX_EVENTS",
            "RESTREAM_EGRESS_VISIT_MAX_UNITS",
            "RESTREAM_EGRESS_VISIT_MAX_BYTES",
            "RESTREAM_EGRESS_VISIT_MAX_US",
            "RESTREAM_EGRESS_MAX_PENDING_BYTES",
        ],
        || {
            let fabric = EgressFabricConfig::from_env();
            assert_eq!(fabric, EgressFabricConfig::default());
            assert_eq!(fabric.rollout, EgressRolloutMode::All);
            assert!(fabric.rollout.routes_srt());
            assert!(fabric.rollout.routes_rtmp());
            assert_eq!(
                fabric.shard_count().get(),
                default_egress_fabric_shards(crate::system_sampling::effective_cpu_count())
            );
            assert_eq!(fabric.shard_config().command_channel_capacity().get(), 1024);
            let budget = fabric.work_budget();
            assert_eq!(budget.max_units, 32);
            assert_eq!(budget.max_bytes, 256 * 1024);
        },
    );
}

#[test]
fn egress_fabric_config_loads_and_clamps_env() {
    with_env_vars(
        &[
            ("RESTREAM_EGRESS_FABRIC", "true"),
            ("RESTREAM_EGRESS_SHARDS", "0"),
            ("RESTREAM_EGRESS_COMMAND_CAPACITY", "0"),
            ("RESTREAM_EGRESS_COMMAND_BATCH", "0"),
            ("RESTREAM_EGRESS_READY_BATCH", "0"),
            ("RESTREAM_EGRESS_TIMER_BATCH", "0"),
            ("RESTREAM_EGRESS_IDLE_WAIT_MS", "0"),
            ("RESTREAM_EGRESS_SRT_POLLER_MAX_EVENTS", "0"),
            ("RESTREAM_EGRESS_VISIT_MAX_UNITS", "0"),
            ("RESTREAM_EGRESS_VISIT_MAX_BYTES", "1"),
            ("RESTREAM_EGRESS_VISIT_MAX_US", "0"),
            ("RESTREAM_EGRESS_MAX_PENDING_BYTES", "999999999"),
        ],
        || {
            let fabric = EgressFabricConfig::from_env();
            assert_eq!(fabric.rollout, EgressRolloutMode::Srt);
            assert_eq!(fabric.shards, 1);
            assert_eq!(fabric.command_channel_capacity, 1);
            assert_eq!(fabric.command_batch_budget, 1);
            assert_eq!(fabric.readiness_batch_budget, 1);
            assert_eq!(fabric.timer_batch_budget, 1);
            assert_eq!(fabric.idle_wait_ms, 1);
            assert_eq!(fabric.srt_poller_max_events, 1);
            assert_eq!(fabric.visit_max_units, 1);
            assert_eq!(fabric.visit_max_bytes, 188);
            assert_eq!(fabric.visit_max_us, 1);
            assert_eq!(fabric.max_pending_bytes, 16 * 1024 * 1024);
        },
    );
}

#[test]
fn egress_fabric_config_validate_is_silent_for_sane_defaults() {
    assert_eq!(
        EgressFabricConfig::default().validate(6),
        Vec::<String>::new()
    );
}

#[test]
fn egress_fabric_config_validate_flags_cross_field_issues() {
    let fabric = EgressFabricConfig {
        max_pending_bytes: 100,
        visit_max_bytes: 1_000,
        shards: 32,
        drain_timeout_ms: 10,
        command_channel_capacity: 4,
        command_batch_budget: 8,
        ..EgressFabricConfig::default()
    };

    let warnings = fabric.validate(4);

    assert_eq!(warnings.len(), 4, "warnings: {warnings:#?}");
    assert!(warnings[0].contains("RESTREAM_EGRESS_MAX_PENDING_BYTES"));
    assert!(warnings[1].contains("RESTREAM_EGRESS_SHARDS"));
    assert!(warnings[2].contains("RESTREAM_EGRESS_DRAIN_TIMEOUT_MS"));
    assert!(warnings[3].contains("RESTREAM_EGRESS_COMMAND_BATCH"));
}

#[test]
fn egress_rollout_mode_parses_protocol_selection_and_legacy_booleans() {
    let cases = [
        ("off", EgressRolloutMode::Off),
        ("0", EgressRolloutMode::Off),
        ("false", EgressRolloutMode::Off),
        ("srt", EgressRolloutMode::Srt),
        ("1", EgressRolloutMode::Srt),
        ("true", EgressRolloutMode::Srt),
        ("rtmp", EgressRolloutMode::Rtmp),
        ("all", EgressRolloutMode::All),
        ("shadow-metrics", EgressRolloutMode::ShadowMetrics),
        ("SRT", EgressRolloutMode::Srt),
    ];
    for (value, expected) in cases {
        with_env_vars(&[("RESTREAM_EGRESS_FABRIC", value)], || {
            assert_eq!(EgressFabricConfig::from_env().rollout, expected, "{value}");
        });
    }
    // Unknown values fall back to the default rather than guessing.
    with_env_vars(&[("RESTREAM_EGRESS_FABRIC", "bogus")], || {
        assert_eq!(
            EgressFabricConfig::from_env().rollout,
            EgressRolloutMode::All
        );
    });
}

#[test]
fn egress_rollout_mode_routing_is_protocol_selective() {
    assert!(!EgressRolloutMode::Off.routes_srt());
    assert!(!EgressRolloutMode::Off.routes_rtmp());
    assert!(EgressRolloutMode::Srt.routes_srt());
    assert!(!EgressRolloutMode::Srt.routes_rtmp());
    assert!(!EgressRolloutMode::Rtmp.routes_srt());
    assert!(EgressRolloutMode::Rtmp.routes_rtmp());
    assert!(EgressRolloutMode::All.routes_srt());
    assert!(EgressRolloutMode::All.routes_rtmp());
    // Shadow mode is active for calculations but never owns a connection.
    assert!(EgressRolloutMode::ShadowMetrics.is_active());
    assert!(!EgressRolloutMode::ShadowMetrics.routes_srt());
    assert!(!EgressRolloutMode::ShadowMetrics.routes_rtmp());
}

#[test]
fn tokio_runtime_config_tracks_cpu_limits_and_overrides() {
    assert_eq!(default_tokio_worker_threads(1), 1);
    assert_eq!(default_tokio_worker_threads(2), 2);
    assert_eq!(default_tokio_worker_threads(6), 2);
    assert_eq!(default_tokio_worker_threads(12), 4);
    assert_eq!(default_tokio_worker_threads(64), 8);

    assert_eq!(default_egress_fabric_shards(1), 2);
    assert_eq!(default_egress_fabric_shards(2), 2);
    assert_eq!(default_egress_fabric_shards(6), 6);
    assert_eq!(default_egress_fabric_shards(12), 8);
    assert_eq!(default_egress_fabric_shards(64), 8);

    with_env_vars(
        &[
            ("RESTREAM_TOKIO_WORKER_THREADS", "3"),
            ("RESTREAM_TOKIO_MAX_BLOCKING_THREADS", "32"),
        ],
        || {
            let runtime = TokioRuntimeConfig::from_env();
            assert_eq!(runtime.worker_threads, 3);
            assert_eq!(runtime.max_blocking_threads, 32);
            assert_eq!(AppConfig::from_env().tokio_runtime, runtime);
        },
    );
}

#[test]
fn backend_policy_is_loaded_by_config_module() {
    with_env_vars(
        &[
            ("RESTREAM_INTERNAL_VIDEO_PRESETS", "true"),
            ("RESTREAM_INTERNAL_HEVC_TO_H264", "1"),
            ("RESTREAM_INTERNAL_HLS_PREVIEW", "yes"),
            ("RESTREAM_INTERNAL_AUDIO_COMPLEX", "off"),
        ],
        || {
            let policy = backend_policy_from_env();
            assert!(policy.internal_video_presets);
            assert!(policy.internal_hevc_to_h264);
            assert!(policy.internal_hls_preview);
            assert!(!policy.internal_complex_audio);
        },
    );
}

#[test]
fn legacy_global_internal_transcoder_env_does_not_enable_stage_families() {
    with_env_overlay(
        &[("RESTREAM_USE_INTERNAL_TRANSCODER", "1")],
        &[
            "RESTREAM_INTERNAL_VIDEO_PRESETS",
            "RESTREAM_INTERNAL_HEVC_TO_H264",
            "RESTREAM_INTERNAL_HLS_PREVIEW",
            "RESTREAM_INTERNAL_AUDIO_COMPLEX",
        ],
        || {
            let policy = backend_policy_from_env();
            assert_eq!(policy, BackendPolicy::default());
        },
    );
}

#[test]
fn backend_policy_does_not_use_global_internal_switch_for_all_stages() {
    with_env_overlay(
        &[("RESTREAM_USE_INTERNAL_TRANSCODER", "1")],
        &[
            "RESTREAM_INTERNAL_VIDEO_PRESETS",
            "RESTREAM_INTERNAL_HEVC_TO_H264",
            "RESTREAM_INTERNAL_HLS_PREVIEW",
            "RESTREAM_INTERNAL_AUDIO_COMPLEX",
        ],
        || {
            let policy = backend_policy_from_env();
            assert!(!policy.internal_video_presets);
            assert!(!policy.internal_hevc_to_h264);
            assert!(!policy.internal_hls_preview);
            assert!(!policy.internal_complex_audio);
        },
    );
}

#[test]
fn srt_egress_reuse_local_port_defaults_on_and_allows_override() {
    with_env_overlay(&[], &["RESTREAM_SRT_EGRESS_REUSE_LOCAL_PORT"], || {
        assert!(AppConfig::from_env().srt_egress_reuse_local_port);
    });
    with_env_vars(&[("RESTREAM_SRT_EGRESS_REUSE_LOCAL_PORT", "true")], || {
        assert!(AppConfig::from_env().srt_egress_reuse_local_port);
    });
    with_env_vars(&[("RESTREAM_SRT_EGRESS_REUSE_LOCAL_PORT", "1")], || {
        assert!(AppConfig::from_env().srt_egress_reuse_local_port);
    });
    with_env_vars(&[("RESTREAM_SRT_EGRESS_REUSE_LOCAL_PORT", "false")], || {
        assert!(!AppConfig::from_env().srt_egress_reuse_local_port);
    });
    with_env_vars(&[("RESTREAM_SRT_EGRESS_REUSE_LOCAL_PORT", "0")], || {
        assert!(!AppConfig::from_env().srt_egress_reuse_local_port);
    });
}

#[test]
fn srt_connect_timeout_defaults_and_allows_override() {
    with_env_overlay(&[], &["RESTREAM_SRT_CONNECT_TIMEOUT_MS"], || {
        assert_eq!(AppConfig::from_env().srt_connect_timeout_ms, 3_000);
    });
    with_env_vars(&[("RESTREAM_SRT_CONNECT_TIMEOUT_MS", "500")], || {
        assert_eq!(AppConfig::from_env().srt_connect_timeout_ms, 500);
    });
}

#[test]
fn initial_admin_password_is_loaded_by_config_module() {
    with_env_vars(&[("RESTREAM_INITIAL_ADMIN_PASSWORD", "dev-secret")], || {
        let config = AppConfig::from_env();
        assert_eq!(config.initial_admin_password.as_deref(), Some("dev-secret"));
    });
}

#[test]
fn rtmps_extra_trust_roots_pem_path_defaults_to_none_and_can_be_overridden() {
    assert_eq!(AppConfig::default().rtmps_extra_trust_roots_pem_path, None);
    with_env_vars(
        &[(
            "RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM",
            "/etc/restream/rtmps-trust-roots.pem",
        )],
        || {
            let config = AppConfig::from_env();
            assert_eq!(
                config.rtmps_extra_trust_roots_pem_path.as_deref(),
                Some("/etc/restream/rtmps-trust-roots.pem")
            );
        },
    );
}

#[test]
fn effective_summary_covers_runtime_knobs_without_secret_values() {
    let config = AppConfig {
        srt_passphrase: Some("super-secret".to_string()),
        initial_admin_password: Some("admin-secret".to_string()),
        ffmpeg_bin_path: Some("/usr/bin/ffmpeg".to_string()),
        ..AppConfig::default()
    };

    let summary = config.effective_summary();
    assert_eq!(summary["ports"]["http"], 3030);
    assert_eq!(summary["tuning"]["reconcilerIntervalMs"], 1000);
    assert_eq!(
        summary["tokio"]["workerThreads"],
        config.tokio_runtime.worker_threads
    );
    assert_eq!(
        summary["tokio"]["maxBlockingThreads"],
        config.tokio_runtime.max_blocking_threads
    );
    assert_eq!(summary["egressFabric"]["enabled"], true);
    assert_eq!(
        summary["egressFabric"]["shards"],
        config.egress_fabric.shards
    );
    assert_eq!(summary["paths"]["ffmpegBin"], "/usr/bin/ffmpeg");
    assert_eq!(summary["backendPolicy"]["internalHlsPreview"], false);
    assert_eq!(
        summary["ffmpeg"]["externalPermits"],
        config.external_ffmpeg_permits
    );
    assert_eq!(summary["buffers"]["ringCapacity"], 1024);
    assert_eq!(summary["srt"]["passphraseConfigured"], true);
    assert_eq!(summary["srt"]["pbkeylen"], 16);
    assert!(!summary.to_string().contains("super-secret"));
    assert!(!summary.to_string().contains("admin-secret"));
}
