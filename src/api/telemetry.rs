use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode, header},
    response::IntoResponse,
};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::{Path as FsPath, PathBuf};
use std::sync::{Arc, Mutex};
use sysinfo::{Disks, Networks, System};
use tokio::io::AsyncReadExt;
use tracing::{error, warn};

use crate::alerts;
use crate::application::ingest::load_pipeline_file_ingest_state;
use crate::application::ports::SqliteIngestLookup;
use crate::diag;
use crate::events;

use super::health::{
    build_health_snapshot_for_pipeline_ids, build_health_summary_snapshot_for_pipeline_ids,
    list_dashboard_runtime_pipeline_ids, merge_dashboard_runtime_focus_pipeline,
    select_dashboard_runtime_pipeline_ids,
};
use super::state::{
    AppState, ENGINE_CPU_SAMPLE, EngineCpuSample, MAX_URL_LEN, check_field_len,
    get_session_token_from_headers, recording_enabled_map, require_authenticated,
};

pub fn default_events_limit() -> usize {
    100
}

#[derive(Debug, Deserialize)]
pub struct EventsQuery {
    pub pipeline_id: Option<String>,
    #[serde(default = "default_events_limit")]
    pub limit: usize,
}

#[derive(Deserialize)]
pub struct DashboardRuntimeQuery {
    pub health_view: Option<String>,
    pub metrics_view: Option<String>,
    pub pipeline_id: Option<String>,
}

#[derive(Deserialize)]
pub struct MetricsSystemQuery {
    pub view: Option<String>,
}

#[derive(Deserialize)]
pub struct DiagnosticsQuery {
    pub probe: Option<String>,
    #[allow(dead_code)]
    pub publisher: Option<String>,
    #[allow(dead_code)]
    pub since: Option<String>,
}

pub fn expected_media_path(media_dir: &str, filename: &str) -> PathBuf {
    let configured = PathBuf::from(media_dir);
    let root = if configured.is_absolute() {
        configured
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(configured)
    };
    root.join(filename)
}

pub async fn build_file_diagnostics_context(
    state: &AppState,
    pipeline_id: &str,
) -> Option<diag::FileDiagnosticsContext> {
    let pipeline = state.pipeline_service.get_by_id(pipeline_id).await.ok()?;
    let ingest = load_pipeline_file_ingest_state(
        &SqliteIngestLookup::new(state.db.clone()),
        &state.engine,
        &pipeline,
    )
    .await
    .ok()?
    .ingest?;
    let path = expected_media_path(&state.media_dir, &ingest.filename);
    let metadata = std::fs::metadata(&path).ok();
    let file_exists = metadata.is_some();
    let file_size_bytes = metadata.as_ref().map(std::fs::Metadata::len);
    let file_modified_at = metadata
        .as_ref()
        .and_then(|meta| meta.modified().ok())
        .map(|timestamp| chrono::DateTime::<chrono::Utc>::from(timestamp).to_rfc3339());

    let (analysis, analysis_error) = if file_exists {
        let path_for_task = path.clone();
        match tokio::task::spawn_blocking(move || {
            crate::media::file_analysis::analyze_media_file(&path_for_task)
        })
        .await
        {
            Ok(Ok(analysis)) => (Some(analysis), None),
            Ok(Err(error)) => (None, Some(error)),
            Err(error) => (None, Some(format!("analysis task failed: {error}"))),
        }
    } else {
        (None, None)
    };

    Some(diag::FileDiagnosticsContext {
        ingest_id: ingest.id,
        filename: ingest.filename,
        path,
        file_exists,
        file_size_bytes,
        file_modified_at,
        loop_enabled: ingest.loop_flag,
        start_time: ingest.start_time,
        live_optimized: ingest.live_optimized,
        target_gop_seconds: ingest.target_gop_seconds,
        analysis,
        analysis_error,
    })
}

