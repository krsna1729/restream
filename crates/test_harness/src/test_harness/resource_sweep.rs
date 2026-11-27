use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::process::{Child, Command};

use super::{
    FfmpegStats, GeneralizedSinkMetrics, GeneralizedSinkServer, HarnessSrtCrypto, HarnessSrtMode,
    HarnessSrtSinkPool, MSR_LANGUAGE_CODES, MSR_LANGUAGE_NAMES, PublishTrackSelection, RampApi,
    RtmpOutputMode, SignalTolerances, append_line, append_srt_crypto,
    apply_harness_srt_listener_env, apply_srt_listener_env, capture_signal_sample, cleanup_ramp_db,
    create_backup_input, create_output, create_output_with_rtmp_mode, decode_pcm_quality,
    default_restream_bin, default_work_db_path, env_secs, env_usize, ffmpeg_children_stats,
    ffprobe_live_sample, file_tail_lines, harness_admin_password, harness_port_defaults,
    harness_srt_crypto_from_env, harness_srt_ffmpeg_url, harness_srt_standard_publish_url,
    mediamtx_udp_read_buffer_line, parse_srt_crypto_variants, pinned_mediamtx,
    plain_mediamtx_config, probe_dims_ramp, read_proc_status_kb, round2, run_ffmpeg_filter_log,
    safe_artifact_stem, scaled_output_progress_timeout, signal_report_json, spawn_mediamtx,
    spawn_publisher_with_selection, start_generalized_sink_server, start_output, stop_child,
    stop_generalized_sink_server, validate_signal_quality_with_tolerances, wait_for_api_input_live,
    wait_for_http_ok, wait_for_input_state, wait_for_outputs_progress,
};

#[path = "resource_sweep/bitrate.rs"]
mod bitrate;
pub(crate) use bitrate::bitrate_sweep;
#[path = "resource_sweep/msr.rs"]
mod msr;
pub(crate) use msr::*;

#[path = "resource_sweep/branch_matrix.rs"]
mod branch_matrix;
pub(crate) use branch_matrix::{branch_matrix, srt_crypto_matrix};
#[path = "resource_sweep/catalog.rs"]
mod catalog;
pub(crate) use catalog::{
    ResourceEgressScenario, SweepOutputKind, resource_egress_scenario, resource_egress_scenarios,
};
#[path = "resource_sweep/config.rs"]
mod config;
pub(crate) use config::SweepConfig;
use config::{
    ResourceSweepEnv, ResourceSweepLifecycle, ResourceSweepPeer, parse_string_set,
    parse_sweep_configs, parse_usize_list, sweep_configs,
};
#[path = "resource_sweep/burst.rs"]
mod burst;
#[path = "resource_sweep/delivery.rs"]
mod delivery;
#[path = "resource_sweep/first_progress.rs"]
mod first_progress;
#[path = "resource_sweep/measurement.rs"]
mod measurement;
pub(crate) use measurement::read_proc_status_kb_checked;
#[path = "resource_sweep/hls_sink.rs"]
pub(crate) mod hls_sink;
#[path = "resource_sweep/peer_ports.rs"]
mod peer_ports;
#[path = "resource_sweep/thread_cpu.rs"]
mod thread_cpu;
use peer_ports::instance_suffixed_path;
#[cfg(test)]
use peer_ports::{peer_instance_for, srt_url_on_host};
#[path = "resource_sweep/packet_contract.rs"]
mod packet_contract;
#[path = "resource_sweep/packet_contract_peers.rs"]
mod packet_contract_peers;
#[path = "resource_sweep/packet_contract_run.rs"]
mod packet_contract_run;
#[path = "resource_sweep/packet_contract_summary.rs"]
mod packet_contract_summary;
#[path = "resource_sweep/packet_contract_tests.rs"]
#[cfg(test)]
mod packet_contract_tests;
#[path = "resource_sweep/packet_contract_verdict.rs"]
mod packet_contract_verdict;
#[path = "resource_sweep/packet_contract_verdict_tests.rs"]
#[cfg(test)]
mod packet_contract_verdict_tests;
#[path = "resource_sweep/sweep_tests.rs"]
#[cfg(test)]
mod sweep_tests;
use catalog::ResourceScenarioMeta;
use measurement::{
    ResourceAggregate, csv_escape, read_proc_stat_ticks, resource_aggregate_json,
    sample_resource_window, write_resource_sweep_csv,
};

