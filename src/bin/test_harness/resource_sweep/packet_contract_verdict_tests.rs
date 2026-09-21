//! Verdict and baseline-eligibility tests for the packet-rate contract.

use super::packet_contract::*;
use super::packet_contract_peers::{PeerReading, PeerState, PeerStateConfig};
use super::packet_contract_run::{cpu_masks_disjoint, parse_cpu_mask};
use super::packet_contract_summary::{baseline_eligibility, rung_summary};
use super::packet_contract_tests::{
    SAMPLER_TEST_LOCK, clean_sample, sample_with, system_with_owners,
};
use super::packet_contract_verdict::sample_validity;
use crate::resource_sweep::ResourceScenarioMeta;
use serde_json::{Value, json};

/// Per-sample delivery faults: a stall or connection churn is immediate, while
/// the ±5% workload band is judged over the whole common window (next test).
#[test]
fn remote_peer_stalls_and_churn_fail_immediately() {
    let peer = |payload: Value, accepted: f64, closed: f64| {
        json!([{
            "host": "peer-a", "runId": "run-1", "runIdChanged": false,
            "expectedOutputs": 100,
            "payloadBytesPerSec": payload,
            "acceptedPerSec": accepted, "closedPerSec": closed,
            "udpRcvbufErrorsPerSec": 0.0, "udpSndbufErrorsPerSec": 0.0,
            "udpInErrorsPerSec": 0.0, "nicRxDroppedPerSec": 0.0, "nicTxDroppedPerSec": 0.0,
            "error": Value::Null,
        }])
    };
    let remote = |peers: Value| sample_with(json!({"expectedPeers": 1, "peers": peers}));

    assert_eq!(
        sample_validity(&remote(peer(json!(100_000_000.0), 0.0, 0.0)), 100)
            .unwrap()
            .0,
        "healthy"
    );

    // A stalled peer delivers nothing: immediate contamination.
    let (status, reasons) = sample_validity(&remote(peer(json!(0.0), 0.0, 0.0)), 100).unwrap();
    assert_eq!(status, "contaminated", "{reasons:?}");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("peer peer-a delivered no payload")),
        "{reasons:?}"
    );

    // A peer whose delivered-payload sensor is unreadable is not a pass.
    let (status, reasons) = sample_validity(&remote(peer(Value::Null, 0.0, 0.0)), 100).unwrap();
    assert_ne!(status, "healthy");
    assert!(
        reasons
            .iter()
            .any(|reason| reason.contains("delivered payload rate is not observable")),
        "{reasons:?}"
    );

    // Connections closing or re-accepting inside the rated window mean the
    // steady state was not steady.
    for (accepted, closed) in [(1.0, 0.0), (0.0, 1.0)] {
        let (status, reasons) =
            sample_validity(&remote(peer(json!(100_000_000.0), accepted, closed)), 100).unwrap();
        assert_eq!(status, "invalid", "{reasons:?}");
        assert!(
            reasons
                .iter()
                .any(|reason| reason.contains("connection churn during the rated window")),
            "{reasons:?}"
        );
    }
}