pub async fn pipeline_diagnostics_sse_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    axum::extract::Query(query): axum::extract::Query<DiagnosticsQuery>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let probe_protocol = match state
        .engine
        .active_ingest_protocol_for_probe(&pipeline_id)
        .await
    {
        Some(protocol) => protocol,
        None => {
            return (StatusCode::NOT_FOUND, "No active ingest for this pipeline").into_response();
        }
    };

    if let Some(requested_protocol) = query.probe
        && requested_protocol != probe_protocol
    {
        return (
            StatusCode::BAD_REQUEST,
            format!(
                "Probe protocol must match active ingest protocol ({})",
                probe_protocol
            ),
        )
            .into_response();
    }
    let engine = state.engine.clone();
    let file_context = if probe_protocol == "file" {
        build_file_diagnostics_context(&state, &pipeline_id).await
    } else {
        None
    };

    let sem = engine.get_or_create_diag_semaphore(&pipeline_id).await;
    let permit = match sem.clone().try_acquire_owned() {
        Ok(p) => p,
        Err(_) => {
            return (
                StatusCode::TOO_MANY_REQUESTS,
                "A diagnostic is already running for this pipeline",
            )
                .into_response();
        }
    };

    let (tx, rx) = tokio::sync::mpsc::channel::<String>(32);
    tokio::spawn(async move {
        let _permit = permit;
        diag::run_diagnostics(
            engine,
            pipeline_id,
            probe_protocol,
            state.media_dir.clone(),
            file_context,
            tx,
        )
        .await;
    });

    let stream = tokio_stream::wrappers::ReceiverStream::new(rx);
    let body = axum::body::Body::from_stream(futures_util::StreamExt::map(stream, |s| {
        Ok::<_, std::convert::Infallible>(s)
    }));

    (
        StatusCode::OK,
        [
            (header::CONTENT_TYPE, "text/event-stream"),
            (header::CACHE_CONTROL, "no-cache"),
            (header::HeaderName::from_static("x-accel-buffering"), "no"),
        ],
        body,
    )
        .into_response()
}

pub async fn status_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let sys = System::new_all();
    let bonding_available = state.engine.bonding_available();
    let (mut status, _) = crate::runtime_info::status_and_sbom(bonding_available);
    status["os"] = system_status(&sys);

    Json(status).into_response()
}