/// Live process stack shared by a resource-sweep sample.
///
/// Exactly one of `mediamtx`/`sink_peers` is populated, one entry per peer
/// instance (`env.peer_count`, default 1), depending on `env.peer_mode`:
/// `mediamtx` holds real child processes (`env.peer_mode ==
/// ResourceSweepPeer::Mediamtx`); `sink_peers` holds in-process
/// harness-native listeners (`env.peer_mode == ResourceSweepPeer::Sink` --
/// see `harness_srt_sink.rs` and `sinks.rs`'s `GeneralizedSinkServer`, which
/// replaced spawning a separate `restream --sink-mode` process). Every
/// non-`msr` resource-sweep scenario runs with `peer_count == 1` and
/// `peer_mode == Mediamtx`, so `mediamtx` stays a single-element Vec and
/// `sink_peers` stays empty there, matching pre-existing behavior.
struct ResourceSweepStack {
    mediamtx: Vec<Child>,
    sink_peers: SinkPeerStack,
    restream: Child,
    api: RampApi,
    restream_pid: u32,
}

/// In-process harness-native sink peers: one RTMP accept-and-discard
/// listener per `PEER_COUNT` instance, plus one `HarnessSrtSinkPool` with one
/// production SRT Owner thread per sink port (`PeerPorts::srt_ports`).
#[derive(Default)]
struct SinkPeerStack {
    rtmp: Vec<GeneralizedSinkServer>,
    /// Counting-only metrics of every RTMP/RTMPS sink listener, for
    /// per-connection delivery.
    rtmp_metrics: Vec<Arc<GeneralizedSinkMetrics>>,
    srt_pool: Option<HarnessSrtSinkPool>,
    hls: Option<hls_sink::HlsCountingSink>,
}

/// Stop every child in a peer-instance Vec (or any other child list),
/// mirroring the single-child `stop_child` used elsewhere in this module.
async fn stop_children(children: &mut [Child]) {
    for child in children {
        stop_child(child).await;
    }
}

/// Stop every in-process harness sink peer (RTMP listeners and the shared
/// SRT pool). Mirrors `stop_children`'s role for the mediamtx-`Child` path.
async fn stop_harness_sink_peers(stack: &mut SinkPeerStack) {
    for server in stack.rtmp.drain(..) {
        stop_generalized_sink_server(server);
    }
    stack.rtmp_metrics.clear();
    if let Some(pool) = stack.srt_pool.take() {
        pool.stop();
    }
    if let Some(sink) = stack.hls.take() {
        sink.stop();
    }
}

pub(crate) async fn resource_sweep() -> Result<Value, String> {
    let env = ResourceSweepEnv::from_env()?;
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.samples_jsonl);
    packet_contract::begin(&env.work_dir, packet_contract::RunMetadata::from_env(&env));

    let mut stack = if env.lifecycle == ResourceSweepLifecycle::Isolated {
        None
    } else {
        Some(start_resource_sweep_stack(&env, LocalPeerNeeds::ALL).await?)
    };
    let mut retained_publishers: Vec<Child> = Vec::new();
    let mut aggregates = Vec::new();

    if env.scenario_enabled("baseline-empty") {
        aggregates.push(run_resource_baseline(&env, &mut stack, &mut retained_publishers).await?);
    }
    if env.scenario_enabled("ingest-only") {
        for config in sweep_configs() {
            aggregates.push(
                run_resource_ingest_only(&env, &mut stack, &mut retained_publishers, *config)
                    .await?,
            );
        }
    }
    if env.scenario_enabled("ingest-growth-same") {
        aggregates.extend(
            run_resource_ingest_growth(&env, &mut stack, &mut retained_publishers, false).await?,
        );
    }
    if env.scenario_enabled("ingest-growth-mixed") {
        aggregates.extend(
            run_resource_ingest_growth(&env, &mut stack, &mut retained_publishers, true).await?,
        );
    }
    for scenario in resource_egress_scenarios() {
        if !env.scenario_enabled(&scenario.name) {
            continue;
        }
        aggregates.extend(
            run_resource_egress_growth(
                &env,
                &mut stack,
                &mut retained_publishers,
                &scenario.name,
                sweep_configs()[scenario.config_index],
                &scenario.output_kinds,
            )
            .await?,
        );
    }

    write_resource_sweep_csv(&env.summary_csv, &aggregates)?;
    let packet_contract_summary = packet_contract::finish(&env.work_dir)?;
    let result = json!({
        "mode": "resource-sweep",
        "lifecycle": env.lifecycle.as_str(),
        "artifacts": {
            "summaryJson": env.summary_json,
            "summaryCsv": env.summary_csv,
            "samplesJsonl": env.samples_jsonl,
            "packetContractSamplesJsonl": packet_contract::samples_jsonl(&env.work_dir),
            "packetContractJson": packet_contract_summary,
            "restreamLog": env.restream_log,
            "mediamtxLog": env.mediamtx_log,
        },
        "aggregates": aggregates.iter().map(resource_aggregate_json).collect::<Vec<_>>(),
    });
    std::fs::write(
        &env.summary_json,
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .map_err(|e| e.to_string())?;

    if env.no_cleanup {
        println!("resource-sweep no-cleanup: leaving final stack running");
        // kill_on_drop(true) is set at spawn time for these children, so simply
        // skipping stop_child() isn't enough — dropping the Child handles below
        // (at function return) would still SIGKILL them. mem::forget leaks the
        // handles instead, which is fine since the process is about to _exit.
        for child in retained_publishers.drain(..) {
            std::mem::forget(child);
        }
        if let Some(stack) = stack.take() {
            std::mem::forget(stack);
        }
    } else {
        for child in &mut retained_publishers {
            stop_child(child).await;
        }
        if let Some(stack) = stack.as_mut() {
            stop_child(&mut stack.restream).await;
            stop_children(&mut stack.mediamtx).await;
        }
    }
    Ok(result)
}