/// Workload conformance is judged over the whole common rated window, because
/// the 8 Mbps fixture is VBR and its ±5% contract is a whole-span average: a
/// conforming stream may sit outside the band for one second.
#[test]
fn workload_conformance_averages_over_the_common_window() {
    let run = json!({
        "gitSha": "abc123", "gitDirty": false, "bitrateLabel": "8M",
        "peerMode": "sink", "lifecycle": "isolated",
    });
    // Each peer sample carries the payload delta over the peer's own observed
    // interval; the window integration uses those, not the host's interval.
    let sample = |seconds: u64, payload: f64| {
        let mut sample = clean_sample();
        sample["intervalSecs"] = json!(1.0);
        sample["ratedSecs"] = json!(seconds);
        sample["expectedPeers"] = json!(1);
        sample["peers"] = json!([{
            "host": "peer-a", "runId": "run-1", "runIdChanged": false,
            "expectedOutputs": 100,
            "payloadBytesPerSec": payload,
            "payloadBytesDelta": (payload * 1.0) as u64,
            "intervalSecs": 1.0,
            "acceptedPerSec": 0.0, "closedPerSec": 0.0,
            "udpRcvbufErrorsPerSec": 0.0, "udpSndbufErrorsPerSec": 0.0,
            "udpInErrorsPerSec": 0.0, "nicRxDroppedPerSec": 0.0, "nicTxDroppedPerSec": 0.0,
            "error": Value::Null,
        }]);
        sample
    };
    let summarize = |payloads: &[f64]| {
        let samples: Vec<Value> = payloads
            .iter()
            .enumerate()
            .map(|(index, payload)| sample(index as u64 + 1, *payload))
            .collect();
        let refs: Vec<&Value> = samples.iter().collect();
        rung_summary("egress-growth-source-srt", 100, refs.len(), &run, &refs)
    };

    // Steady delivery: no workload reason.
    let steady = summarize(&[100_000_000.0; 12]);
    assert_eq!(steady["validity"]["status"], "healthy", "{steady}");
    assert!(
        !steady["validity"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or("")
                .contains("over the common window")),
        "{steady}"
    );

    // VBR: half the seconds below the band, half above, averaging to the
    // contract rate. Per-second checks would fail this; the window must not.
    let vbr = summarize(&[
        90_000_000.0,
        90_000_000.0,
        90_000_000.0,
        90_000_000.0,
        90_000_000.0,
        90_000_000.0,
        110_000_000.0,
        110_000_000.0,
        110_000_000.0,
        110_000_000.0,
        110_000_000.0,
        110_000_000.0,
    ]);
    assert_eq!(vbr["validity"]["status"], "healthy", "{vbr}");

    // Sustained under-delivery: the window average is outside the band.
    let short = summarize(&[80_000_000.0; 12]);
    assert_eq!(
        short["validity"]["status"], "contaminated",
        "validity: {}",
        short["validity"]
    );
    assert!(
        short["validity"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or("")
                .contains("expected at the 8 Mbps workload")),
        "{short}"
    );
}