pub async fn build_system_metrics_snapshot(state: &AppState, summary: bool) -> serde_json::Value {
    let mut sys = System::new_all();
    sys.refresh_all();

    let cpu_pct = sys.global_cpu_info().cpu_usage() as f64;
    let total_mem = sys.total_memory();
    let used_mem = sys.used_memory();
    let free_mem = total_mem.saturating_sub(used_mem);
    let mem_pct = if total_mem > 0 {
        (used_mem as f64 / total_mem as f64) * 100.0
    } else {
        0.0
    };
    let core_count = sys.cpus().len();
    let load_avg = System::load_average();
    let engine = engine_metrics(&sys, core_count);

    let media_root = {
        let configured = FsPath::new(&state.media_dir);
        let absolute = if configured.is_absolute() {
            configured.to_path_buf()
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(configured)
        };
        std::fs::canonicalize(&absolute).unwrap_or(absolute)
    };
    let disks = Disks::new_with_refreshed_list();

    fn disk_usage_for_path(disks: &Disks, path: &FsPath) -> Option<(u64, u64, String)> {
        disks
            .iter()
            .filter_map(|disk| {
                let mount = disk.mount_point();
                path.starts_with(mount)
                    .then_some((disk, mount.components().count()))
            })
            .max_by_key(|(_, depth)| *depth)
            .map(|(disk, _)| {
                let total = disk.total_space();
                let used = total.saturating_sub(disk.available_space());
                (total, used, disk.mount_point().display().to_string())
            })
    }

    let system_root = {
        #[cfg(unix)]
        {
            PathBuf::from("/")
        }
        #[cfg(not(unix))]
        {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        }
    };
    let (total_disk, used_disk, disk_mount) =
        if let Some((total, used, mount)) = disk_usage_for_path(&disks, &system_root) {
            (total, used, Some(mount))
        } else {
            let (total, used) = disks.iter().fold((0u64, 0u64), |(t, u), d| {
                (
                    t + d.total_space(),
                    u + (d.total_space() - d.available_space()),
                )
            });
            (total, used, None)
        };
    let free_disk = total_disk.saturating_sub(used_disk);
    let disk_pct = if total_disk > 0 {
        (used_disk as f64 / total_disk as f64) * 100.0
    } else {
        0.0
    };
    let media_disk = disk_usage_for_path(&disks, &media_root).map(|(total, used, mount)| {
        let free = total.saturating_sub(used);
        let used_pct = if total > 0 {
            (used as f64 / total as f64) * 100.0
        } else {
            0.0
        };
        serde_json::json!({
            "totalBytes": total,
            "usedBytes": used,
            "freeBytes": free,
            "usedPercent": used_pct,
            "scope": "mediaDir",
            "mountPoint": mount,
            "mediaDir": state.media_dir,
            "mediaRoot": media_root.display().to_string()
        })
    });

    fn is_external_interface(name: &str) -> bool {
        let lower = name.to_ascii_lowercase();
        if lower == "lo" || lower.starts_with("lo:") {
            return false;
        }
        let virtual_prefixes = [
            "docker",
            "br-",
            "veth",
            "virbr",
            "vmnet",
            "zt",
            "tailscale",
            "tun",
            "tap",
            "wg",
        ];
        !virtual_prefixes
            .iter()
            .any(|prefix| lower.starts_with(prefix))
    }

    let nets1 = Networks::new_with_refreshed_list();
    tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
    let nets2 = Networks::new_with_refreshed_list();
    let mut total_rx = 0u64;
    let mut total_tx = 0u64;
    let mut external_interfaces = Vec::new();
    let mut ignored_interfaces = Vec::new();
    for (iface, n2) in nets2.iter() {
        if let Some(n1) = nets1.get(iface) {
            let rx = n2.total_received().saturating_sub(n1.total_received());
            let tx = n2
                .total_transmitted()
                .saturating_sub(n1.total_transmitted());
            let active = rx > 0 || tx > 0;
            if is_external_interface(iface) {
                total_rx += rx;
                total_tx += tx;
                if active {
                    external_interfaces.push(serde_json::json!({
                        "name": iface,
                        "downloadBytesPerSec": rx * 4,
                        "uploadBytesPerSec": tx * 4,
                        "downloadKbps": (rx * 4 * 8) as f64 / 1000.0,
                        "uploadKbps": (tx * 4 * 8) as f64 / 1000.0,
                    }));
                }
            } else if active {
                ignored_interfaces.push(iface.to_string());
            }
        }
    }
    let dl_bytes_sec = total_rx * 4;
    let ul_bytes_sec = total_tx * 4;
    let dl_kbps = (dl_bytes_sec * 8) as f64 / 1000.0;
    let ul_kbps = (ul_bytes_sec * 8) as f64 / 1000.0;

    let now = chrono::Utc::now().to_rfc3339();
    if summary {
        serde_json::json!({
            "generatedAt": now,
            "cpu": {
                "usagePercent": cpu_pct,
            },
            "memory": {
                "usedPercent": mem_pct
            },
            "engine": engine,
            "disk": {
                "usedPercent": disk_pct,
            },
            "network": {
                "downloadKbps": dl_kbps,
                "uploadKbps": ul_kbps,
            }
        })
    } else {
        serde_json::json!({
            "generatedAt": now,
            "cpu": {
                "usagePercent": cpu_pct,
                "cores": core_count,
                "load1": load_avg.one
            },
            "memory": {
                "totalBytes": total_mem,
                "usedBytes": used_mem,
                "freeBytes": free_mem,
                "usedPercent": mem_pct
            },
            "engine": engine,
            "disk": {
                "totalBytes": total_disk,
                "usedBytes": used_disk,
                "freeBytes": free_disk,
                "usedPercent": disk_pct,
                "scope": "systemRoot",
                "mountPoint": disk_mount,
                "root": system_root.display().to_string()
            },
            "mediaDisk": media_disk,
            "network": {
                "scope": "external",
                "downloadBytesPerSec": dl_bytes_sec,
                "uploadBytesPerSec": ul_bytes_sec,
                "downloadKbps": dl_kbps,
                "uploadKbps": ul_kbps,
                "interfaces": external_interfaces,
                "ignoredInterfaces": ignored_interfaces,
                "sampleMs": 250
            }
        })
    }
}

pub async fn v1_dashboard_runtime_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<DashboardRuntimeQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let summary_health = query.health_view.as_deref() == Some("summary");
    let summary_metrics = query.metrics_view.as_deref() == Some("summary");
    let all_pipeline_ids = list_dashboard_runtime_pipeline_ids(&state).await;
    let requested_pipeline_id = query.pipeline_id.as_deref().filter(|pipeline_id| {
        all_pipeline_ids
            .iter()
            .any(|candidate| candidate == *pipeline_id)
    });
    let health_pipeline_ids = select_dashboard_runtime_pipeline_ids(
        requested_pipeline_id,
        summary_health,
        all_pipeline_ids.clone(),
    );
    let (health, metrics) = tokio::join!(
        async {
            if summary_health {
                let mut health =
                    build_health_summary_snapshot_for_pipeline_ids(&state, &health_pipeline_ids)
                        .await;
                if let Some(pipeline_id) = requested_pipeline_id {
                    let focused_health =
                        build_health_snapshot_for_pipeline_ids(&state, &[pipeline_id.to_string()])
                            .await;
                    merge_dashboard_runtime_focus_pipeline(
                        &mut health,
                        &focused_health,
                        pipeline_id,
                    );
                }
                health
            } else {
                build_health_snapshot_for_pipeline_ids(&state, &health_pipeline_ids).await
            }
        },
        build_system_metrics_snapshot(&state, summary_metrics)
    );

    Json(serde_json::json!({
        "health": health,
        "metrics": metrics,
    }))
    .into_response()
}