async fn spawn_mediamtx_peer(env: &ResourceSweepEnv, index: usize) -> Result<Child, String> {
    let (rtmp, rtmps, srt, api) = env.peer_ports().instance(index);
    let log_path = instance_suffixed_path(&env.mediamtx_log, index);
    let config_path = instance_suffixed_path(&env.mediamtx_config, index);
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let rtmp_tls_lines = match &env.rtmps_tls {
        Some((cert, key)) => format!(
            "rtmpEncryption: \"optional\"\nrtmpsAddress: :{}\nrtmpServerCert: {}\nrtmpServerKey: {}\n",
            rtmps,
            cert.display(),
            key.display()
        ),
        None => "rtmpEncryption: \"no\"\n".to_string(),
    };
    std::fs::write(
        &config_path,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nwriteQueueSize: 512\n{udp}rtmp: yes\nrtmpAddress: :{rtmp}\n{rtmp_tls_lines}rtsp: no\nsrt: yes\nsrtAddress: :{srt}\nhls: no\nwebrtc: no\nmoq: no\napi: yes\napiAddress: :{api}\nmetrics: no\npaths:\n  all:\n",
            udp = mediamtx_udp_read_buffer_line(),
        ),
    )
    .map_err(|e| e.to_string())?;
    spawn_mediamtx(
        Command::new(pinned_mediamtx()?),
        &config_path,
        log,
        api,
        Duration::from_secs(30),
        &format!("mediamtx[{index}]"),
    )
    .await
}

async fn verify_preexisting_mediamtx_peer(index: usize, api_port: u16) -> Result<Child, String> {
    // Mediamtx pre-started externally — verify it's live, don't spawn. The
    // dummy child is just a `Vec<Child>`-shaped placeholder; stopping it
    // later is a no-op against the already-exited `true` process.
    let mut dummy = Command::new("true")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("dummy: {e}"))?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{api_port}/v3/paths/list"),
        Duration::from_secs(10),
    )
    .await
    {
        let _ = dummy.kill().await;
        return Err(format!("pre-started mediamtx[{index}] not ready: {err}"));
    }
    Ok(dummy)
}

async fn start_resource_sweep_peers(env: &ResourceSweepEnv) -> Result<Vec<Child>, String> {
    let skip_start = std::env::var("PEER_SKIP_START").is_ok();
    let mut children = Vec::with_capacity(env.peer_count);
    for index in 0..env.peer_count {
        let spawned = if skip_start {
            let (_, _, _, api) = env.peer_ports().instance(index);
            verify_preexisting_mediamtx_peer(index, api).await
        } else {
            spawn_mediamtx_peer(env, index).await
        };
        match spawned {
            Ok(child) => children.push(child),
            Err(err) => {
                stop_children(&mut children).await;
                return Err(err);
            }
        }
    }
    Ok(children)
}

