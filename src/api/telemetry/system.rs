use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use sysinfo::{Disks, Networks, System};

use crate::api::state::AppState;
use crate::host::system_sampling::{ProcessResourceSnapshot, sample_process_resources};
use restream_engine::egress::backends::hls_put_shard::HLS_PUT_TLS_COUNTERS;
use restream_engine::egress::backends::rtmp::RTMPS_TLS_COUNTERS;
use restream_engine::egress::tls::TlsTelemetrySnapshot;
use restream_engine::uring_capabilities::UringCapabilities;

use super::configured_media_root;

/// Builds the dashboard/system metrics payload, with a summary mode that keeps
/// only the fields needed by compact operator surfaces.
pub async fn build_system_metrics_snapshot(state: &AppState, summary: bool) -> serde_json::Value {
    let (cpu_pct, total_mem, used_mem, core_count, engine) =
        crate::host::system_sampling::sampled_system(|sys| {
            let core_count = sys.cpus().len();
            (
                sys.global_cpu_usage() as f64,
                sys.total_memory(),
                sys.used_memory(),
                core_count,
                engine_metrics(sys, core_count),
            )
        })
        .unwrap_or_default();
    let free_mem = total_mem.saturating_sub(used_mem);
    let mem_pct = used_percent(total_mem, used_mem);
    let load1 = System::load_average().map_or(0.0, |load| load.one);
    let capacity = state.capacity_snapshot().await;
    let egress_shards = state
        .egress_shard_statuses(std::time::Duration::from_secs(5))
        .await;
    let host_capacity = state.host_capacity_json(egress_shards.len());
    let io_uring = uring_capabilities();
    let rtmps = RTMPS_TLS_COUNTERS.snapshot();
    let hls_put_tls = HLS_PUT_TLS_COUNTERS.snapshot();

    // Disk listing and path resolution are blocking syscalls. A host that
    // will not list its disks reports none.
    let media_dir = state.media_dir.clone();
    let (system_disk, media_disk) = tokio::task::spawn_blocking(move || {
        let disks = Disks::new_with_refreshed_list().ok();
        let disks: &[sysinfo::Disk] = disks.as_deref().unwrap_or_default();
        (system_disk_usage(disks), media_disk_json(&media_dir, disks))
    })
    .await
    .unwrap_or_else(|_| (system_disk_usage(&[]), None));
    let free_disk = system_disk.total.saturating_sub(system_disk.used);
    let disk_pct = used_percent(system_disk.total, system_disk.used);

    let network = sample_external_network().await;
    let dl_bytes_sec = network.rx_bytes_per_sec;
    let ul_bytes_sec = network.tx_bytes_per_sec;
    let dl_kbps = (dl_bytes_sec * 8) as f64 / 1000.0;
    let ul_kbps = (ul_bytes_sec * 8) as f64 / 1000.0;

    let now = restream_domain::timestamp::now_rfc3339();
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
            "capacity": capacity,
            "hostCapacity": host_capacity,
            "mediaExecutor": restream_engine::executor::snapshot(false),
            "egressShards": egress_shards,
            "ioUring": io_uring,
            "rtmps": tls_counters_json(&rtmps),
            "hlsPutTls": tls_counters_json(&hls_put_tls),
            "disk": {
                "usedPercent": disk_pct,
            },
            // The Media library's storage card reads the summary too.
            "mediaDisk": media_disk,
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
                "load1": load1
            },
            "memory": {
                "totalBytes": total_mem,
                "usedBytes": used_mem,
                "freeBytes": free_mem,
                "usedPercent": mem_pct
            },
            "engine": engine,
            "capacity": capacity,
            "hostCapacity": host_capacity,
            "mediaExecutor": restream_engine::executor::snapshot(true),
            "egressShards": egress_shards,
            "ioUring": io_uring,
            "rtmps": tls_counters_json(&rtmps),
            "hlsPutTls": tls_counters_json(&hls_put_tls),
            "disk": {
                "totalBytes": system_disk.total,
                "usedBytes": system_disk.used,
                "freeBytes": free_disk,
                "usedPercent": disk_pct,
                "scope": "systemRoot",
                "mountPoint": system_disk.mount,
                "root": system_disk.root.display().to_string()
            },
            "mediaDisk": media_disk,
            "network": {
                "scope": "external",
                "downloadBytesPerSec": dl_bytes_sec,
                "uploadBytesPerSec": ul_bytes_sec,
                "downloadKbps": dl_kbps,
                "uploadKbps": ul_kbps,
                "interfaces": network.interfaces,
                "ignoredInterfaces": network.ignored,
                "sampleMs": 250
            }
        })
    }
}

