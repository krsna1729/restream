//! Resource-sweep unit tests (scenario needs, ports, URL retargeting).

use super::*;

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
        backend_policy_env: Vec::new(),
        rtmps_tls: None,
    }
}

#[test]
fn peer_instance_ports_offset_from_instance_zero() {
    let env = test_env();
    // Instance 0 always matches the pre-existing single-mediamtx ports.
    assert_eq!(peer_instance_ports(&env, 0), (1936, 1937, 8891, 9997));
    assert_eq!(peer_instance_ports(&env, 3), (1939, 1940, 8894, 10000));
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