/// Starts the in-process harness-native sink peers: per instance an RTMP
/// accept-and-discard listener, plus one SRT Owner thread per sink port
/// (`PeerPorts::srt_ports`) -- no readiness polling needed, unlike spawning a
/// separate process, since a synchronous bind either succeeds or fails
/// immediately.
/// `PEER_SKIP_START` is not honoured here: it exists for pre-started
/// external processes (mediamtx), which an in-process listener has no
/// equivalent of -- a harness sink peer is always started fresh, bound
/// directly by this harness process.
async fn start_harness_sink_peers(
    env: &ResourceSweepEnv,
    needs: LocalPeerNeeds,
) -> Result<SinkPeerStack, String> {
    let mut rtmp = Vec::with_capacity(env.peer_count);
    let mut rtmp_metrics = Vec::with_capacity(env.peer_count);
    let mut srt_ports = Vec::with_capacity(env.peer_count);
    for index in 0..env.peer_count {
        let (rtmp_port, rtmps_port, _, _api) = env.peer_ports().instance(index);
        if needs.rtmp
            && let Some((cert, key)) = &env.rtmps_tls
        {
            let metrics = Arc::new(GeneralizedSinkMetrics::counting_only());
            match crate::sinks::start_generalized_rtmps_sink_server(
                rtmps_port,
                cert,
                key,
                metrics.clone(),
            )
            .await
            {
                Ok(server) => {
                    rtmp.push(server);
                    rtmp_metrics.push(metrics);
                }
                Err(err) => {
                    for server in rtmp {
                        stop_generalized_sink_server(server);
                    }
                    return Err(format!(
                        "harness sink RTMPS listener on {rtmps_port}: {err}"
                    ));
                }
            }
        }
        if needs.rtmp {
            let metrics = Arc::new(GeneralizedSinkMetrics::counting_only());
            match start_generalized_sink_server(rtmp_port, metrics.clone()).await {
                Ok(server) => {
                    rtmp.push(server);
                    rtmp_metrics.push(metrics);
                }
                Err(err) => {
                    for server in rtmp {
                        stop_generalized_sink_server(server);
                    }
                    return Err(format!("harness sink RTMP listener on {rtmp_port}: {err}"));
                }
            }
        }
        srt_ports.extend(env.peer_ports().srt_ports(index));
    }

    let srt_pool = if needs.srt {
        let udp_buffer = env_usize(
            "HARNESS_SRT_SINK_UDP_BUFFER",
            crate::harness_srt_sink::DEFAULT_UDP_BUFFER,
        );
        match HarnessSrtSinkPool::start(&srt_ports, udp_buffer) {
            Ok(pool) => Some(pool),
            Err(err) => {
                for server in rtmp {
                    stop_generalized_sink_server(server);
                }
                return Err(format!("harness SRT sink pool on {srt_ports:?}: {err}"));
            }
        }
    } else {
        None
    };
    let hls = if needs.hls {
        match hls_sink::HlsCountingSink::start(harness_port_defaults().hls_put).await {
            Ok(sink) => Some(sink),
            Err(err) => {
                for server in rtmp {
                    stop_generalized_sink_server(server);
                }
                if let Some(pool) = srt_pool {
                    pool.stop();
                }
                return Err(err);
            }
        }
    } else {
        None
    };
    Ok(SinkPeerStack {
        rtmp,
        rtmp_metrics,
        srt_pool,
        hls,
    })
}

/// Which local sink peers a scenario needs. RTMP outputs always terminate on
/// local peers; SRT outputs do only when no remote hosts are configured, so a
/// remote SRT-only rung starts no local sink pool at all — idle workers there
/// would only perturb scheduling and the host counters the contract reads.
#[derive(Clone, Copy, PartialEq, Eq)]
struct LocalPeerNeeds {
    rtmp: bool,
    srt: bool,
    hls: bool,
}

impl LocalPeerNeeds {
    const ALL: Self = Self {
        rtmp: true,
        srt: true,
        hls: true,
    };

    fn for_output_kinds(env: &ResourceSweepEnv, kinds: &[SweepOutputKind]) -> Self {
        let is_srt = |kind: &SweepOutputKind| {
            matches!(
                kind,
                SweepOutputKind::SrtSource | SweepOutputKind::Srt720p | SweepOutputKind::Srt1080p
            )
        };
        let is_hls = |kind: &SweepOutputKind| *kind == SweepOutputKind::HlsPut;
        Self {
            rtmp: kinds.iter().any(|kind| !is_srt(kind) && !is_hls(kind)),
            srt: kinds.iter().any(is_srt) && env.srt_peer_hosts.is_empty(),
            hls: kinds.iter().any(is_hls),
        }
    }
}

