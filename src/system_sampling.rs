//! Dependency-light host and process resource sampling.
//!
//! This module owns operating-system reads and sampling history. Callers receive
//! typed snapshots and remain responsible for transport- or UI-specific shaping.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

use sysinfo::System;

#[cfg(target_os = "linux")]
use crate::media::host_capacity::parse_cpu_list_count;
use crate::media::host_capacity::proc_status_value;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NofileLimitSnapshot {
    Available {
        soft: u64,
        hard: u64,
    },
    #[cfg(not(unix))]
    Unsupported,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CpuAllowedListSnapshot {
    pub raw: String,
    pub count: Option<u64>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct CgroupCpuMaxSnapshot {
    pub raw: String,
    pub cpus: Option<f64>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct CpuCapacitySnapshot {
    pub available_parallelism: Option<u64>,
    pub allowed_list: Option<CpuAllowedListSnapshot>,
    pub cgroup_max: Option<CgroupCpuMaxSnapshot>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct HostSettingsSnapshot {
    pub nofile: NofileLimitSnapshot,
    pub receive_buffer_max: Option<u64>,
    pub send_buffer_max: Option<u64>,
    pub cpu_capacity: CpuCapacitySnapshot,
}

#[derive(Clone, Debug, Default)]
pub(crate) struct ProcessResourceSnapshot {
    pub cpu_percent: f64,
    pub restream_cpu_percent: f64,
    pub external_ffmpeg_cpu_percent: f64,
    pub cpu_sample_ready: bool,
    pub restream_memory_bytes: u64,
    pub external_ffmpeg_memory_bytes: u64,
    pub total_memory_bytes: u64,
    pub external_ffmpeg_count: u64,
    pub process_thread_count: u64,
    pub fd_count: Option<u64>,
}

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct ChildProcessResourceSnapshot {
    pub cpu_percent: Option<f64>,
    pub memory_bytes: Option<u64>,
}

#[derive(Clone, Copy, Debug)]
struct CpuSample {
    total_ticks: u64,
    process_ticks: u64,
}

#[derive(Clone, Copy, Debug)]
struct EngineCpuSample {
    total_ticks: u64,
    restream_ticks: u64,
    external_ffmpeg_ticks: u64,
}

#[derive(Clone, Copy, Debug, Default)]
struct EngineCpuUsageSnapshot {
    total_percent: f64,
    restream_percent: f64,
    external_ffmpeg_percent: f64,
    ready: bool,
}

static CHILD_PROCESS_CPU_SAMPLES: OnceLock<Mutex<HashMap<u32, CpuSample>>> = OnceLock::new();
static ENGINE_CPU_SAMPLE: OnceLock<Mutex<Option<EngineCpuSample>>> = OnceLock::new();
static HOST_SETTINGS_CACHE: OnceLock<Mutex<Option<(Instant, HostSettingsSnapshot)>>> =
    OnceLock::new();
const HOST_SETTINGS_CACHE_TTL: Duration = Duration::from_secs(15);

pub(crate) fn sample_host_settings() -> HostSettingsSnapshot {
    let cache = HOST_SETTINGS_CACHE.get_or_init(|| Mutex::new(None));
    let mut guard = restream_sync::lock::lock(cache);
    if let Some((instant, snapshot)) = guard.as_ref()
        && instant.elapsed() < HOST_SETTINGS_CACHE_TTL
    {
        return snapshot.clone();
    }
    let fresh = sample_host_settings_uncached();
    *guard = Some((Instant::now(), fresh.clone()));
    fresh
}

fn sample_host_settings_uncached() -> HostSettingsSnapshot {
    HostSettingsSnapshot {
        nofile: sample_nofile_limit(),
        receive_buffer_max: proc_sys_u64("net.core.rmem_max"),
        send_buffer_max: proc_sys_u64("net.core.wmem_max"),
        cpu_capacity: sample_cpu_capacity(),
    }
}

#[cfg(unix)]
fn sample_nofile_limit() -> NofileLimitSnapshot {
    // getrlimit(RLIMIT_NOFILE) cannot fail for the calling process; `None`
    // is RLIM_INFINITY.
    let limit = rustix::process::getrlimit(rustix::process::Resource::Nofile);
    NofileLimitSnapshot::Available {
        soft: limit.current.unwrap_or(u64::MAX),
        hard: limit.maximum.unwrap_or(u64::MAX),
    }
}

#[cfg(not(unix))]
fn sample_nofile_limit() -> NofileLimitSnapshot {
    NofileLimitSnapshot::Unsupported
}

fn proc_sys_u64(key: &str) -> Option<u64> {
    let path = format!("/proc/sys/{}", key.replace('.', "/"));
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

fn available_parallelism_u64() -> Option<u64> {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .ok()
        .and_then(|cpus| u64::try_from(cpus).ok())
}

#[cfg(target_os = "linux")]
#[cfg(target_os = "linux")]
fn parse_cgroup_cpu_max(value: &str) -> CgroupCpuMaxSnapshot {
    let mut parts = value.split_whitespace();
    let quota = parts.next().unwrap_or("max");
    let period = parts.next().unwrap_or("");
    let cpus = if quota == "max" {
        None
    } else {
        match (quota.parse::<f64>().ok(), period.parse::<f64>().ok()) {
            (Some(quota), Some(period)) if period > 0.0 => Some(quota / period),
            _ => None,
        }
    };
    CgroupCpuMaxSnapshot {
        raw: value.to_string(),
        cpus,
    }
}

#[cfg(target_os = "linux")]
fn read_cgroup_cpu_max() -> Option<CgroupCpuMaxSnapshot> {
    let cgroup = std::fs::read_to_string("/proc/self/cgroup").ok()?;
    let unified_path = cgroup.lines().find_map(|line| {
        let mut parts = line.splitn(3, ':');
        let hierarchy = parts.next()?;
        let controllers = parts.next()?;
        let path = parts.next()?;
        (hierarchy == "0" && controllers.is_empty()).then_some(path)
    })?;
    let relative = unified_path.trim_start_matches('/');
    let cpu_max_path = std::path::Path::new("/sys/fs/cgroup")
        .join(relative)
        .join("cpu.max");
    let value = std::fs::read_to_string(cpu_max_path).ok()?;
    Some(parse_cgroup_cpu_max(value.trim()))
}

fn sample_cpu_capacity() -> CpuCapacitySnapshot {
    let mut snapshot = CpuCapacitySnapshot {
        available_parallelism: available_parallelism_u64(),
        ..CpuCapacitySnapshot::default()
    };
    #[cfg(target_os = "linux")]
    {
        let status = std::fs::read_to_string("/proc/self/status").ok();
        snapshot.allowed_list = status.as_deref().and_then(|status| {
            let raw = proc_status_value(status, "Cpus_allowed_list")?;
            Some(CpuAllowedListSnapshot {
                count: parse_cpu_list_count(&raw),
                raw,
            })
        });
        snapshot.cgroup_max = read_cgroup_cpu_max();
    }
    snapshot
}

pub(crate) fn proc_total_ticks() -> Option<u64> {
    let stat = std::fs::read_to_string("/proc/stat").ok()?;
    let cpu = stat.lines().find(|line| line.starts_with("cpu "))?;
    Some(
        cpu.split_whitespace()
            .skip(1)
            .filter_map(|value| value.parse::<u64>().ok())
            .sum(),
    )
}

pub(crate) fn proc_process_ticks(pid: u32) -> Option<u64> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let end_comm = stat.rfind(')')?;
    let fields: Vec<&str> = stat[end_comm + 2..].split_whitespace().collect();
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    Some(utime + stime)
}

fn proc_rss_bytes(pid: u32) -> Option<u64> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    let rss_kib = status.lines().find_map(|line| {
        let value = line.strip_prefix("VmRSS:")?.trim();
        value
            .split_whitespace()
            .next()
            .and_then(|number| number.parse::<u64>().ok())
    })?;
    Some(rss_kib.saturating_mul(1024))
}

fn proc_self_thread_count() -> u64 {
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return 0;
    };
    status
        .lines()
        .find_map(|line| {
            let value = line.strip_prefix("Threads:")?.trim();
            value.parse::<u64>().ok()
        })
        .unwrap_or(0)
}

fn proc_self_fd_count() -> Option<u64> {
    std::fs::read_dir("/proc/self/fd")
        .ok()
        .map(|entries| entries.filter_map(Result::ok).count() as u64)
}

pub(crate) fn sample_child_process_resources(
    pids: impl IntoIterator<Item = u32>,
) -> HashMap<u32, ChildProcessResourceSnapshot> {
    let pids = pids.into_iter().collect::<Vec<_>>();
    if pids.is_empty() {
        return HashMap::new();
    }
    let total_ticks = proc_total_ticks();
    let core_count = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1);
    let sample_store = CHILD_PROCESS_CPU_SAMPLES.get_or_init(|| Mutex::new(HashMap::new()));
    let mut previous = restream_sync::lock::lock(sample_store);
    let mut resources = HashMap::new();
    for pid in pids {
        let process_ticks = proc_process_ticks(pid);
        let cpu_percent = total_ticks.zip(process_ticks).and_then(|(total, ticks)| {
            let sample = CpuSample {
                total_ticks: total,
                process_ticks: ticks,
            };
            let cpu = previous.get(&pid).and_then(|prev| {
                let total_delta = sample.total_ticks.saturating_sub(prev.total_ticks);
                if total_delta == 0 {
                    return None;
                }
                let process_delta = sample.process_ticks.saturating_sub(prev.process_ticks);
                let scale = core_count.max(1) as f64 * 100.0 / total_delta as f64;
                Some(process_delta as f64 * scale)
            });
            previous.insert(pid, sample);
            cpu
        });
        resources.insert(
            pid,
            ChildProcessResourceSnapshot {
                cpu_percent,
                memory_bytes: proc_rss_bytes(pid),
            },
        );
    }
    resources
}

/// One long-lived host sampler shared by every API handler. Each handler
/// used to build `System::new_all()` + `refresh_all()` per request: every
/// process on the host with command lines, environments and per-thread task
/// lists, from a Tokio worker. The capacity harness requests
/// `/metrics/system` on every tick, and in its RTMP fan-out profile Tokio was
/// 43.7% of Restream samples, dominated by procfs reads. A fresh `System` also
/// cannot report host CPU usage (there is no previous sample to diff), so the
/// value was wrong. Now CPU usage is a delta between successive calls, and
/// only Restream and its child processes are refreshed.
static HOST_SAMPLER: std::sync::LazyLock<std::sync::Mutex<System>> =
    std::sync::LazyLock::new(|| {
        std::sync::Mutex::new(System::new_with_specifics(
            sysinfo::RefreshKind::nothing()
                .with_cpu(sysinfo::CpuRefreshKind::nothing().with_cpu_usage())
                .with_memory(sysinfo::MemoryRefreshKind::everything()),
        ))
    });

/// Refresh host CPU usage and memory plus Restream and its children only,
/// then run `read` on the refreshed view. `read` must not block: the sampler
/// is shared.
pub(crate) fn sampled_system<R>(read: impl FnOnce(&System) -> R) -> R {
    let mut sys = restream_sync::lock::lock(&HOST_SAMPLER);
    sys.refresh_cpu_usage();
    sys.refresh_memory();
    let own = sysinfo::Pid::from_u32(std::process::id());
    let refresh = sysinfo::ProcessRefreshKind::nothing().with_memory();
    match own_child_pids() {
        Some(children) => {
            let mut pids: Vec<sysinfo::Pid> =
                children.into_iter().map(sysinfo::Pid::from_u32).collect();
            pids.push(own);
            // Children that exited since the last sample are dropped: the
            // previous set is refreshed too, with dead ones removed.
            let known: Vec<sysinfo::Pid> = sys.processes().keys().copied().collect();
            pids.extend(known);
            pids.sort_unstable();
            pids.dedup();
            sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::Some(&pids), true, refresh);
        }
        // No `/proc/<pid>/task/<tid>/children` (kernel without
        // CONFIG_PROC_CHILDREN): fall back to the process list, still without
        // command lines, environments or task lists.
        None => {
            sys.refresh_processes_specifics(sysinfo::ProcessesToUpdate::All, true, refresh);
        }
    }
    read(&sys)
}