/// The common rated-window barrier is stamped when the local snapshot arrives,
/// after the peer polls have returned. If the barrier were stamped before the
/// polls (or if a pre-fetched snapshot were passed in), peer-poll latency would
/// inflate `commonRatedWindowSecs` and could push a sub-10s rung over the
/// contract minimum.
#[tokio::test]
async fn peer_poll_latency_cannot_inflate_the_common_window() {
    let _guard = SAMPLER_TEST_LOCK.lock().await;
    let work_dir = std::env::temp_dir().join(format!(
        "packet-contract-prime-order-{}",
        std::process::id()
    ));
    std::fs::create_dir_all(&work_dir).unwrap();
    let peer_config = PeerStateConfig {
        hosts: vec!["peer-a".to_string()],
        port: 9,
        expected_outputs_per_host: vec![100],
    };
    begin(
        &work_dir,
        RunMetadata {
            git_sha: Some("abc123".to_string()),
            git_dirty: Some(false),
            build: None,
            lifecycle: "isolated".to_string(),
            topology_kind: "loopback".to_string(),
            topology_netns: None,
            restream_binary: "restream".to_string(),
            restream_bin_explicit: false,
            bitrate_label: "8M".to_string(),
            peer_mode: "sink".to_string(),
            peer_targets: vec!["peer-a".to_string()],
            peer_state: Some(peer_config),
            sample_secs: 10,
            settle_secs: 10,
            sample_interval_ms: 1000,
            egress_counts: vec![100],
            scenario_filter: vec!["egress-growth-source-srt".to_string()],
        },
    );
    let meta = ResourceScenarioMeta {
        scenario: "egress-growth-source-srt",
        label: "100-per-group".to_string(),
        pipelines: 1,
        outputs: 100,
        ingest_types: "h264-srt".to_string(),
        egress_mix: "srt-source".to_string(),
        transcode: "no",
    };
    let system = {
        let mut system = system_with_owners(json!([{
            "present": true, "txPackets": 0, "txCompletedOk": 0, "rxPackets": 0,
            "serviceVisits": 0, "serviceActions": 0, "maintenanceActions": 0,
            "txFailedSends": 0, "txExhaustions": 0, "serviceBudgetExhausted": 0,
            "rxRingDropped": 0, "rxTruncated": 0,
            "txClass": {"dataFirst": 0, "dataRetransmit": 0},
        }]));
        system["capacity"] = json!({"activeLeaves": 100});
        system
    };

    // A peer poll that takes 60 ms, then the local snapshot.
    let slow_poll = |config: PeerStateConfig| async move {
        tokio::time::sleep(std::time::Duration::from_millis(60)).await;
        vec![PeerReading {
            host: config.hosts[0].clone(),
            observed_at: std::time::Instant::now(),
            state: Ok(PeerState {
                run_id: "run-1".to_string(),
                accepted: 0,
                closed: 0,
                discarded_bytes: 0,
                udp_in_errors: Some(0),
                udp_rcvbuf_errors: Some(0),
                udp_sndbuf_errors: Some(0),
                nic_rx_dropped: Some(0),
                nic_tx_dropped: Some(0),
                cpus_allowed: Some("3-5".to_string()),
            }),
        }]
    };
    prime_with(|| async { Ok(system.clone()) }, &meta, slow_poll)
        .await
        .unwrap();

    // Record immediately: if the barrier predated the poll, this sample would
    // already report ~60 ms of rated window.
    record_at(&system, 0.06, 0.0, &meta, std::time::Instant::now(), None)
        .await
        .unwrap();
    let summary_path = finish(&work_dir).unwrap().expect("summary written");
    let summary: Value = serde_json::from_slice(&std::fs::read(&summary_path).unwrap()).unwrap();
    let rated = summary["samples"][0]["ratedSecs"].as_f64().unwrap();
    assert!(
        rated < 0.03,
        "the barrier must be stamped after the peer polls, not before: ratedSecs={rated}"
    );
    let _ = std::fs::remove_dir_all(&work_dir);
}

