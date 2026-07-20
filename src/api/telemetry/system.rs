use std::collections::HashMap;
use std::path::{Path, PathBuf};

use sysinfo::{Disks, Networks, System};

use crate::api::state::AppState;
use crate::system_sampling::{ProcessResourceSnapshot, sample_process_resources};

use super::configured_media_root;

/// Builds the dashboard/system metrics payload, with a summary mode that keeps
/// only the fields needed by compact operator surfaces.
pub async fn build_system_metrics_snapshot(state: &AppState, summary: bool) -> serde_json::Value {
    let mut sys = System::new_all();
    sys.refresh_all();

    let cpu_pct = sys.global_cpu_usage() as f64;
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
        let absolute = configured_media_root(&state.media_dir);
        std::fs::canonicalize(&absolute).unwrap_or(absolute)
    };
    let disks = Disks::new_with_refreshed_list();

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
            let (total, used) = disks.iter().fold((0u64, 0u64), |(t, u), disk| {
                (
                    t + disk.total_space(),
                    u + (disk.total_space() - disk.available_space()),
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

fn disk_usage_for_path(disks: &Disks, path: &Path) -> Option<(u64, u64, String)> {
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
    let physical_cores = System::physical_core_count();
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
    crate::system_sampling::proc_total_ticks()
}

pub fn proc_process_ticks(pid: u32) -> Option<u64> {
    crate::system_sampling::proc_process_ticks(pid)
}

pub fn engine_process_pids(sys: &System) -> Vec<u32> {
    crate::system_sampling::engine_process_pids(sys)
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
