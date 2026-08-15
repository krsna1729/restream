use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::process::{Child, Command};

use super::{
    FfmpegStats, HarnessSrtCrypto, HarnessSrtMode, MSR_LANGUAGE_CODES, MSR_LANGUAGE_NAMES,
    MixedEnv, PublishTrackSelection, RampApi, RtmpOutputMode, SignalTolerances, append_line,
    append_srt_crypto, apply_harness_srt_listener_env, apply_srt_listener_env,
    capture_signal_sample, cleanup_ramp_db, create_backup_input, create_output,
    create_output_with_rtmp_mode, decode_pcm_quality, default_restream_bin, default_work_db_path,
    env_secs, env_usize, ffprobe_live_sample, file_tail_lines, harness_admin_password,
    harness_port_defaults, harness_srt_crypto_from_env, harness_srt_ffmpeg_url,
    harness_srt_standard_publish_url, mixed_input_case_for_command, parse_srt_crypto_variants,
    probe_dims_ramp, remove_mediamtx_config_env, run_ffmpeg_filter_log,
    run_mixed_input_case_with_env, safe_artifact_stem, signal_report_json,
    spawn_publisher_with_selection, start_output, stop_child, sweep_fixture,
    validate_signal_quality_with_tolerances, wait_for_api_input_live, wait_for_http_ok,
    wait_for_input_state, wait_for_outputs_progress, wait_for_tcp_listener_ready,
    wait_for_udp_listener_ready,
};

#[path = "resource_sweep/bitrate.rs"]
mod bitrate;
pub(crate) use bitrate::bitrate_sweep;
#[path = "resource_sweep/msr.rs"]
mod msr;
pub(crate) use msr::*;

#[path = "resource_sweep/branch_matrix.rs"]
mod branch_matrix;
#[cfg(test)]
pub(crate) use branch_matrix::selected_backend_policy_variants;
pub(crate) use branch_matrix::{backend_policy_matrix, branch_matrix, srt_crypto_matrix};
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
#[path = "resource_sweep/measurement.rs"]
mod measurement;
pub(super) use measurement::ffmpeg_children_stats;
pub(crate) use measurement::read_proc_status_kb_checked;
use measurement::{
    ResourceAggregate, ResourceScenarioMeta, csv_escape, read_proc_stat_ticks,
    resource_aggregate_json, sample_resource_window, write_resource_sweep_csv,
};

/// Live process stack shared by a resource-sweep sample.
///
/// `mediamtx` holds one `Child` per peer instance (`env.peer_count`, default
/// 1): mediamtx processes normally, or `restream --sink-mode` processes
/// when `env.peer_mode == ResourceSweepPeer::Sink`. Every non-`msr`
/// resource-sweep scenario runs with `peer_count == 1`, so this stays a
/// single-element Vec and existing behavior is unchanged.
struct ResourceSweepStack {
    mediamtx: Vec<Child>,
    restream: Child,
    api: RampApi,
    restream_pid: u32,
}

/// Stop every child in a peer-instance Vec (or any other child list),
/// mirroring the single-child `stop_child` used elsewhere in this module.
async fn stop_children(children: &mut [Child]) {
    for child in children {
        stop_child(child).await;
    }
}