/// Direct children of this process, from each thread's procfs `children`
/// list (children are recorded against the thread that forked them).
fn own_child_pids() -> Option<Vec<u32>> {
    let tasks = std::fs::read_dir("/proc/self/task").ok()?;
    let mut children = Vec::new();
    let mut readable = false;
    for task in tasks.flatten() {
        let Ok(list) = std::fs::read_to_string(task.path().join("children")) else {
            continue;
        };
        readable = true;
        children.extend(
            list.split_whitespace()
                .filter_map(|pid| pid.parse::<u32>().ok()),
        );
    }
    readable.then_some(children)
}

pub(crate) fn engine_process_pids(sys: &System) -> Vec<u32> {
    let own_pid = std::process::id();
    let own_sys_pid = sysinfo::Pid::from_u32(own_pid);
    let mut pids = vec![own_pid];
    for (pid, process) in sys.processes() {
        let name = process.name().to_string_lossy().to_ascii_lowercase();
        if process.parent() == Some(own_sys_pid) && name.contains("ffmpeg") {
            pids.push(pid.as_u32());
        }
    }
    pids.sort_unstable();
    pids.dedup();
    pids
}

fn sample_engine_cpu_usage(
    restream_pid: u32,
    external_ffmpeg_pids: &[u32],
    core_count: usize,
) -> EngineCpuUsageSnapshot {
    let restream_ticks = proc_process_ticks(restream_pid).unwrap_or(0);
    let external_ffmpeg_ticks = external_ffmpeg_pids.iter().fold(0u64, |total, pid| {
        total.saturating_add(proc_process_ticks(*pid).unwrap_or(0))
    });
    let Some(total_ticks) = proc_total_ticks() else {
        return EngineCpuUsageSnapshot::default();
    };
    let sample = EngineCpuSample {
        total_ticks,
        restream_ticks,
        external_ffmpeg_ticks,
    };
    let lock = ENGINE_CPU_SAMPLE.get_or_init(|| Mutex::new(None));
    let mut previous = restream_sync::lock::lock(lock);
    let Some(previous_sample) = *previous else {
        *previous = Some(sample);
        return EngineCpuUsageSnapshot::default();
    };
    *previous = Some(sample);
    let total_delta = sample
        .total_ticks
        .saturating_sub(previous_sample.total_ticks);
    if total_delta == 0 {
        return EngineCpuUsageSnapshot {
            ready: true,
            ..EngineCpuUsageSnapshot::default()
        };
    }
    let scale = core_count.max(1) as f64 * 100.0 / total_delta as f64;
    let restream_percent = sample
        .restream_ticks
        .saturating_sub(previous_sample.restream_ticks) as f64
        * scale;
    let external_ffmpeg_percent = sample
        .external_ffmpeg_ticks
        .saturating_sub(previous_sample.external_ffmpeg_ticks)
        as f64
        * scale;
    EngineCpuUsageSnapshot {
        total_percent: restream_percent + external_ffmpeg_percent,
        restream_percent,
        external_ffmpeg_percent,
        ready: true,
    }
}