async fn start_resource_sweep_stack(
    env: &ResourceSweepEnv,
    needs: LocalPeerNeeds,
) -> Result<ResourceSweepStack, String> {
    if !env.restream_bin.exists() {
        return Err(format!(
            "restream binary not found at {}",
            env.restream_bin.display()
        ));
    }
    std::fs::create_dir_all(env.work_dir.join("logs")).map_err(|e| e.to_string())?;
    cleanup_ramp_db(&env.restream_db_path);
    let mut mediamtx = Vec::new();
    let mut sink_peers = SinkPeerStack::default();
    match env.peer_mode {
        ResourceSweepPeer::Mediamtx => mediamtx = start_resource_sweep_peers(env).await?,
        ResourceSweepPeer::Sink => sink_peers = start_harness_sink_peers(env, needs).await?,
    }

    let restream_log = std::fs::File::create(&env.restream_log).map_err(|e| e.to_string())?;
    let restream_err = restream_log.try_clone().map_err(|e| e.to_string())?;
    // The measured datapath runs on its own CPUs when the operator partitions
    // the host (`RESTREAM_CPUSET`), so receiver work cannot steal the core
    // under measurement. The contract records the *observed* mask from
    // `/proc/<pid>/status`, not this request.
    let mut restream_cmd = match std::env::var("RESTREAM_CPUSET") {
        Ok(mask) if !mask.trim().is_empty() => {
            let mut cmd = Command::new("taskset");
            cmd.arg("-c").arg(mask.trim()).arg(&env.restream_bin);
            cmd
        }
        _ => Command::new(&env.restream_bin),
    };
    restream_cmd
        .env("RESTREAM_HTTP_PORT", env.restream_http.to_string())
        .env("RESTREAM_RTMP_PORT", env.restream_rtmp.to_string())
        .env("RESTREAM_SRT_PORT", env.restream_srt.to_string())
        .env("RESTREAM_INITIAL_ADMIN_PASSWORD", harness_admin_password())
        .env("RESTREAM_LOG_DIR", env.work_dir.join("logs"))
        .env(
            "RESTREAM_DB_PATH",
            env.restream_db_path.to_string_lossy().to_string(),
        )
        .stdout(Stdio::from(restream_log))
        .stderr(Stdio::from(restream_err))
        .kill_on_drop(true);
    // The fixture certificate the HTTPS HLS sink and the MediaMTX RTMPS
    // listener present, trusted as the generic harness spawn does.
    let (cert, _) = restream_engine::test_fixtures::rtmps_harness_cert_fixture()?;
    let cert = std::path::absolute(&cert).map_err(|e| e.to_string())?;
    restream_cmd.env("RESTREAM_RTMPS_EXTRA_TRUST_ROOTS_PEM", cert);
    apply_srt_listener_env(&mut restream_cmd, &env.srt_crypto);
    let mut restream = restream_cmd.spawn().map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/healthz", env.restream_http),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut restream).await;
        stop_children(&mut mediamtx).await;
        stop_harness_sink_peers(&mut sink_peers).await;
        return Err(format!("restream did not become ready: {err}"));
    }
    let mut api = RampApi::new(env.restream_http);
    api.login().await?;
    let restream_pid = restream.id().ok_or("restream pid missing")?;
    Ok(ResourceSweepStack {
        mediamtx,
        sink_peers,
        restream,
        api,
        restream_pid,
    })
}

async fn ensure_resource_stack<'a>(
    env: &ResourceSweepEnv,
    stack: &'a mut Option<ResourceSweepStack>,
    needs: LocalPeerNeeds,
) -> Result<&'a mut ResourceSweepStack, String> {
    if stack.is_none() {
        *stack = Some(start_resource_sweep_stack(env, needs).await?);
    }
    stack
        .as_mut()
        .ok_or("resource sweep stack missing".to_string())
}

async fn run_resource_baseline(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
) -> Result<ResourceAggregate, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env, LocalPeerNeeds::ALL).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack, LocalPeerNeeds::ALL).await?
    };
    let meta = ResourceScenarioMeta {
        scenario: "baseline-empty",
        label: "empty".to_string(),
        pipelines: 0,
        outputs: 0,
        ingest_types: "none".to_string(),
        egress_mix: "none".to_string(),
        transcode: "none",
    };
    let aggregate = sample_resource_window(env, active, meta).await?;
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_children(&mut local_stack.as_mut().unwrap().mediamtx).await;
        stop_harness_sink_peers(&mut local_stack.as_mut().unwrap().sink_peers).await;
    }
    let _ = retained_publishers;
    Ok(aggregate)
}

async fn run_resource_ingest_only(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
    config: SweepConfig,
) -> Result<ResourceAggregate, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env, LocalPeerNeeds::ALL).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack, LocalPeerNeeds::ALL).await?
    };
    let stream_key = format!("resource-{}", config.name);
    let pipeline_id = create_resource_pipeline(&active.api, config.name, &stream_key).await?;
    let mut publisher = spawn_resource_publisher(env, config, &stream_key)?;
    wait_for_api_input_live(&active.api, &pipeline_id, Duration::from_secs(45)).await?;
    let meta = ResourceScenarioMeta {
        scenario: "ingest-only",
        label: config.name.to_string(),
        pipelines: 1,
        outputs: 0,
        ingest_types: config.name.to_string(),
        egress_mix: "none".to_string(),
        transcode: "none",
    };
    let aggregate = sample_resource_window(env, active, meta).await?;
    if env.lifecycle == ResourceSweepLifecycle::Cumulative {
        retained_publishers.push(publisher);
    } else {
        stop_child(&mut publisher).await;
        delete_resource_pipeline(&active.api, &pipeline_id).await;
    }
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_children(&mut local_stack.as_mut().unwrap().mediamtx).await;
        stop_harness_sink_peers(&mut local_stack.as_mut().unwrap().sink_peers).await;
    }
    Ok(aggregate)
}

