use super::{AppConfig, DEFAULT_MEDIA_DIR, RuntimeTuning, ServerPorts, TokioRuntimeConfig};
use crate::capacity::LimitSource;
use crate::media::config::MoqIngestSetting;
use crate::media::config::env::{with_env_overlay, with_env_vars};
use crate::media::host_capacity::HostCapacity;

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
fn capacity_limits_accept_host_calibration_overrides() {
    with_env_vars(
        &[
            ("RESTREAM_CAPACITY_INGRESS_PPS", "1234"),
            ("RESTREAM_CAPACITY_EGRESS_PPS", "5678"),
            ("RESTREAM_CAPACITY_NIC_BPS", "9000000000"),
            ("RESTREAM_CAPACITY_MEMORY_BYTES", "123456789"),
            ("RESTREAM_CAPACITY_FFMPEG_STAGES", "3.5"),
            // Not positive: the prior stands, and is reported as the prior.
            ("RESTREAM_CAPACITY_DISK_BPS", "-1"),
        ],
        || {
            let config = AppConfig::from_env();
            assert_eq!(config.capacity_limits.ingress_pps, 1234.0);
            assert_eq!(config.capacity_limits.egress_pps, 5678.0);
            assert_eq!(config.capacity_limits.nic_bps, 9_000_000_000.0);
            assert_eq!(config.capacity_limits.memory_bytes, 123_456_789.0);
            assert_eq!(config.capacity_limits.ffmpeg_stages, 3.5);
            assert_eq!(config.capacity_limits.disk_bps, 500_000_000.0);
            let sources = config.capacity_sources;
            assert_eq!(sources.ingress_pps, LimitSource::Configured);
            assert_eq!(sources.ffmpeg_stages, LimitSource::Configured);
            assert_eq!(sources.disk_bps, LimitSource::Prior);
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
            ("RESTREAM_LOG_RETENTION_DAYS", "14"),
        ],
        || {
            let config = AppConfig::from_env();
            assert_eq!(config.db_path, "/state/custom.db");
            assert_eq!(config.media_dir, "/assets");
            assert_eq!(config.log_dir, "/var/log/restream");
            assert_eq!(config.log_retention_days, 14);
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
fn tokio_runtime_config_tracks_cpu_limits_and_overrides() {
    // (CPUs, CONTROL workers, MEDIA workers, egress shards per pool).
    for (cpus, tokio, media, shards) in [
        (1, 1, 1, 2),
        (2, 2, 2, 2),
        (6, 2, 4, 6),
        (12, 4, 4, 8),
        (64, 8, 4, 8),
    ] {
        let host = HostCapacity { cpus };
        assert_eq!(host.tokio_workers(), tokio, "{cpus} CPUs");
        assert_eq!(host.media_workers(), media, "{cpus} CPUs");
        assert_eq!(host.egress_shards_per_pool(), shards, "{cpus} CPUs");
    }

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
fn initial_admin_password_is_loaded_by_config_module() {
    with_env_vars(&[("RESTREAM_INITIAL_ADMIN_PASSWORD", "dev-secret")], || {
        let config = AppConfig::from_env();
        assert_eq!(config.initial_admin_password.as_deref(), Some("dev-secret"));
    });
}

#[test]
fn moq_ingest_is_off_by_default_and_its_port_follows_the_listen_address() {
    with_env_overlay(
        &[],
        &[
            "RESTREAM_MOQ_INGEST_LISTEN",
            "RESTREAM_MOQ_TLS_CERT",
            "RESTREAM_MOQ_TLS_KEY",
        ],
        || {
            let config = AppConfig::from_env();
            assert_eq!(config.engine.moq_ingest, MoqIngestSetting::Off);
            assert_eq!(config.ports.moq, None);
        },
    );
    with_env_vars(
        &[
            ("RESTREAM_MOQ_INGEST_LISTEN", "0.0.0.0:4443"),
            ("RESTREAM_MOQ_TLS_CERT", "/tls/cert.pem"),
            ("RESTREAM_MOQ_TLS_KEY", "/tls/key.pem"),
            ("RESTREAM_MOQ_MAX_CONNECTIONS", "16"),
            ("RESTREAM_MOQ_ORIGIN_MAX_VIEWERS", "8"),
        ],
        || {
            let config = AppConfig::from_env();
            let moq = config
                .engine
                .moq_ingest
                .config()
                .expect("listener configured");
            assert_eq!(moq.listen.port(), 4443);
            assert_eq!(moq.tls_cert_path, "/tls/cert.pem");
            assert_eq!(moq.max_connections, 16);
            assert_eq!(moq.origin_max_viewers, 8);
            assert_eq!(config.ports.moq, Some(4443));
            assert_eq!(config.effective_summary()["ports"]["moq"], 4443);
        },
    );
    with_env_overlay(
        &[("RESTREAM_MOQ_INGEST_LISTEN", "0.0.0.0:4443")],
        &["RESTREAM_MOQ_TLS_CERT", "RESTREAM_MOQ_TLS_KEY"],
        || {
            let config = AppConfig::from_env();
            assert!(matches!(
                config.engine.moq_ingest,
                MoqIngestSetting::Invalid(_)
            ));
            assert_eq!(config.ports.moq, None);
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
    assert_eq!(
        summary["egressFabric"]["shards"],
        config.engine.egress_fabric.shards
    );
    assert_eq!(summary["paths"]["ffmpegBin"], "/usr/bin/ffmpeg");
    assert_eq!(
        summary["ffmpeg"]["externalPermits"],
        config.engine.external_ffmpeg_permits
    );
    assert_eq!(summary["buffers"]["ringCapacity"], 1024);
    assert_eq!(summary["srt"]["passphraseConfigured"], true);
    assert_eq!(summary["srt"]["pbkeylen"], 16);
    assert!(!summary.to_string().contains("super-secret"));
    assert!(!summary.to_string().contains("admin-secret"));
}