pub(crate) fn sample_process_resources(sys: &System, core_count: usize) -> ProcessResourceSnapshot {
    let own_pid = std::process::id();
    let own_sys_pid = sysinfo::Pid::from_u32(own_pid);
    let pids = engine_process_pids(sys);
    let restream_memory_bytes = sys
        .process(own_sys_pid)
        .map(|process| process.memory())
        .unwrap_or(0);
    let mut external_ffmpeg_pids = Vec::new();
    let mut external_ffmpeg_memory_bytes = 0u64;
    let mut total_memory_bytes = 0u64;
    for pid in &pids {
        let Some(process) = sys.process(sysinfo::Pid::from_u32(*pid)) else {
            continue;
        };
        let memory = process.memory();
        total_memory_bytes = total_memory_bytes.saturating_add(memory);
        if *pid != own_pid
            && process
                .name()
                .to_string_lossy()
                .to_ascii_lowercase()
                .contains("ffmpeg")
        {
            external_ffmpeg_pids.push(*pid);
            external_ffmpeg_memory_bytes = external_ffmpeg_memory_bytes.saturating_add(memory);
        }
    }
    let cpu = sample_engine_cpu_usage(own_pid, &external_ffmpeg_pids, core_count);
    ProcessResourceSnapshot {
        cpu_percent: cpu.total_percent,
        restream_cpu_percent: cpu.restream_percent,
        external_ffmpeg_cpu_percent: cpu.external_ffmpeg_percent,
        cpu_sample_ready: cpu.ready,
        restream_memory_bytes,
        external_ffmpeg_memory_bytes,
        total_memory_bytes,
        external_ffmpeg_count: external_ffmpeg_pids.len() as u64,
        process_thread_count: proc_self_thread_count(),
        fd_count: proc_self_fd_count(),
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::parse_cgroup_cpu_max;

    #[test]
    fn cgroup_cpu_max_reports_unlimited_and_fractional_quota() {
        assert_eq!(
            parse_cgroup_cpu_max("max 100000"),
            super::CgroupCpuMaxSnapshot {
                raw: "max 100000".to_string(),
                cpus: None,
            }
        );
        assert_eq!(
            parse_cgroup_cpu_max("250000 100000"),
            super::CgroupCpuMaxSnapshot {
                raw: "250000 100000".to_string(),
                cpus: Some(2.5),
            }
        );
    }
}

#[cfg(test)]
mod sampler_tests {
    use super::*;

    #[test]
    #[cfg(target_os = "linux")]
    fn sampler_sees_restream_and_its_spawned_children() {
        let mut child = std::process::Command::new("sleep")
            .arg("5")
            .spawn()
            .expect("spawn a child process");
        let child_pid = child.id();
        let children = own_child_pids().expect("procfs children lists are readable");
        assert!(
            children.contains(&child_pid),
            "{child_pid} not in {children:?}"
        );
        let (own, child_seen) = sampled_system(|sys| {
            (
                sys.process(sysinfo::Pid::from_u32(std::process::id()))
                    .is_some(),
                sys.process(sysinfo::Pid::from_u32(child_pid)).is_some(),
            )
        });
        assert!(
            own && child_seen,
            "the sampler refreshes Restream and its children"
        );
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn host_cpu_usage_is_a_percentage_across_successive_samples() {
        let _ = sampled_system(|sys| sys.global_cpu_usage());
        std::thread::sleep(sysinfo::MINIMUM_CPU_UPDATE_INTERVAL);
        let usage = sampled_system(|sys| sys.global_cpu_usage());
        assert!((0.0..=100.0).contains(&usage), "{usage}");
    }
}