pub fn system_status(sys: &System) -> serde_json::Value {
    serde_json::json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "hostname": System::host_name().unwrap_or_default(),
        "kernelVersion": System::kernel_version(),
        "uptime": System::uptime(),
        "totalMem": sys.total_memory(),
        "cpu": cpu_status(sys),
    })
}

pub fn cpu_status(sys: &System) -> serde_json::Value {
    let cpuinfo = read_cpuinfo_summary();
    let first_cpu = sys.cpus().first();
    let logical_cpus = sys.cpus().len();
    let physical_cores = sys.physical_core_count();
    let threads_per_core = physical_cores
        .filter(|cores| *cores > 0)
        .map(|cores| logical_cpus as f64 / cores as f64);
    let flags = cpuinfo
        .get("flags")
        .map(|value| selected_cpu_flags(value))
        .unwrap_or_default();
    let hypervisor_detected = flags.iter().any(|flag| flag == "hypervisor");
    let virtualization = if flags.iter().any(|flag| flag == "vmx") {
        Some("VT-x")
    } else if flags.iter().any(|flag| flag == "svm") {
        Some("AMD-V")
    } else {
        None
    };

    serde_json::json!({
        "modelName": cpuinfo
            .get("model name")
            .or_else(|| cpuinfo.get("hardware"))
            .or_else(|| cpuinfo.get("processor"))
            .cloned()
            .or_else(|| first_cpu.map(|cpu| cpu.brand().to_string())),
        "logicalCpus": logical_cpus,
        "physicalCores": physical_cores,
        "threadsPerCore": threads_per_core,
        "virtualization": virtualization,
        "hypervisorDetected": hypervisor_detected,
        "hypervisorVendor": if hypervisor_detected { detect_hypervisor_vendor() } else { None },
        "flags": flags,
    })
}

pub fn read_cpuinfo_summary() -> HashMap<String, String> {
    let mut summary = HashMap::new();
    let Ok(text) = std::fs::read_to_string("/proc/cpuinfo") else {
        return summary;
    };
    for line in text.lines() {
        if line.trim().is_empty() && !summary.is_empty() {
            break;
        }
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        summary
            .entry(key.trim().to_ascii_lowercase())
            .or_insert_with(|| value.trim().to_string());
    }
    summary
}

pub fn selected_cpu_flags(flags: &str) -> Vec<String> {
    const USEFUL_FLAGS: &[&str] = &[
        "sse4_1",
        "sse4_2",
        "avx",
        "avx2",
        "avx512f",
        "avx_vnni",
        "fma",
        "aes",
        "sha_ni",
        "vaes",
        "vpclmulqdq",
        "bmi1",
        "bmi2",
        "vmx",
        "svm",
        "hypervisor",
    ];
    let available = flags
        .split_whitespace()
        .collect::<std::collections::BTreeSet<_>>();
    USEFUL_FLAGS
        .iter()
        .filter(|flag| available.contains(**flag))
        .map(|flag| (*flag).to_string())
        .collect()
}

pub fn detect_hypervisor_vendor() -> Option<String> {
    for path in [
        "/sys/hypervisor/type",
        "/sys/class/dmi/id/sys_vendor",
        "/sys/class/dmi/id/product_name",
    ] {
        let Some(value) = read_trimmed_file(path) else {
            continue;
        };
        let lower = value.to_ascii_lowercase();
        if lower.contains("microsoft") {
            return Some("Microsoft".to_string());
        }
        if lower.contains("vmware") {
            return Some("VMware".to_string());
        }
        if lower.contains("kvm") || lower.contains("qemu") {
            return Some("KVM/QEMU".to_string());
        }
        if lower.contains("virtualbox") {
            return Some("VirtualBox".to_string());
        }
    }
    None
}