/// Baseline eligibility is a separate gate from runtime validity: it requires
/// the canonical run shape, a clean tree, and bench binaries built from that
/// same tree — so an accidental promotion is mechanically impossible.
#[test]
fn baseline_eligibility_requires_the_canonical_run_shape() {
    let canonical_run = || {
        json!({
            "gitSha": "abc123",
            "gitDirty": false,
            "buildProvenance": {"gitSha": "abc123", "gitDirty": false, "builtAt": "2026-09-21T00:00:00Z"},
            "lifecycle": "isolated",
            "peerMode": "sink",
            "restreamBinExplicit": false,
            "topologyKind": "loopback",
            "bitrateLabel": "8M",
            "egressCounts": [100],
            "scenarioFilter": ["egress-growth-source-srt"],
            "settleSecs": 10,
            "peerStateEndpoint": {"hosts": ["peer-a"], "port": 9997},
        })
    };
    let canonical_workload = || json!({"ingestTypes": "h264-srt", "egressMix": "srt-source", "transcode": "no", "configuredOutputs": 100});
    let partitioned = json!({
        "kind": "netns-veth", "sameHost": true, "cpuPartitioned": true,
        "restreamCpusAllowed": "0-2", "peerCpusAllowed": {"10.53.0.2": "3-5"},
    });
    let eligible = baseline_eligibility(
        &canonical_run(),
        "egress-growth-source-srt",
        100,
        12.0,
        "healthy",
        8,
        8,
        &canonical_workload(),
        &partitioned,
    );
    assert_eq!(eligible["eligible"], true, "{eligible}");

    // Each case starts from the canonical run: patches must not accumulate, or
    // one case's damage would mask the next case's reason.
    let patch_case = |run_patch: Value, workload_patch: Value| {
        let mut run = canonical_run();
        let mut workload = canonical_workload();
        if let (Value::Object(base), Value::Object(patch)) = (&mut run, run_patch) {
            for (key, value) in patch {
                base.insert(key, value);
            }
        }
        if let (Value::Object(base), Value::Object(patch)) = (&mut workload, workload_patch) {
            for (key, value) in patch {
                base.insert(key, value);
            }
        }
        (run, workload)
    };

    for (run_patch, workload_patch, scenario, outputs, window, status, needle) in [
        (
            json!({"gitDirty": true}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "dirty",
        ),
        (
            json!({"gitSha": Value::Null}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "no git SHA",
        ),
        (
            json!({"buildProvenance": Value::Null}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "no bench build provenance stamp",
        ),
        (
            json!({"buildProvenance": {"gitSha": "deadbeef", "gitDirty": false}}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "not the recorded HEAD",
        ),
        (
            json!({"buildProvenance": {"gitSha": "abc123", "gitDirty": true}}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "built from a dirty tree",
        ),
        (
            json!({"lifecycle": "cumulative"}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "not the isolated rung lifecycle",
        ),
        (
            json!({"peerMode": "mediamtx"}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "not the harness sink peer",
        ),
        (
            json!({"egressCounts": [100, 300]}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "not exactly [100]",
        ),
        (
            json!({"scenarioFilter": Value::Null}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "not exactly the canonical SRT fanout",
        ),
        (
            json!({"settleSecs": 4}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "below the 10s minimum",
        ),
        (
            json!({"peerStateEndpoint": Value::Null}),
            json!({}),
            "egress-growth-source-srt",
            300,
            12.0,
            "healthy",
            "need non-loopback sink peers",
        ),
        (
            json!({"bitrateLabel": "4M"}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "not 8M",
        ),
        (
            json!({}),
            json!({"transcode": "yes"}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "transcode",
        ),
        (
            json!({}),
            json!({"ingestTypes": "h265-srt"}),
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            "ingestTypes",
        ),
        (
            json!({}),
            json!({}),
            "egress-growth-transcode-mixed",
            100,
            12.0,
            "healthy",
            "not the canonical SRT fanout",
        ),
        (
            json!({}),
            json!({}),
            "egress-growth-source-srt",
            150,
            12.0,
            "healthy",
            "not a ladder rung",
        ),
        (
            json!({}),
            json!({}),
            "egress-growth-source-srt",
            100,
            4.0,
            "healthy",
            "shorter than 10s",
        ),
        (
            json!({}),
            json!({}),
            "egress-growth-source-srt",
            100,
            12.0,
            "contaminated",
            "runtime validity is contaminated",
        ),
    ] {
        let (case_run, case_workload) = patch_case(run_patch, workload_patch);
        let eligibility = baseline_eligibility(
            &case_run,
            scenario,
            outputs,
            window,
            status,
            8,
            8,
            &case_workload,
            &partitioned,
        );
        assert_eq!(eligibility["eligible"], false, "{needle}: {eligibility}");
        assert!(
            eligibility["reasons"]
                .as_array()
                .unwrap()
                .iter()
                .any(|reason| reason.as_str().unwrap_or("").contains(needle)),
            "{needle}: {eligibility}"
        );
    }

    // A rung with unrated samples must never promote, even when the wall-clock
    // window is long enough and nothing else is wrong.
    let unrated = baseline_eligibility(
        &canonical_run(),
        "egress-growth-source-srt",
        100,
        12.0,
        "no-rated-samples",
        0,
        8,
        &canonical_workload(),
        &partitioned,
    );
    assert_eq!(unrated["eligible"], false, "{unrated}");
    assert!(
        unrated["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or("")
                .contains("0 of 8 samples are rated")),
        "{unrated}"
    );
    let partially_rated = baseline_eligibility(
        &canonical_run(),
        "egress-growth-source-srt",
        100,
        12.0,
        "healthy",
        7,
        8,
        &canonical_workload(),
        &partitioned,
    );
    assert_eq!(partially_rated["eligible"], false, "{partially_rated}");
    assert!(
        partially_rated["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or("")
                .contains("7 of 8 samples are rated")),
        "{partially_rated}"
    );

    // Remote peers satisfy the 300-output rule; loopback does not.
    let remote_300 = baseline_eligibility(
        &json!({
            "gitSha": "abc123", "gitDirty": false,
            "buildProvenance": {"gitSha": "abc123", "gitDirty": false},
            "lifecycle": "isolated", "peerMode": "sink", "bitrateLabel": "8M",
            "egressCounts": [300], "scenarioFilter": ["egress-growth-source-srt"],
            "settleSecs": 10, "restreamBinExplicit": false,
            "topologyKind": "netns-veth",
            "peerStateEndpoint": {"hosts": ["peer-a", "peer-b"], "port": 9997},
        }),
        "egress-growth-source-srt",
        300,
        12.0,
        "healthy",
        8,
        8,
        &json!({"ingestTypes": "h264-srt", "egressMix": "srt-source", "transcode": "no"}),
        &json!({"kind": "remote", "sameHost": false, "cpuPartitioned": null}),
    );
    assert_eq!(remote_300["eligible"], true, "{remote_300}");
}

/// Peer delivery is integrated over the peer's *own* observed intervals, not the
/// measuring host's sample interval: the two can drift, and the frozen contract
/// must not depend on them agreeing. Coverage of the common window is required.
#[test]
fn peer_delivery_uses_the_peers_own_intervals() {
    let run = json!({
        "gitSha": "abc123", "gitDirty": false, "bitrateLabel": "8M",
        "peerMode": "sink", "lifecycle": "isolated", "restreamBinExplicit": false,
    });
    // Each peer observation covers 2 s of wall clock while the host samples
    // every 1 s, so `rate × host interval` would be half the true delivery.
    let sample = |seconds: u64, peer_interval: f64, payload_delta: u64| {
        let mut sample = clean_sample();
        sample["intervalSecs"] = json!(1.0);
        sample["ratedSecs"] = json!(seconds);
        sample["expectedPeers"] = json!(1);
        sample["peers"] = json!([{
            "host": "peer-a", "runId": "run-1", "runIdChanged": false,
            "expectedOutputs": 100,
            "payloadBytesPerSec": payload_delta as f64 / peer_interval,
            "payloadBytesDelta": payload_delta,
            "intervalSecs": peer_interval,
            "acceptedPerSec": 0.0, "closedPerSec": 0.0,
            "udpRcvbufErrorsPerSec": 0.0, "udpSndbufErrorsPerSec": 0.0,
            "udpInErrorsPerSec": 0.0, "nicRxDroppedPerSec": 0.0, "nicTxDroppedPerSec": 0.0,
            "error": Value::Null,
        }]);
        sample
    };
    let summarize = |samples: Vec<Value>| {
        let refs: Vec<&Value> = samples.iter().collect();
        rung_summary("egress-growth-source-srt", 100, refs.len(), &run, &refs)
    };

    // Six peer observations of 2 s each, each carrying 200 MB of payload:
    // 1.2 GB over 12 observed seconds = exactly the 8 Mbps workload.
    let samples: Vec<Value> = (1..=6)
        .map(|index| sample(index * 2, 2.0, 200_000_000))
        .collect();
    let rung = summarize(samples);
    let delivery = &rung["peerDelivery"]["peer-a"];
    assert_eq!(delivery["deliveredBytes"], 1_200_000_000_u64);
    assert_eq!(delivery["observedSecs"], 12.0);
    assert_eq!(delivery["expectedBytesPerSec"], 100_000_000.0);
    assert_eq!(rung["validity"]["status"], "healthy", "{rung}");

    // A peer that only observed 4 s of the window cannot be judged on it.
    let samples: Vec<Value> = (1..=2)
        .map(|index| sample(index * 2, 2.0, 200_000_000))
        .collect();
    let rung = summarize(samples);
    assert_eq!(rung["validity"]["status"], "contaminated", "{rung}");
    assert!(
        rung["validity"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or("")
                .contains("observed only 4.0s of the common window")),
        "{rung}"
    );

    // Under-delivery over the peer's own window is still caught.
    let samples: Vec<Value> = (1..=6)
        .map(|index| sample(index * 2, 2.0, 160_000_000))
        .collect();
    let rung = summarize(samples);
    assert_eq!(rung["validity"]["status"], "contaminated", "{rung}");
    assert!(
        rung["validity"]["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or("")
                .contains("delivered 960000000 B over 12.0s observed")),
        "{rung}"
    );
}

/// A same-host lane must prove CPU partitioning: masks that overlap, or no
/// observed masks at all, are not baseline-eligible.
#[test]
fn same_host_topologies_require_disjoint_cpu_masks() {
    let run = json!({
        "gitSha": "abc123", "gitDirty": false,
        "buildProvenance": {"gitSha": "abc123", "gitDirty": false},
        "lifecycle": "isolated", "peerMode": "sink", "bitrateLabel": "8M",
        "egressCounts": [100], "scenarioFilter": ["egress-growth-source-srt"],
        "settleSecs": 10, "restreamBinExplicit": false,
        "topologyKind": "netns-veth",
        "peerStateEndpoint": {"hosts": ["10.53.0.2"], "port": 9997},
    });
    let workload = json!({"ingestTypes": "h264-srt", "egressMix": "srt-source", "transcode": "no"});
    let eligibility = |topology: Value| {
        baseline_eligibility(
            &run,
            "egress-growth-source-srt",
            100,
            12.0,
            "healthy",
            8,
            8,
            &workload,
            &topology,
        )
    };

    let partitioned = eligibility(json!({
        "kind": "netns-veth", "sameHost": true, "cpuPartitioned": true,
        "restreamCpusAllowed": "0-2", "peerCpusAllowed": {"10.53.0.2": "3-5"},
    }));
    assert_eq!(partitioned["eligible"], true, "{partitioned}");

    let overlapping = eligibility(json!({
        "kind": "netns-veth", "sameHost": true, "cpuPartitioned": false,
        "restreamCpusAllowed": "0-2", "peerCpusAllowed": {"10.53.0.2": "2-5"},
    }));
    assert_eq!(overlapping["eligible"], false, "{overlapping}");
    assert!(
        overlapping["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or("")
                .contains("without disjoint CPU masks")),
        "{overlapping}"
    );

    let unobserved = eligibility(json!({
        "kind": "netns-veth", "sameHost": true, "cpuPartitioned": null,
        "restreamCpusAllowed": null, "peerCpusAllowed": {},
    }));
    assert_eq!(unobserved["eligible"], false, "{unobserved}");
    assert!(
        unobserved["reasons"]
            .as_array()
            .unwrap()
            .iter()
            .any(|reason| reason
                .as_str()
                .unwrap_or("")
                .contains("without observed CPU masks")),
        "{unobserved}"
    );

    // A genuinely remote lane needs no local partitioning.
    let remote = eligibility(json!({
        "kind": "remote", "sameHost": false, "cpuPartitioned": null,
    }));
    assert_eq!(remote["eligible"], true, "{remote}");
}

/// CPU-list parsing and disjointness, as the topology check uses them.
#[test]
fn cpu_masks_parse_and_compare() {
    assert_eq!(parse_cpu_mask("0-2"), Some(vec![0, 1, 2]));
    assert_eq!(parse_cpu_mask("0,2-3"), Some(vec![0, 2, 3]));
    assert_eq!(parse_cpu_mask(" 4 "), Some(vec![4]));
    assert_eq!(parse_cpu_mask(""), None);
    assert_eq!(parse_cpu_mask("3-1"), None);
    assert_eq!(parse_cpu_mask("x"), None);

    assert_eq!(cpu_masks_disjoint("0-2", "3-5"), Some(true));
    assert_eq!(cpu_masks_disjoint("0-3", "3-5"), Some(false));
    assert_eq!(cpu_masks_disjoint("0-2", "bogus"), None);
}