async fn run_resource_ingest_growth(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
    mixed: bool,
) -> Result<Vec<ResourceAggregate>, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env, LocalPeerNeeds::ALL).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack, LocalPeerNeeds::ALL).await?
    };

    let mut publishers = Vec::new();
    let mut pipeline_ids = Vec::new();
    let mut feed_outputs: Vec<(String, Vec<String>)> = Vec::new();
    let outputs_per_feed = if mixed {
        0
    } else {
        env.ingest_outputs_per_feed
    };
    let max_ingests = *env.ingest_counts.iter().max().unwrap_or(&1);
    let same_growth = *sweep_configs()
        .iter()
        .find(|config| config.name == env.ingest_growth_config)
        .ok_or_else(|| format!("unknown ingest growth config {}", env.ingest_growth_config))?;
    let mut out = Vec::new();
    for index in 1..=max_ingests {
        let config = if mixed {
            sweep_configs()[index - 1]
        } else {
            same_growth
        };
        let (publisher, pipeline_id, output_ids) =
            start_growth_feed(env, &active.api, index, config, outputs_per_feed).await?;
        publishers.push(publisher);
        feed_outputs.push((pipeline_id.clone(), output_ids));
        pipeline_ids.push(pipeline_id);
        if env.ingest_counts.contains(&index) {
            out.push(
                sample_ingest_checkpoint(env, active, index, mixed, same_growth, &feed_outputs)
                    .await?,
            );
        }
    }
    if env.lifecycle == ResourceSweepLifecycle::Cumulative {
        retained_publishers.extend(publishers);
    } else {
        for child in &mut publishers {
            stop_child(child).await;
        }
        for pipeline_id in pipeline_ids {
            delete_resource_pipeline(&active.api, &pipeline_id).await;
        }
    }
    if local_only {
        stop_isolated_stack(&mut local_stack).await;
    }
    Ok(out)
}

async fn run_resource_egress_growth(
    env: &ResourceSweepEnv,
    stack: &mut Option<ResourceSweepStack>,
    retained_publishers: &mut Vec<Child>,
    scenario_name: &str,
    config: SweepConfig,
    output_kinds: &[SweepOutputKind],
) -> Result<Vec<ResourceAggregate>, String> {
    let local_only = env.lifecycle == ResourceSweepLifecycle::Isolated;
    let peer_needs = LocalPeerNeeds::for_output_kinds(env, output_kinds);
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env, peer_needs).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack, peer_needs).await?
    };
    let stream_key = format!("resource-{scenario_name}");
    let pipeline_id = create_resource_pipeline(&active.api, scenario_name, &stream_key).await?;
    let mut publisher = spawn_resource_publisher(env, config, &stream_key)?;
    wait_for_api_input_live(&active.api, &pipeline_id, Duration::from_secs(45)).await?;
    let mut output_ids = Vec::new();
    let mut output_starts: Vec<(String, Instant)> = Vec::new();
    let max_outputs = *env.egress_counts.iter().max().unwrap_or(&1);
    let planned = catalog::plan_outputs(scenario_name, output_kinds, max_outputs);
    let burst_mode = burst::burst_enabled();
    if burst_mode {
        output_starts =
            start_egress_burst(env, &active.api, &pipeline_id, config, &planned).await?;
        output_ids = output_starts.iter().map(|(id, _)| id.clone()).collect();
    }
    let mut out = Vec::new();
    for index in 1..=max_outputs {
        if !burst_mode {
            let started =
                start_egress_outputs(env, &active.api, &pipeline_id, config, &planned, index)
                    .await?;
            output_ids.extend(started.iter().map(|(id, _)| id.clone()));
            output_starts.extend(started);
        }
        if env.egress_counts.contains(&index) {
            sample_egress_checkpoint(
                EgressCheckpoint {
                    env,
                    stack: active,
                    pipeline_id: &pipeline_id,
                    scenario_name,
                    config,
                    output_kinds,
                    planned: &planned,
                    index,
                    output_ids: &output_ids,
                    output_starts: &output_starts,
                    burst_mode,
                },
                &mut out,
            )
            .await?;
        }
    }
    if env.lifecycle == ResourceSweepLifecycle::Cumulative {
        retained_publishers.push(publisher);
    } else {
        stop_child(&mut publisher).await;
        delete_resource_pipeline(&active.api, &pipeline_id).await;
    }
    if local_only {
        stop_isolated_stack(&mut local_stack).await;
    }
    Ok(out)
}