fn used_percent(total: u64, used: u64) -> f64 {
    if total > 0 {
        (used as f64 / total as f64) * 100.0
    } else {
        0.0
    }
}

/// Usage of the disk holding the system root, or of every disk summed when
/// none holds it.
struct SystemDiskUsage {
    root: PathBuf,
    total: u64,
    used: u64,
    mount: Option<String>,
}

fn system_disk_usage(disks: &[sysinfo::Disk]) -> SystemDiskUsage {
    let root = {
        #[cfg(unix)]
        {
            PathBuf::from("/")
        }
        #[cfg(not(unix))]
        {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
        }
    };
    let (total, used, mount) = if let Some((total, used, mount)) = disk_usage_for_path(disks, &root)
    {
        (total, used, Some(mount))
    } else {
        let (total, used) = disks.iter().fold((0u64, 0u64), |(t, u), disk| {
            (
                t + disk.total_space(),
                u + (disk.total_space() - disk.available_space()),
            )
        });
        (total, used, None)
    };
    SystemDiskUsage {
        root,
        total,
        used,
        mount,
    }
}

/// Usage of the disk holding the media directory, if one does.
fn media_disk_json(media_dir: &str, disks: &[sysinfo::Disk]) -> Option<serde_json::Value> {
    let media_root = {
        let absolute = configured_media_root(media_dir);
        std::fs::canonicalize(&absolute).unwrap_or(absolute)
    };
    disk_usage_for_path(disks, &media_root).map(|(total, used, mount)| {
        serde_json::json!({
            "totalBytes": total,
            "usedBytes": used,
            "freeBytes": total.saturating_sub(used),
            "usedPercent": used_percent(total, used),
            "scope": "mediaDir",
            "mountPoint": mount,
            "mediaDir": media_dir,
            "mediaRoot": media_root.display().to_string()
        })
    })
}

/// External interfaces' traffic over a 250 ms sample, per second.
struct NetworkSample {
    rx_bytes_per_sec: u64,
    tx_bytes_per_sec: u64,
    interfaces: Vec<serde_json::Value>,
    /// Active interfaces left out as not external.
    ignored: Vec<String>,
}

async fn sample_external_network() -> NetworkSample {
    let nets1 = Networks::new_with_refreshed_list().ok();
    tokio::time::sleep(tokio::time::Duration::from_millis(250)).await;
    let nets2 = Networks::new_with_refreshed_list().ok();
    let mut sample = NetworkSample {
        rx_bytes_per_sec: 0,
        tx_bytes_per_sec: 0,
        interfaces: Vec::new(),
        ignored: Vec::new(),
    };
    // Rates need both samples; a host that will not list its interfaces
    // reports none.
    let samples = nets1.as_ref().zip(nets2.as_ref());
    for (iface, n2) in samples.into_iter().flat_map(|(_, nets2)| nets2.iter()) {
        let Some(n1) = samples.and_then(|(nets1, _)| nets1.get(iface)) else {
            continue;
        };
        let rx = n2.total_received().saturating_sub(n1.total_received());
        let tx = n2
            .total_transmitted()
            .saturating_sub(n1.total_transmitted());
        let active = rx > 0 || tx > 0;
        let iface = iface.to_string_lossy();
        if is_external_interface(&iface) {
            sample.rx_bytes_per_sec += rx * 4;
            sample.tx_bytes_per_sec += tx * 4;
            if active {
                sample.interfaces.push(serde_json::json!({
                    "name": iface,
                    "downloadBytesPerSec": rx * 4,
                    "uploadBytesPerSec": tx * 4,
                    "downloadKbps": (rx * 4 * 8) as f64 / 1000.0,
                    "uploadKbps": (tx * 4 * 8) as f64 / 1000.0,
                }));
            }
        } else if active {
            sample.ignored.push(iface.to_string());
        }
    }
    sample
}