pub fn read_trimmed_file(path: impl AsRef<FsPath>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub async fn status_sbom_get_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let bonding_available = state.engine.bonding_available();
    let (_, sbom) = crate::runtime_info::status_and_sbom(bonding_available);
    (
        [
            (
                header::CONTENT_TYPE,
                "application/vnd.cyclonedx+json; version=1.5",
            ),
            (
                header::CONTENT_DISPOSITION,
                "attachment; filename=\"restream-sbom.cdx.json\"",
            ),
        ],
        Json(sbom),
    )
        .into_response()
}

pub fn proc_total_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let cpu = stat.lines().find(|line| line.starts_with("cpu "))?;
    Some(
        cpu.split_whitespace()
            .skip(1)
            .filter_map(|value| value.parse::<u64>().ok())
            .sum(),
    )
}

pub fn proc_process_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let end_comm = stat.rfind(')')?;
    let fields: Vec<&str> = stat[end_comm + 2..].split_whitespace().collect();
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some(utime + stime)
}

pub fn engine_process_pids(sys: &System) -> Vec<u32> {
    let own_pid = std::process::id();
    let own_sys_pid = sysinfo::Pid::from_u32(own_pid);
    let mut pids = vec![own_pid];

    for (pid, process) in sys.processes() {
        let name = process.name().to_ascii_lowercase();
        if process.parent() == Some(own_sys_pid) && name.contains("ffmpeg") {
            pids.push(pid.as_u32());
        }
    }

    pids.sort_unstable();
    pids.dedup();
    pids
}

pub fn engine_metrics(sys: &System, core_count: usize) -> serde_json::Value {
    let own_pid = std::process::id();
    let own_sys_pid = sysinfo::Pid::from_u32(own_pid);
    let pids = engine_process_pids(sys);

    let restream_memory = sys
        .process(own_sys_pid)
        .map(|process| process.memory())
        .unwrap_or(0);
    let mut external_ffmpeg_count = 0u64;
    let mut external_ffmpeg_memory = 0u64;
    let mut total_memory = 0u64;
    let mut external_ffmpeg_ticks = 0u64;

    for pid in &pids {
        if let Some(process) = sys.process(sysinfo::Pid::from_u32(*pid)) {
            let memory = process.memory();
            total_memory = total_memory.saturating_add(memory);
            if *pid != own_pid && process.name().to_ascii_lowercase().contains("ffmpeg") {
                external_ffmpeg_count += 1;
                external_ffmpeg_memory = external_ffmpeg_memory.saturating_add(memory);
                external_ffmpeg_ticks =
                    external_ffmpeg_ticks.saturating_add(proc_process_ticks(*pid).unwrap_or(0));
            }
        }
    }

    let restream_ticks = proc_process_ticks(own_pid).unwrap_or(0);
    let total_ticks = proc_total_ticks();
    let mut cpu_sample_ready = false;
    let (cpu_percent, restream_cpu_percent, external_ffmpeg_cpu_percent) = total_ticks
        .and_then(|total_ticks| {
            let sample = EngineCpuSample {
                total_ticks,
                restream_ticks,
                external_ffmpeg_ticks,
            };
            let lock = ENGINE_CPU_SAMPLE.get_or_init(|| Mutex::new(None));
            let mut previous = lock.lock().ok()?;
            let cpu = previous.map(|prev| {
                cpu_sample_ready = true;
                let total_delta = sample.total_ticks.saturating_sub(prev.total_ticks);
                if total_delta == 0 {
                    return (0.0, 0.0, 0.0);
                }
                let scale = core_count.max(1) as f64 * 100.0 / total_delta as f64;
                let restream_delta = sample.restream_ticks.saturating_sub(prev.restream_ticks);
                let ffmpeg_delta = sample
                    .external_ffmpeg_ticks
                    .saturating_sub(prev.external_ffmpeg_ticks);
                let restream_cpu = restream_delta as f64 * scale;
                let ffmpeg_cpu = ffmpeg_delta as f64 * scale;
                (restream_cpu + ffmpeg_cpu, restream_cpu, ffmpeg_cpu)
            });
            *previous = Some(sample);
            cpu
        })
        .unwrap_or((0.0, 0.0, 0.0));

    serde_json::json!({
        "cpuPercent": cpu_percent,
        "cpuSampleReady": cpu_sample_ready,
        "restreamCpuPercent": restream_cpu_percent,
        "externalFfmpegCpuPercent": external_ffmpeg_cpu_percent,
        "memoryBytes": restream_memory,
        "restreamMemoryBytes": restream_memory,
        "totalMemoryBytes": total_memory,
        "externalFfmpegCount": external_ffmpeg_count,
        "externalFfmpegMemoryBytes": external_ffmpeg_memory,
    })
}

