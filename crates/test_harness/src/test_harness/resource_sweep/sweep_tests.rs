//! Resource-sweep unit tests (scenario needs, ports, URL retargeting).

use super::*;
use crate::core::PEER_PORT_STRIDE;

/// The multi-host rung path: only the loopback authority is replaced.
#[test]
fn srt_urls_move_onto_a_remote_peer_host() {
    let url = "srt://127.0.0.1:8891?streamid=publish:live/key&latency=200";
    assert_eq!(
        srt_url_on_host(url, "peer-a"),
        "srt://peer-a:8891?streamid=publish:live/key&latency=200"
    );
    assert_eq!(
        srt_url_on_host(url, "2001:db8::5"),
        "srt://[2001:db8::5]:8891?streamid=publish:live/key&latency=200",
        "IPv6 peer hosts are bracketed"
    );
    assert_eq!(
        srt_url_on_host(url, "[2001:db8::5]"),
        "srt://[2001:db8::5]:8891?streamid=publish:live/key&latency=200"
    );
    let already_remote = "srt://peer-b:8891?streamid=publish:live/key";
    assert_eq!(srt_url_on_host(already_remote, "peer-a"), already_remote);
}

fn test_env() -> ResourceSweepEnv {
    ResourceSweepEnv {
        work_dir: PathBuf::from("."),
        summary_json: PathBuf::from("summary.json"),
        summary_csv: PathBuf::from("summary.csv"),
        samples_jsonl: PathBuf::from("samples.jsonl"),
        restream_log: PathBuf::from("restream.log"),
        mediamtx_log: PathBuf::from("mediamtx.log"),
        mediamtx_config: PathBuf::from("mediamtx.yml"),
        restream_bin: PathBuf::from("restream"),
        restream_db_path: PathBuf::from("restream.db"),
        restream_http: 3030,
        restream_rtmp: 1935,
        restream_srt: 10080,
        mtx_rtmp: 1936,
        mtx_rtmps: 1937,
        mtx_srt: 8891,
        mtx_api: 9997,
        peer_count: 4,
        peer_mode: ResourceSweepPeer::Mediamtx,
        srt_peer_hosts: Vec::new(),
        ingest_growth_config: "h264-srt".to_string(),
        ingest_outputs_per_feed: 0,
        ingest_output_kind: super::catalog::SweepOutputKind::SrtSource,
        sample_secs: 1,
        sample_interval_ms: 1000,
        settle_secs: 1,
        ingest_counts: Vec::new(),
        egress_counts: Vec::new(),
        bitrate: "1.5M".to_string(),
        scenario_filter: None,
        lifecycle: ResourceSweepLifecycle::Continuous,
        no_cleanup: false,
        srt_crypto: HarnessSrtCrypto::plaintext(),
        rtmps_tls: None,
    }
}

#[test]
fn peer_instance_ports_offset_from_instance_zero() {
    let env = test_env();
    // Instance 0 always matches the pre-existing single-mediamtx ports.
    assert_eq!(peer_instance_ports(&env, 0), (1936, 1937, 8891, 9997));
    assert_eq!(peer_instance_ports(&env, 3), (1948, 1949, 8903, 10009));
}

#[test]
fn peer_instance_ports_never_collide_across_instances() {
    let env = test_env();
    // `mtx_rtmp` and `mtx_rtmps` are adjacent (1936/1937 by default), so a
    // one-per-instance offset gives instance 1's RTMP port the value of
    // instance 0's RTMPS port and the second listener fails to bind. This is
    // the validator `ResourceSweepEnv` runs when it is built.
    validate_peer_instance_ports(&env).expect("the default peer bands must not collide");
}

#[test]
fn peer_instance_ports_reject_a_band_whose_difference_is_a_stride_multiple() {
    let mut env = test_env();
    env.mtx_rtmps = env.mtx_rtmp + PEER_PORT_STRIDE;
    let error =
        validate_peer_instance_ports(&env).expect_err("an overlapping band must be rejected");
    assert!(error.contains("both use port"), "unexpected error: {error}");
}

#[test]
fn instance_suffixed_path_leaves_instance_zero_unchanged() {
    let path = PathBuf::from("/work/msr-mediamtx.yml");
    assert_eq!(instance_suffixed_path(&path, 0), path);
    assert_eq!(
        instance_suffixed_path(&path, 2),
        PathBuf::from("/work/msr-mediamtx-2.yml")
    );
}

#[test]
fn instance_suffixed_path_handles_extensionless_paths() {
    let path = PathBuf::from("/work/mediamtx-log");
    assert_eq!(
        instance_suffixed_path(&path, 1),
        PathBuf::from("/work/mediamtx-log-1")
    );
}

#[test]
fn outputs_spread_across_peer_instances() {
    let mut counts = [0usize; 3];
    for index in 0..300 {
        counts[peer_instance_for(&format!("resource-sweep-out-{index}"), 3)] += 1;
    }
    assert!(counts.iter().all(|count| *count > 60), "{counts:?}");
    assert_eq!(
        peer_instance_for("any", 1),
        0,
        "one peer keeps every output"
    );
}

#[test]
fn a_scenario_filter_that_selects_nothing_is_reported() {
    let filter = |names: &[&str]| names.iter().map(|name| name.to_string()).collect();
    assert!(
        config::unknown_scenarios(&filter(&["baseline-empty", "egress-growth-hevc-bridge"]))
            .is_empty()
    );
    assert_eq!(
        config::unknown_scenarios(&filter(&[
            "resource.egress-growth-source-same",
            "egress-growth-source-same",
            "ingest",
        ])),
        ["ingest", "resource.egress-growth-source-same"]
    );
}

/// Restream sends every SRT output of an egress shard from one UDP socket, so
/// the sink's parallelism comes only from the ports the harness spreads outputs
/// over, never from kernel hashing (Q-027). The capacity ramp's SRT x100 rung
/// on two sink peers must use all four Owner ports (the default two per peer),
/// each with about a quarter of the outputs.
#[test]
fn srt_outputs_spread_over_every_sink_owner_port() {
    let mut env = test_env();
    env.peer_count = 2;
    env.peer_mode = ResourceSweepPeer::Sink;
    let mut per_port = std::collections::BTreeMap::<u16, usize>::new();
    for index in 1..=100 {
        let name = format!("egress-growth-source-srt-srt-source-{index}");
        let (url, _) =
            resource_output_url(&env, sweep_configs()[0], SweepOutputKind::SrtSource, &name);
        let port = url
            .strip_prefix("srt://127.0.0.1:")
            .and_then(|rest| rest.split('?').next())
            .and_then(|port| port.parse::<u16>().ok())
            .unwrap_or_else(|| panic!("unexpected SRT output URL {url}"));
        *per_port.entry(port).or_default() += 1;
    }
    assert_eq!(
        per_port.keys().copied().collect::<Vec<_>>(),
        [8891, 8892, 8895, 8896],
        "{per_port:?}"
    );
    assert!(per_port.values().all(|&count| count <= 32), "{per_port:?}");
}