/// One TLS egress transport's process-lifetime counters (RTMPS, HLS PUT).
fn tls_counters_json(tls: &TlsTelemetrySnapshot) -> serde_json::Value {
    serde_json::json!({
        "connections": tls.connections,
        "tls12": tls.tls12,
        "tls13": tls.tls13,
        "ktlsRequested": tls.ktls_requested,
        "ktlsAttempts": tls.ktls_attempts,
        "ktlsSuccess": tls.ktls_success,
        "ktlsUnsupported": tls.ktls_unsupported,
        "ktlsError": tls.ktls_error,
        "userspaceTlsConnections": tls.userspace_tls_connections,
        "ktlsCapabilities": {
            "tls12Aes128Gcm": tls.ktls_tls12_aes128_gcm,
            "tls12Aes256Gcm": tls.ktls_tls12_aes256_gcm,
            "tls13Aes128Gcm": tls.ktls_tls13_aes128_gcm,
            "tls13Aes256Gcm": tls.ktls_tls13_aes256_gcm,
        },
    })
}

fn uring_capabilities() -> &'static serde_json::Value {
    static CAPABILITIES: OnceLock<serde_json::Value> = OnceLock::new();
    CAPABILITIES.get_or_init(|| match UringCapabilities::probe(8) {
        Ok(caps) => serde_json::json!({
            "available": true,
            "pollAdd": caps.poll_add,
            "accept": caps.accept,
            "acceptMultishot": caps.accept_multishot,
            "connect": caps.connect,
            "recv": caps.recv,
            "recvMsg": caps.recv_msg,
            "recvMsgMulti": caps.recv_msg_multi,
            "recvMultishot": caps.recv_multishot,
            "provideBuffers": caps.provide_buffers,
            "recvBundle": caps.recv_bundle,
            "recvZc": caps.recv_zc,
            "send": caps.send,
            "sendMsg": caps.send_msg,
            "sendmsgIovec": caps.sendmsg_iovec,
            "sendZc": caps.send_zc,
            "sendBundle": caps.send_bundle,
            "napi": caps.napi,
            "deploymentTier": caps.deployment_tier().as_str(),
        }),
        Err(error) => serde_json::json!({
            "available": false,
            "error": error.kind().to_string(),
        }),
    })
}

fn disk_usage_for_path(disks: &[sysinfo::Disk], path: &Path) -> Option<(u64, u64, String)> {
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

pub fn system_status(sys: &System) -> serde_json::Value {
    serde_json::json!({
        "platform": std::env::consts::OS,
        "arch": std::env::consts::ARCH,
        "hostname": System::host_name().unwrap_or_default(),
        "kernelVersion": System::kernel_version().ok(),
        "uptime": System::uptime().unwrap_or(0),
        "totalMem": sys.total_memory(),
        "cpu": cpu_status(sys),
    })
}

pub fn cpu_status(sys: &System) -> serde_json::Value {
    let cpuinfo = read_cpuinfo_summary();
    let first_cpu = sys.cpus().first();
    let logical_cpus = sys.cpus().len();
    let physical_cores = System::physical_core_count().ok();
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

pub fn read_trimmed_file(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn proc_total_ticks() -> Option<u64> {
    crate::host::system_sampling::proc_total_ticks()
}

pub fn proc_process_ticks(pid: u32) -> Option<u64> {
    crate::host::system_sampling::proc_process_ticks(pid)
}

pub fn engine_process_pids(sys: &System) -> Vec<u32> {
    crate::host::system_sampling::engine_process_pids(sys)
}

pub fn engine_metrics(sys: &System, core_count: usize) -> serde_json::Value {
    let snapshot = sample_process_resources(sys, core_count);
    serde_json::json!({
        "cpuPercent": snapshot.cpu_percent,
        "cpuSampleReady": snapshot.cpu_sample_ready,
        "restreamCpuPercent": snapshot.restream_cpu_percent,
        "externalFfmpegCpuPercent": snapshot.external_ffmpeg_cpu_percent,
        "memoryBytes": snapshot.restream_memory_bytes,
        "restreamMemoryBytes": snapshot.restream_memory_bytes,
        "totalMemoryBytes": snapshot.total_memory_bytes,
        "externalFfmpegCount": snapshot.external_ffmpeg_count,
        "externalFfmpegMemoryBytes": snapshot.external_ffmpeg_memory_bytes,
    })
}

pub(crate) fn process_resource_snapshot(sys: &System) -> ProcessResourceSnapshot {
    sample_process_resources(sys, sys.cpus().len())
}
