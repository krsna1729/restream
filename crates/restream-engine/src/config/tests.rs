use super::env::{with_env_overlay, with_env_vars};
use super::{
    EXTERNAL_FFMPEG_LIVE_LIVENESS_FLOOR, EgressFabricConfig, EgressShardProfile, EngineConfig,
    derive_external_ffmpeg_permits, effective_memory_bytes, env_optional_positive_usize,
    target_egress_fabric_shards,
};
use crate::host_capacity::HostCapacity;

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
            assert_eq!(EngineConfig::from_env().external_ffmpeg_permits, 2);
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
            assert_eq!(EngineConfig::from_env().external_ffmpeg_permits, 3);
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
            let config = EngineConfig::from_env();
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
fn egress_fabric_config_defaults_disabled_and_builds_runtime_values() {
    with_env_overlay(
        &[],
        &[
            "RESTREAM_EGRESS_SHARDS",
            "RESTREAM_EGRESS_COMMAND_CAPACITY",
            "RESTREAM_EGRESS_COMMAND_BATCH",
            "RESTREAM_EGRESS_READY_BATCH",
            "RESTREAM_EGRESS_TIMER_BATCH",
            "RESTREAM_EGRESS_IDLE_WAIT_MS",
            "RESTREAM_EGRESS_TCP_POLLER_MAX_EVENTS",
            "RESTREAM_EGRESS_VISIT_MAX_UNITS",
            "RESTREAM_EGRESS_VISIT_MAX_BYTES",
            "RESTREAM_EGRESS_VISIT_MAX_US",
            "RESTREAM_EGRESS_MAX_PENDING_BYTES",
        ],
        || {
            let fabric = EgressFabricConfig::from_env();
            assert_eq!(fabric, EgressFabricConfig::default());
            assert_eq!(fabric.shards, HostCapacity::get().egress_shards_per_pool());
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
            ("RESTREAM_EGRESS_SHARDS", "0"),
            ("RESTREAM_EGRESS_COMMAND_CAPACITY", "0"),
            ("RESTREAM_EGRESS_COMMAND_BATCH", "0"),
            ("RESTREAM_EGRESS_READY_BATCH", "0"),
            ("RESTREAM_EGRESS_TIMER_BATCH", "0"),
            ("RESTREAM_EGRESS_IDLE_WAIT_MS", "0"),
            ("RESTREAM_EGRESS_TCP_POLLER_MAX_EVENTS", "0"),
            ("RESTREAM_EGRESS_VISIT_MAX_UNITS", "0"),
            ("RESTREAM_EGRESS_VISIT_MAX_BYTES", "1"),
            ("RESTREAM_EGRESS_VISIT_MAX_US", "0"),
            ("RESTREAM_EGRESS_MAX_PENDING_BYTES", "999999999"),
        ],
        || {
            let fabric = EgressFabricConfig::from_env();
            assert_eq!(fabric.shards, 1);
            assert_eq!(fabric.command_channel_capacity, 1);
            assert_eq!(fabric.command_batch_budget, 1);
            assert_eq!(fabric.readiness_batch_budget, 1);
            assert_eq!(fabric.timer_batch_budget, 1);
            assert_eq!(fabric.idle_wait_ms, 1);
            assert_eq!(fabric.tcp_poller_max_events, 1);
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
fn srt_connect_timeout_defaults_and_allows_override() {
    with_env_overlay(&[], &["RESTREAM_SRT_CONNECT_TIMEOUT_MS"], || {
        assert_eq!(EngineConfig::from_env().srt_connect_timeout_ms, 10_000);
    });
    with_env_vars(&[("RESTREAM_SRT_CONNECT_TIMEOUT_MS", "500")], || {
        assert_eq!(EngineConfig::from_env().srt_connect_timeout_ms, 500);
    });
}

#[test]
fn env_optional_positive_usize_rejects_zero_and_garbage() {
    with_env_overlay(&[], &["RESTREAM_SRT_UDP_BUF_BYTES"], || {
        assert_eq!(
            env_optional_positive_usize("RESTREAM_SRT_UDP_BUF_BYTES"),
            None
        );
    });
    with_env_vars(&[("RESTREAM_SRT_UDP_BUF_BYTES", "0")], || {
        assert_eq!(
            env_optional_positive_usize("RESTREAM_SRT_UDP_BUF_BYTES"),
            None
        );
    });
    with_env_vars(&[("RESTREAM_SRT_UDP_BUF_BYTES", "nope")], || {
        assert_eq!(
            env_optional_positive_usize("RESTREAM_SRT_UDP_BUF_BYTES"),
            None
        );
    });
    with_env_vars(&[("RESTREAM_SRT_UDP_BUF_BYTES", "262144")], || {
        assert_eq!(
            env_optional_positive_usize("RESTREAM_SRT_UDP_BUF_BYTES"),
            Some(262144)
        );
    });
}

#[test]
fn srt_knobs_use_untrimmed_config_values_and_thread_local_overrides() {
    for name in [
        "RESTREAM_SRT_UDP_BUF_BYTES",
        "RESTREAM_SRT_RECV_BUDGET_DATAGRAMS",
    ] {
        for raw in ["", "0", "-1", "x", " 5", "5 ", " 5 "] {
            with_env_vars(&[(name, raw)], || {
                let parsed = env_optional_positive_usize(name);
                assert_eq!(parsed, None, "{name}={raw:?}");
                assert_eq!(restream_srt::knobs::resolve_recv_budget(parsed), None);
            });
        }
        with_env_vars(&[(name, "5")], || {
            let parsed = env_optional_positive_usize(name);
            assert_eq!(parsed, Some(5));
            let budget = restream_srt::knobs::resolve_recv_budget(parsed).expect("positive");
            assert_eq!(budget.max_datagrams, 5);
            assert_eq!(budget.max_rounds, 1);
        });
    }
}

#[test]
fn srt_egress_connect_concurrency_defaults_and_allows_override() {
    with_env_overlay(&[], &["RESTREAM_SRT_EGRESS_CONNECT_CONCURRENCY"], || {
        assert_eq!(EngineConfig::from_env().srt_egress_connect_concurrency, 64);
    });
    with_env_vars(&[("RESTREAM_SRT_EGRESS_CONNECT_CONCURRENCY", "8")], || {
        assert_eq!(EngineConfig::from_env().srt_egress_connect_concurrency, 8);
    });
    with_env_vars(&[("RESTREAM_SRT_EGRESS_CONNECT_CONCURRENCY", "0")], || {
        assert_eq!(
            EngineConfig::from_env().srt_egress_connect_concurrency,
            1,
            "clamped to at least 1 -- zero would permanently deadlock every SRT connect"
        );
    });
}

#[test]
fn the_memory_limit_is_the_lower_of_host_and_container() {
    const GIB: u64 = 1 << 30;
    assert_eq!(effective_memory_bytes(128 * GIB, Some(4 * GIB)), 4 * GIB);
    assert_eq!(effective_memory_bytes(16 * GIB, None), 16 * GIB);
    assert_eq!(
        effective_memory_bytes(16 * GIB, Some(u64::MAX)),
        16 * GIB,
        "an unlimited cgroup leaves the host's RAM"
    );
}

#[test]
fn srt_ingress_window_memory_is_split_across_owners_and_zero_disables() {
    // (budget, Owners after the clamp to the host's CPUs)
    let default_for = |owners: &str| {
        let mut result = None;
        with_env_overlay(
            &[("RESTREAM_SRT_INGRESS_OWNERS", owners)],
            &["RESTREAM_SRT_INGRESS_WINDOW_MEMORY_BYTES"],
            || {
                let config = EngineConfig::from_env();
                result = Some((
                    config
                        .srt_ingress_window_memory_bytes
                        .expect("the default budget is on"),
                    config.srt_ingress_owners as u64,
                ));
            },
        );
        result.unwrap()
    };
    let (one, _) = default_for("1");
    let (split, owners) = default_for("2");
    assert_eq!(split, one / owners, "each Owner checks its own share");
    with_env_vars(
        &[("RESTREAM_SRT_INGRESS_WINDOW_MEMORY_BYTES", "1000000")],
        || {
            assert_eq!(
                EngineConfig::from_env().srt_ingress_window_memory_bytes,
                Some(1_000_000)
            );
        },
    );
    with_env_vars(&[("RESTREAM_SRT_INGRESS_WINDOW_MEMORY_BYTES", "0")], || {
        assert_eq!(
            EngineConfig::from_env().srt_ingress_window_memory_bytes,
            None
        );
    });
}

#[test]
fn rtmps_extra_trust_roots_pem_path_defaults_to_none_and_can_be_overridden() {
    assert_eq!(
        EngineConfig::default().rtmps_extra_trust_roots_pem_path,
        None
    );
    with_env_vars(
        &[(
            "RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM",
            "/etc/restream/rtmps-trust-roots.pem",
        )],
        || {
            let config = EngineConfig::from_env();
            assert_eq!(
                config.rtmps_extra_trust_roots_pem_path.as_deref(),
                Some("/etc/restream/rtmps-trust-roots.pem")
            );
        },
    );
}

#[test]
fn target_egress_fabric_shards_matches_known_cases() {
    use EgressShardProfile::{OutputCount, SrtOutputCount};

    // --- OutputCount (RTMP/sink/pipeline shape) ---

    // Zero/low output counts always floor to 1 shard, regardless of how
    // many CPUs are available -- there is nothing to shard yet.
    assert_eq!(target_egress_fabric_shards(OutputCount, 0, 8), 1);
    assert_eq!(target_egress_fabric_shards(OutputCount, 1, 8), 1);

    // A handful of outputs on an 8-core host stays well under the CPU
    // ceiling -- this is the exact gap task #11 didn't close: fewer
    // outputs than one shard's OUTPUTS_PER_SHARD budget should not pay
    // for `egress_shards_per_pool() == 8` dedicated shard threads.
    assert_eq!(target_egress_fabric_shards(OutputCount, 38, 8), 1);

    // Right at and just past the CPU ceiling's output budget
    // (egress_shards_per_pool() == 8, OUTPUTS_PER_SHARD == 128).
    assert_eq!(target_egress_fabric_shards(OutputCount, 8 * 128, 8), 8);
    assert_eq!(target_egress_fabric_shards(OutputCount, 8 * 128 + 1, 8), 8);

    // Never exceeds the CPU-derived ceiling no matter how many outputs.
    assert_eq!(target_egress_fabric_shards(OutputCount, 1_000_000, 8), 8);

    // A constrained 1-CPU host still gets the same
    // egress_shards_per_pool() == 2 ceiling once outputs justify it.
    assert_eq!(target_egress_fabric_shards(OutputCount, 0, 1), 1);
    assert_eq!(target_egress_fabric_shards(OutputCount, 1_000_000, 1), 2);

    // SRT starts with output-count sizing, not a full CPU-sized pool per feed.
    assert_eq!(target_egress_fabric_shards(SrtOutputCount, 0, 8), 1);
    assert_eq!(target_egress_fabric_shards(SrtOutputCount, 1, 8), 1);
    assert_eq!(target_egress_fabric_shards(SrtOutputCount, 64, 8), 1);
    assert_eq!(target_egress_fabric_shards(SrtOutputCount, 65, 8), 2);
    assert_eq!(target_egress_fabric_shards(SrtOutputCount, 129, 8), 3);
    assert_eq!(target_egress_fabric_shards(SrtOutputCount, 1_000_000, 8), 8);
}

proptest::proptest! {
    /// The output-count-aware target never leaves the CPU-derived range:
    /// always at least one shard, never more than
    /// `HostCapacity::egress_shards_per_pool` would allow.
    #[test]
    fn target_egress_fabric_shards_stays_within_cpu_bounds(
        outputs in 0usize..100_000,
        cpus in 1usize..256,
    ) {
        for profile in [EgressShardProfile::OutputCount, EgressShardProfile::SrtOutputCount] {
            let target = target_egress_fabric_shards(profile, outputs, cpus);
            let ceiling = HostCapacity { cpus }.egress_shards_per_pool();
            proptest::prop_assert!(target >= 1);
            proptest::prop_assert!(target <= ceiling);
        }
    }

    /// More outputs never demands *fewer* shards, for a fixed CPU count --
    /// the formula must not be able to shrink the target out from under a
    /// growing pipeline.
    #[test]
    fn target_egress_fabric_shards_is_monotonic_in_outputs(
        cpus in 1usize..256,
        smaller in 0usize..50_000,
        delta in 0usize..50_000,
    ) {
        let larger = smaller + delta;
        for profile in [EgressShardProfile::OutputCount, EgressShardProfile::SrtOutputCount] {
            let target_small = target_egress_fabric_shards(profile, smaller, cpus);
            let target_large = target_egress_fabric_shards(profile, larger, cpus);
            proptest::prop_assert!(target_large >= target_small);
        }
    }

}