pub(crate) async fn resource_sweep() -> Result<Value, String> {
    let env = ResourceSweepEnv::from_env()?;
    std::fs::create_dir_all(&env.work_dir).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(&env.summary_csv);
    let _ = std::fs::remove_file(&env.summary_json);
    let _ = std::fs::remove_file(&env.samples_jsonl);

    let mut stack = if env.lifecycle == ResourceSweepLifecycle::Isolated {
        None
    } else {
        Some(start_resource_sweep_stack(&env).await?)
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
    let result = json!({
        "mode": "resource-sweep",
        "lifecycle": env.lifecycle.as_str(),
        "artifacts": {
            "summaryJson": env.summary_json,
            "summaryCsv": env.summary_csv,
            "samplesJsonl": env.samples_jsonl,
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

/// Ports for peer instance `index` (0-based): each of `mtx_rtmp`/`mtx_rtmps`/
/// `mtx_srt`/`mtx_api` offset by `index`. Instance 0 always matches the
/// pre-existing single-mediamtx ports, so `peer_count == 1` is byte-identical
/// to prior behavior.
fn peer_instance_ports(env: &ResourceSweepEnv, index: usize) -> (u16, u16, u16, u16) {
    let offset = index as u16;
    (
        env.mtx_rtmp.wrapping_add(offset),
        env.mtx_rtmps.wrapping_add(offset),
        env.mtx_srt.wrapping_add(offset),
        env.mtx_api.wrapping_add(offset),
    )
}

/// The HTTP port for sink-peer instance `index`. Sink peers are full
/// `restream` processes (see `spawn_sink_peer`) and therefore need their own
/// HTTP/DB surface; this range sits well clear of every other harness port
/// range (`mtx_api + 2000 ..`) so it doesn't collide at any `PEER_COUNT` the
/// harness realistically runs (a handful of instances, not thousands).
fn sink_peer_http_port(env: &ResourceSweepEnv, index: usize) -> u16 {
    env.mtx_api
        .saturating_add(2000)
        .saturating_add(index as u16)
}

/// Suffix `path` with `-{index}` (before the extension) for `index > 0`,
/// leaving `index == 0` untouched so instance-0 artifact filenames stay
/// stable for existing tooling and single-instance runs.
fn instance_suffixed_path(path: &Path, index: usize) -> PathBuf {
    if index == 0 {
        return path.to_path_buf();
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("instance");
    let file_name = match path.extension().and_then(|ext| ext.to_str()) {
        Some(ext) => format!("{stem}-{index}.{ext}"),
        None => format!("{stem}-{index}"),
    };
    path.with_file_name(file_name)
}

async fn spawn_mediamtx_peer(env: &ResourceSweepEnv, index: usize) -> Result<Child, String> {
    let (rtmp, rtmps, srt, api) = peer_instance_ports(env, index);
    let log_path = instance_suffixed_path(&env.mediamtx_log, index);
    let config_path = instance_suffixed_path(&env.mediamtx_config, index);
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let err_log = log.try_clone().map_err(|e| e.to_string())?;
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
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nwriteQueueSize: 512\nrtmp: yes\nrtmpAddress: :{rtmp}\n{rtmp_tls_lines}rtsp: no\nsrt: yes\nsrtAddress: :{srt}\nhls: no\nwebrtc: no\nmoq: no\napi: yes\napiAddress: :{api}\nmetrics: no\npaths:\n  all:\n"
        ),
    )
    .map_err(|e| e.to_string())?;
    let mut cmd = Command::new("mediamtx");
    let mut child = remove_mediamtx_config_env(&mut cmd)
        .arg(&config_path)
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err_log))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{api}/v3/paths/list"),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut child).await;
        return Err(format!("mediamtx[{index}] did not become ready: {err}"));
    }
    Ok(child)
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

async fn spawn_sink_peer(env: &ResourceSweepEnv, index: usize) -> Result<Child, String> {
    if !env.restream_bin.exists() {
        return Err(format!(
            "restream binary not found at {}",
            env.restream_bin.display()
        ));
    }
    let (rtmp, _rtmps, srt, _api) = peer_instance_ports(env, index);
    let http = sink_peer_http_port(env, index);
    let log_dir = env.work_dir.join("logs");
    std::fs::create_dir_all(&log_dir).map_err(|e| e.to_string())?;
    let log_path = log_dir.join(format!("sink-{index}.log"));
    let log = std::fs::File::create(&log_path).map_err(|e| e.to_string())?;
    let err_log = log.try_clone().map_err(|e| e.to_string())?;
    let db_path = env.work_dir.join(format!("sink-{index}.db"));
    cleanup_ramp_db(&db_path);
    let mut cmd = Command::new(&env.restream_bin);
    cmd.env("RESTREAM_SINK_MODE", "1")
        .env("RESTREAM_HTTP_PORT", http.to_string())
        .env("RESTREAM_RTMP_PORT", rtmp.to_string())
        .env("RESTREAM_SRT_PORT", srt.to_string())
        .env("RESTREAM_INITIAL_ADMIN_PASSWORD", harness_admin_password())
        .env("RESTREAM_LOG_DIR", &log_dir)
        .env("RESTREAM_DB_PATH", db_path.to_string_lossy().to_string())
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(err_log))
        .kill_on_drop(true);
    let mut child = cmd.spawn().map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_tcp_listener_ready(rtmp, Duration::from_secs(30)).await {
        stop_child(&mut child).await;
        return Err(format!(
            "sink peer[{index}] RTMP listener did not become ready: {err}"
        ));
    }
    if let Err(err) = wait_for_udp_listener_ready(srt, Duration::from_secs(30)).await {
        stop_child(&mut child).await;
        return Err(format!(
            "sink peer[{index}] SRT listener did not become ready: {err}"
        ));
    }
    Ok(child)
}

async fn verify_preexisting_sink_peer(index: usize, rtmp: u16, srt: u16) -> Result<Child, String> {
    let mut dummy = Command::new("true")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("dummy: {e}"))?;
    if let Err(err) = wait_for_tcp_listener_ready(rtmp, Duration::from_secs(10)).await {
        let _ = dummy.kill().await;
        return Err(format!(
            "pre-started sink peer[{index}] RTMP listener not ready: {err}"
        ));
    }
    if let Err(err) = wait_for_udp_listener_ready(srt, Duration::from_secs(10)).await {
        let _ = dummy.kill().await;
        return Err(format!(
            "pre-started sink peer[{index}] SRT listener not ready: {err}"
        ));
    }
    Ok(dummy)
}

