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
    wait_for_input_state, wait_for_outputs_progress,
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
pub(crate) use branch_matrix::{
    backend_policy_matrix, branch_matrix, rtmp_fabric_matrix, srt_crypto_matrix, srt_fabric_matrix,
};
#[path = "resource_sweep/catalog.rs"]
mod catalog;
pub(crate) use catalog::{
    ResourceEgressScenario, SweepOutputKind, resource_egress_scenario, resource_egress_scenarios,
};
#[path = "resource_sweep/config.rs"]
mod config;
pub(crate) use config::SweepConfig;
use config::{
    ResourceSweepEnv, ResourceSweepLifecycle, parse_string_set, parse_sweep_configs,
    parse_usize_list, sweep_configs,
};
#[path = "resource_sweep/measurement.rs"]
mod measurement;
pub(super) use measurement::ffmpeg_children_stats;
use measurement::{
    ResourceAggregate, ResourceScenarioMeta, csv_escape, read_proc_stat_ticks,
    read_proc_status_kb_checked, resource_aggregate_json, sample_resource_window,
    write_resource_sweep_csv,
};

/// Live process stack shared by a resource-sweep sample.
struct ResourceSweepStack {
    mediamtx: Child,
    restream: Child,
    api: RampApi,
    restream_pid: u32,
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
            stop_child(&mut stack.mediamtx).await;
        }
    }
    Ok(result)
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
    let mediamtx_log = std::fs::File::create(&env.mediamtx_log).map_err(|e| e.to_string())?;
    let mediamtx_err = mediamtx_log.try_clone().map_err(|e| e.to_string())?;
    std::fs::write(
        &env.mediamtx_config,
        format!(
            "logLevel: warn\nreadTimeout: 30s\nwriteTimeout: 30s\nwriteQueueSize: 512\nrtmp: yes\nrtmpAddress: :{}\nrtmpEncryption: \"no\"\nrtsp: no\nsrt: yes\nsrtAddress: :{}\nhls: no\nwebrtc: no\nmoq: no\napi: yes\napiAddress: :{}\nmetrics: no\npaths:\n  all:\n",
            env.mtx_rtmp, env.mtx_srt, env.mtx_api
        ),
    )
    .map_err(|e| e.to_string())?;
    let mut mediamtx_command = Command::new("mediamtx");
    let mut mediamtx = remove_mediamtx_config_env(&mut mediamtx_command)
        .arg(&env.mediamtx_config)
        .stdout(Stdio::from(mediamtx_log))
        .stderr(Stdio::from(mediamtx_err))
        .kill_on_drop(true)
        .spawn()
        .map_err(|e| e.to_string())?;
    if let Err(err) = wait_for_http_ok(
        &format!("http://127.0.0.1:{}/v3/paths/list", env.mtx_api),
        Duration::from_secs(30),
    )
    .await
    {
        stop_child(&mut mediamtx).await;
        return Err(format!("mediamtx did not become ready: {err}"));
    }

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
        stop_child(&mut mediamtx).await;
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
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
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
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
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
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
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
        stop_child(&mut local_stack.as_mut().unwrap().mediamtx).await;
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
        kind.publish_url(env.mtx_rtmp, env.mtx_srt, name),
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