/// Create one ingest feed and its outputs; waits for the input to go live.
async fn start_growth_feed(
    env: &ResourceSweepEnv,
    api: &RampApi,
    index: usize,
    config: SweepConfig,
    outputs_per_feed: usize,
) -> Result<(Child, String, Vec<String>), String> {
    let stream_key = format!("resource-growth-{index}-{}", config.name);
    let pipeline_id =
        create_resource_pipeline(api, &format!("{}-{index}", config.name), &stream_key).await?;
    let publisher = spawn_resource_publisher(env, config, &stream_key)?;
    wait_for_api_input_live(api, &pipeline_id, Duration::from_secs(45)).await?;
    let kind = env.ingest_output_kind;
    let mut output_ids = Vec::with_capacity(outputs_per_feed);
    for output in 1..=outputs_per_feed {
        let name = format!("{stream_key}-{}-{output}", kind.label());
        let (url, encoding) = env.output_url(config, kind, &name);
        let output_id = create_output_with_rtmp_mode(
            api,
            &pipeline_id,
            &name,
            &url,
            &encoding,
            kind.rtmp_mode(),
        )
        .await?;
        start_output(api, &pipeline_id, &output_id).await?;
        output_ids.push(output_id);
    }
    Ok((publisher, pipeline_id, output_ids))
}

/// Wait for every feed's progress and sample one ingest-growth checkpoint.
async fn sample_ingest_checkpoint(
    env: &ResourceSweepEnv,
    stack: &mut ResourceSweepStack,
    index: usize,
    mixed: bool,
    same_growth: SweepConfig,
    feed_outputs: &[(String, Vec<String>)],
) -> Result<ResourceAggregate, String> {
    let total_outputs = feed_outputs.iter().map(|(_, ids)| ids.len()).sum::<usize>();
    for (pipeline_id, output_ids) in feed_outputs {
        wait_for_outputs_progress(
            &stack.api,
            pipeline_id,
            output_ids,
            resource_output_progress_timeout(total_outputs),
        )
        .await?;
    }
    let ingest_types = if mixed {
        sweep_configs()
            .iter()
            .take(index)
            .map(|cfg| cfg.name)
            .collect::<Vec<_>>()
            .join(",")
    } else {
        same_growth.name.to_string()
    };
    sample_resource_window(
        env,
        stack,
        ResourceScenarioMeta {
            scenario: if mixed {
                "ingest-growth-mixed"
            } else {
                "ingest-growth-same"
            },
            label: format!("{index}-pipelines"),
            pipelines: index,
            outputs: total_outputs,
            ingest_types,
            egress_mix: if total_outputs == 0 {
                "none".to_string()
            } else {
                env.ingest_output_kind.label().to_string()
            },
            transcode: "none",
        },
    )
    .await
}

/// Start every planned output in one burst and return their start times.
async fn start_egress_burst(
    env: &ResourceSweepEnv,
    api: &RampApi,
    pipeline_id: &str,
    config: SweepConfig,
    planned: &[catalog::PlannedOutput],
) -> Result<Vec<(String, Instant)>, String> {
    let specs: Vec<burst::BurstSpec> = planned
        .iter()
        .map(|(_, kind, name)| {
            let (url, encoding) = env.output_url(config, *kind, name);
            (name.clone(), url, encoding, kind.rtmp_mode())
        })
        .collect();
    burst::create_and_start_all(api, pipeline_id, &specs).await
}

/// Start the outputs planned for one egress rung.
async fn start_egress_outputs(
    env: &ResourceSweepEnv,
    api: &RampApi,
    pipeline_id: &str,
    config: SweepConfig,
    planned: &[catalog::PlannedOutput],
    index: usize,
) -> Result<Vec<(String, Instant)>, String> {
    let mut starts = Vec::new();
    for (_, kind, name) in planned
        .iter()
        .filter(|(planned_index, ..)| *planned_index == index)
    {
        let (url, encoding) = env.output_url(config, *kind, name);
        let output_id =
            create_output_with_rtmp_mode(api, pipeline_id, name, &url, &encoding, kind.rtmp_mode())
                .await?;
        start_output(api, pipeline_id, &output_id).await?;
        starts.push((output_id, Instant::now()));
    }
    Ok(starts)
}

/// One egress-growth checkpoint: first-progress, progress wait, packet
/// contract peers and the resource sample.
struct EgressCheckpoint<'a> {
    env: &'a ResourceSweepEnv,
    stack: &'a mut ResourceSweepStack,
    pipeline_id: &'a str,
    scenario_name: &'a str,
    config: SweepConfig,
    output_kinds: &'a [SweepOutputKind],
    planned: &'a [catalog::PlannedOutput],
    index: usize,
    output_ids: &'a [String],
    output_starts: &'a [(String, Instant)],
    burst_mode: bool,
}