async fn start_resource_sweep_peers(env: &ResourceSweepEnv) -> Result<Vec<Child>, String> {
    let skip_start = std::env::var("PEER_SKIP_START").is_ok();
    let mut children = Vec::with_capacity(env.peer_count);
    for index in 0..env.peer_count {
        let spawned = match (env.peer_mode, skip_start) {
            (ResourceSweepPeer::Mediamtx, true) => {
                let (_, _, _, api) = peer_instance_ports(env, index);
                verify_preexisting_mediamtx_peer(index, api).await
            }
            (ResourceSweepPeer::Mediamtx, false) => spawn_mediamtx_peer(env, index).await,
            (ResourceSweepPeer::Sink, true) => {
                let (rtmp, _, srt, _) = peer_instance_ports(env, index);
                verify_preexisting_sink_peer(index, rtmp, srt).await
            }
            (ResourceSweepPeer::Sink, false) => spawn_sink_peer(env, index).await,
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

async fn start_resource_sweep_stack(env: &ResourceSweepEnv) -> Result<ResourceSweepStack, String> {
    if !env.restream_bin.exists() {
        return Err(format!(
            "restream binary not found at {}",
            env.restream_bin.display()
        ));
    }
    std::fs::create_dir_all(env.work_dir.join("logs")).map_err(|e| e.to_string())?;
    cleanup_ramp_db(&env.restream_db_path);
    let mut mediamtx = start_resource_sweep_peers(env).await?;

    let restream_log = std::fs::File::create(&env.restream_log).map_err(|e| e.to_string())?;
    let restream_err = restream_log.try_clone().map_err(|e| e.to_string())?;
    let mut restream_cmd = Command::new(&env.restream_bin);
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
    for (key, value) in &env.backend_policy_env {
        restream_cmd.env(key, value);
    }
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
        return Err(format!("restream did not become ready: {err}"));
    }
    let mut api = RampApi::new(env.restream_http);
    api.login().await?;
    let restream_pid = restream.id().ok_or("restream pid missing")?;
    Ok(ResourceSweepStack {
        mediamtx,
        restream,
        api,
        restream_pid,
    })
}

async fn ensure_resource_stack<'a>(
    env: &ResourceSweepEnv,
    stack: &'a mut Option<ResourceSweepStack>,
) -> Result<&'a mut ResourceSweepStack, String> {
    if stack.is_none() {
        *stack = Some(start_resource_sweep_stack(env).await?);
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
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
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
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
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
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
    };

    let mut publishers = Vec::new();
    let mut pipeline_ids = Vec::new();
    let max_ingests = *env.ingest_counts.iter().max().unwrap_or(&1);
    let mut out = Vec::new();
    for index in 1..=max_ingests {
        let config = if mixed {
            sweep_configs()[index - 1]
        } else {
            sweep_configs()[1]
        };
        let stream_key = format!("resource-growth-{index}-{}", config.name);
        let pipeline_id = create_resource_pipeline(
            &active.api,
            &format!("{}-{index}", config.name),
            &stream_key,
        )
        .await?;
        let publisher = spawn_resource_publisher(env, config, &stream_key)?;
        wait_for_api_input_live(&active.api, &pipeline_id, Duration::from_secs(45)).await?;
        publishers.push(publisher);
        pipeline_ids.push(pipeline_id);
        if env.ingest_counts.contains(&index) {
            let ingest_types = if mixed {
                sweep_configs()
                    .iter()
                    .take(index)
                    .map(|cfg| cfg.name)
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                "h264-srt".to_string()
            };
            out.push(
                sample_resource_window(
                    env,
                    active,
                    ResourceScenarioMeta {
                        scenario: if mixed {
                            "ingest-growth-mixed"
                        } else {
                            "ingest-growth-same"
                        },
                        label: format!("{index}-pipelines"),
                        pipelines: index,
                        outputs: 0,
                        ingest_types,
                        egress_mix: "none".to_string(),
                        transcode: "none",
                    },
                )
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
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_children(&mut local_stack.as_mut().unwrap().mediamtx).await;
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
    let mut local_stack = if local_only {
        Some(start_resource_sweep_stack(env).await?)
    } else {
        None
    };
    let active = if local_only {
        local_stack.as_mut().unwrap()
    } else {
        ensure_resource_stack(env, stack).await?
    };
    let stream_key = format!("resource-{scenario_name}");
    let pipeline_id = create_resource_pipeline(&active.api, scenario_name, &stream_key).await?;
    let mut publisher = spawn_resource_publisher(env, config, &stream_key)?;
    wait_for_api_input_live(&active.api, &pipeline_id, Duration::from_secs(45)).await?;
    let mut output_ids = Vec::new();
    let max_outputs = *env.egress_counts.iter().max().unwrap_or(&1);
    let mut out = Vec::new();
    for index in 1..=max_outputs {
        for kind in output_kinds {
            let name = format!("{scenario_name}-{}-{index}", kind.label());
            let (url, encoding) = resource_output_url(env, config, *kind, &name);
            let output_id = create_output_with_rtmp_mode(
                &active.api,
                &pipeline_id,
                &name,
                &url,
                &encoding,
                kind.rtmp_mode(),
            )
            .await?;
            start_output(&active.api, &pipeline_id, &output_id).await?;
            output_ids.push(output_id);
        }
        if env.egress_counts.contains(&index) {
            let progress_timeout = resource_output_progress_timeout(output_ids.len());
            wait_for_outputs_progress(&active.api, &pipeline_id, &output_ids, progress_timeout)
                .await?;
            out.push(
                sample_resource_window(
                    env,
                    active,
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
        }
    }
    if env.lifecycle == ResourceSweepLifecycle::Cumulative {
        retained_publishers.push(publisher);
    } else {
        stop_child(&mut publisher).await;
        delete_resource_pipeline(&active.api, &pipeline_id).await;
    }
    if local_only {
        stop_child(&mut local_stack.as_mut().unwrap().restream).await;
        stop_children(&mut local_stack.as_mut().unwrap().mediamtx).await;
    }
    Ok(out)
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
        "1.5M",
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

fn resource_output_url(
    env: &ResourceSweepEnv,
    config: SweepConfig,
    kind: SweepOutputKind,
    name: &str,
) -> (String, String) {
    (
        kind.publish_url(env.mtx_rtmp, env.mtx_rtmps, env.mtx_srt, name),
        kind.encoding(config.multi_audio).to_string(),
    )
}

fn resource_output_progress_timeout(output_count: usize) -> Duration {
    let base_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_BASE_SECS", 30);
    let per_output_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_PER_OUTPUT_SECS", 4);
    let cap_secs = env_secs("RESOURCE_SWEEP_PROGRESS_TIMEOUT_CAP_SECS", 240);
    scaled_output_progress_timeout(output_count, base_secs, per_output_secs, cap_secs)
}

pub(crate) fn scaled_output_progress_timeout(
    output_count: usize,
    base_secs: u64,
    per_output_secs: u64,
    cap_secs: u64,
) -> Duration {
    let cap_secs = cap_secs.max(base_secs);
    let extra_outputs = output_count.saturating_sub(1) as u64;
    let scaled_secs = base_secs.saturating_add(extra_outputs.saturating_mul(per_output_secs));
    Duration::from_secs(scaled_secs.min(cap_secs))
}

#[cfg(test)]
mod tests {
    use super::*;

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
            sample_secs: 1,
            sample_interval_ms: 1000,
            settle_secs: 1,
            ingest_counts: Vec::new(),
            egress_counts: Vec::new(),
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
    fn sink_peer_http_port_is_well_clear_of_mtx_api() {
        let env = test_env();
        assert_eq!(sink_peer_http_port(&env, 0), 11997);
        assert_eq!(sink_peer_http_port(&env, 3), 12000);
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
}