pub async fn metrics_system_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<MetricsSystemQuery>,
) -> impl IntoResponse {
    if let Some(token) = get_session_token_from_headers(&headers) {
        if !state.is_authenticated(&token).await {
            return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
        }
    } else {
        return (StatusCode::UNAUTHORIZED, "Unauthorized").into_response();
    }

    let response =
        build_system_metrics_snapshot(&state, query.view.as_deref() == Some("summary")).await;
    Json(response).into_response()
}

pub async fn v1_events_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
    axum::extract::Query(query): axum::extract::Query<EventsQuery>,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let limit = query.limit.min(events::MAX_EVENTS);
    let pipeline_filter = query.pipeline_id.as_deref();
    let event_list = state.engine.recent_events(limit, pipeline_filter);

    Json(serde_json::json!({
        "generatedAt": chrono::Utc::now().to_rfc3339(),
        "count": event_list.len(),
        "events": event_list,
    }))
    .into_response()
}

pub async fn v1_overview_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }

    let pipelines = state
        .pipeline_service
        .list_pipelines()
        .await
        .unwrap_or_default();
    let pipeline_ids: Vec<String> = pipelines.iter().map(|p| p.id.clone()).collect();
    let recording_enabled = recording_enabled_map(&state, &pipeline_ids).await;
    let snapshot = crate::api_runtime_views::health_snapshot(
        &state.engine,
        &pipeline_ids,
        &recording_enabled,
        0,
    )
    .await;

    let alert_list = alerts::derive_alerts(&snapshot);
    let critical = alert_list
        .iter()
        .filter(|a| matches!(a.severity, alerts::Severity::Critical))
        .count();
    let warning = alert_list
        .iter()
        .filter(|a| matches!(a.severity, alerts::Severity::Warning))
        .count();

    let snap_pipelines = snapshot["pipelines"].as_object();

    let total = pipeline_ids.len();
    let mut active = 0usize;
    let mut degraded = 0usize;
    let mut failed_outputs = 0usize;

    if let Some(pip_map) = snap_pipelines {
        for (pip_id, pip) in pip_map {
            let is_live = pip["input"]["status"].as_str() == Some("on");
            if is_live {
                active += 1;
            }
            let has_alerts = alert_list
                .iter()
                .any(|a| a.pipeline_id.as_deref() == Some(pip_id.as_str()));
            if has_alerts {
                degraded += 1;
            }
            if is_live && let Some(outputs) = pip["outputs"].as_object() {
                for output in outputs.values() {
                    if output["status"].as_str().unwrap_or("") != "running" {
                        failed_outputs += 1;
                    }
                }
            }
        }
    }

    let generated_at = snapshot["generatedAt"].as_str().unwrap_or("").to_string();

    Json(serde_json::json!({
        "generatedAt": generated_at,
        "totalPipelines": total,
        "activePipelines": active,
        "degradedPipelines": degraded,
        "failedOutputs": failed_outputs,
        "alertCount": { "critical": critical, "warning": warning },
        "srtListener": snapshot["srtListener"],
    }))
    .into_response()
}

pub async fn v1_engine_telemetry_handler(
    State(state): State<Arc<AppState>>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    Json(crate::api_runtime_views::engine_telemetry(&state.engine).await).into_response()
}

pub async fn v1_pipeline_telemetry_handler(
    State(state): State<Arc<AppState>>,
    Path(pipeline_id): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    Json(crate::api_runtime_views::pipeline_telemetry(&state.engine, &pipeline_id).await)
        .into_response()
}

pub async fn v1_stage_telemetry_handler(
    State(state): State<Arc<AppState>>,
    Path(stage_key): Path<String>,
    headers: HeaderMap,
) -> impl IntoResponse {
    if let Some(response) = require_authenticated(&state, &headers).await {
        return response;
    }
    match crate::api_runtime_views::stage_telemetry_by_display(&state.engine, &stage_key).await {
        Some(val) => Json(val).into_response(),
        None => (StatusCode::NOT_FOUND, "Stage not found").into_response(),
    }
}