async fn sample_egress_checkpoint(
    checkpoint: EgressCheckpoint<'_>,
    out: &mut Vec<ResourceAggregate>,
) -> Result<(), String> {
    let EgressCheckpoint {
        env,
        stack,
        pipeline_id,
        scenario_name,
        config,
        output_kinds,
        planned,
        index,
        output_ids,
        output_starts,
        burst_mode,
    } = checkpoint;
    let api = &stack.api;
    let progress_timeout = resource_output_progress_timeout(output_ids.len());
    first_progress::record_first_progress(
        api,
        pipeline_id,
        output_starts,
        progress_timeout,
        &env.work_dir
            .join(format!("first-progress-{}.json", output_ids.len())),
    )
    .await;
    wait_for_outputs_progress(api, pipeline_id, output_ids, progress_timeout).await?;
    let mut expected_per_host = vec![0_usize; env.srt_peer_hosts.len()];
    for name in catalog::live_output_names(planned, index, burst_mode) {
        if let Some(host) = env.srt_peer_host_for(name)
            && let Some(index) = env.srt_peer_hosts.iter().position(|peer| peer == host)
        {
            expected_per_host[index] += 1;
        }
    }
    packet_contract::set_peer_expected_outputs(expected_per_host);
    out.push(
        sample_resource_window(
            env,
            stack,
            ResourceScenarioMeta {
                scenario: scenario_name,
                label: format!("{index}-per-group"),
                pipelines: 1,
                outputs: output_ids.len(),
                ingest_types: config.name.to_string(),
                egress_mix: output_kinds
                    .iter()
                    .map(|kind| kind.label())
                    .collect::<Vec<_>>()
                    .join(","),
                transcode: if output_kinds.iter().any(|kind| {
                    matches!(
                        kind,
                        SweepOutputKind::Rtmp720p
                            | SweepOutputKind::Srt720p
                            | SweepOutputKind::Rtmp1080p
                            | SweepOutputKind::Srt1080p
                            | SweepOutputKind::RtmpSourceDownmix
                            | SweepOutputKind::SrtSourceDownmix
                    )
                }) {
                    "yes"
                } else {
                    "no"
                },
            },
        )
        .await?,
    );
    Ok(())
}

/// Stop an isolated run's local restream, MediaMTX and sink peers.
async fn stop_isolated_stack(local_stack: &mut Option<ResourceSweepStack>) {
    if let Some(local) = local_stack.as_mut() {
        stop_child(&mut local.restream).await;
        stop_children(&mut local.mediamtx).await;
        stop_harness_sink_peers(&mut local.sink_peers).await;
    }
}

pub(crate) async fn create_resource_pipeline(
    api: &RampApi,
    name: &str,
    stream_key: &str,
) -> Result<String, String> {
    let pipeline = api
        .post_json(
            "/api/v1/pipelines",
            json!({"name": name, "streamKey": stream_key}),
        )
        .await?;
    pipeline["pipeline"]["id"]
        .as_str()
        .map(str::to_string)
        .ok_or("pipeline create response missing pipeline.id".to_string())
}

async fn delete_resource_pipeline(api: &RampApi, pipeline_id: &str) {
    let _ = api
        .delete_json(&format!("/api/v1/pipelines/{pipeline_id}"))
        .await;
}

fn spawn_resource_publisher(
    env: &ResourceSweepEnv,
    config: SweepConfig,
    stream_key: &str,
) -> Result<Child, String> {
    spawn_resource_publisher_with_bitrate(
        env.restream_rtmp,
        env.restream_srt,
        &env.work_dir,
        &env.srt_crypto,
        config,
        stream_key,
        &env.bitrate,
    )
}

fn spawn_resource_publisher_with_bitrate(
    restream_rtmp: u16,
    restream_srt: u16,
    work_dir: &Path,
    srt_crypto: &HarnessSrtCrypto,
    config: SweepConfig,
    stream_key: &str,
    bitrate: &str,
) -> Result<Child, String> {
    let log_path = work_dir.join(format!("publisher-{stream_key}.log"));
    let fixture = sweep_fixture(config, bitrate)?;
    let (url, format, selection) = if config.ingest_proto == "rtmp" {
        (
            format!("rtmp://127.0.0.1:{restream_rtmp}/live/{stream_key}"),
            "flv",
            PublishTrackSelection::PrimaryAv,
        )
    } else {
        (
            append_srt_crypto(
                harness_srt_ffmpeg_url(restream_srt, stream_key, HarnessSrtMode::Publish, None),
                srt_crypto,
            ),
            "mpegts",
            if config.multi_audio {
                PublishTrackSelection::AllStreams
            } else {
                PublishTrackSelection::PrimaryAv
            },
        )
    };
    spawn_publisher_with_selection(&fixture, &url, format, selection, Some(&log_path))
}

fn resource_output_progress_timeout(output_count: usize) -> Duration {
    let base_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_BASE_SECS", 30);
    let per_output_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_PER_OUTPUT_SECS", 4);
    let cap_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_CAP_SECS", 240);
    scaled_output_progress_timeout(output_count, base_secs, per_output_secs, cap_secs)
}

fn sweep_fixture(config: SweepConfig, bitrate_label: &str) -> Result<PathBuf, String> {
    restream_engine::test_fixtures::bench_transport_fixture(
        config.video_codec,
        bitrate_label,
        config.multi_audio,
    )
}
