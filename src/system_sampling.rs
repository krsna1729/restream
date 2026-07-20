//! Dependency-light host and process resource sampling.
//!
//! This module owns operating-system reads and sampling history. Callers receive
//! typed snapshots and remain responsible for transport- or UI-specific shaping.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use sysinfo::System;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum NofileLimitSnapshot {
    Available {
        soft: u64,
        hard: u64,
    },
    ReadFailed,
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

pub(crate) fn sample_host_settings() -> HostSettingsSnapshot {
    HostSettingsSnapshot {
        nofile: sample_nofile_limit(),
        receive_buffer_max: proc_sys_u64("net.core.rmem_max"),
        send_buffer_max: proc_sys_u64("net.core.wmem_max"),
        cpu_capacity: sample_cpu_capacity(),
    }
}

#[cfg(unix)]
fn sample_nofile_limit() -> NofileLimitSnapshot {
    let mut limit = libc::rlimit {
        rlim_cur: 0,
        rlim_max: 0,
    };
    // SAFETY: getrlimit writes to the provided rlimit struct for the current
    // process. The pointer is valid for the duration of the call.
    if unsafe { libc::getrlimit(libc::RLIMIT_NOFILE, &mut limit) } == 0 {
        NofileLimitSnapshot::Available {
            soft: limit.rlim_cur,
            hard: limit.rlim_max,
        }
    } else {
        NofileLimitSnapshot::ReadFailed
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
fn proc_status_value(status: &str, key: &str) -> Option<String> {
    status.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == key).then(|| value.trim().to_string())
    })
}

#[cfg(target_os = "linux")]
fn parse_cpu_list_count(list: &str) -> Option<u64> {
    let mut count = 0u64;
    for part in list
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
    {
        let Some((start, end)) = part.split_once('-') else {
            part.parse::<u64>().ok()?;
            count = count.checked_add(1)?;
            continue;
        };
        let start = start.trim().parse::<u64>().ok()?;
        let end = end.trim().parse::<u64>().ok()?;
        if end < start {
            return None;
        }
        let range_len = (end - start).checked_add(1)?;
        count = count.checked_add(range_len)?;
    }
    Some(count)
}

#[cfg(target_os = "linux")]
fn parse_cpu_allowed_list_count(status: &str) -> Option<usize> {
    let value = proc_status_value(status, "Cpus_allowed_list")?;
    parse_cpu_list_count(&value)
        .and_then(|count| usize::try_from(count).ok())
        .filter(|count| *count > 0)
}

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

#[cfg(target_os = "linux")]
fn parse_cpu_max_quota(value: &str) -> Option<usize> {
    let mut parts = value.split_whitespace();
    let quota = parts.next()?;
    let period = parts.next()?.parse::<usize>().ok()?;
    if quota == "max" || period == 0 {
        return None;
    }
    let quota = quota.parse::<usize>().ok()?;
    Some(quota.div_ceil(period).max(1))
}

/// Samples the limits used during runtime construction.
///
/// The root `cpu.max` lookup intentionally preserves the historical startup
/// behavior. The operational snapshot above resolves the process's nested
/// cgroup so it can report the actual runtime placement without silently
/// changing Tokio worker sizing.
pub(crate) fn effective_cpu_count() -> usize {
    let mut cpus = std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
        .max(1);
    #[cfg(target_os = "linux")]
    {
        let mask_count = std::fs::read_to_string("/proc/self/status")
            .ok()
            .and_then(|status| parse_cpu_allowed_list_count(&status));
        if let Some(mask_count) = mask_count {
            cpus = cpus.min(mask_count.max(1));
        }
        let quota_count = std::fs::read_to_string("/sys/fs/cgroup/cpu.max")
            .ok()
            .and_then(|cpu_max| parse_cpu_max_quota(&cpu_max));
        if let Some(quota_count) = quota_count {
            cpus = cpus.min(quota_count.max(1));
        }
    }
    cpus.max(1)
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
    let mut previous = match sample_store.lock() {
        Ok(lock) => lock,
        Err(_) => return HashMap::new(),
    };
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
    let Ok(mut previous) = lock.lock() else {
        return EngineCpuUsageSnapshot::default();
    };
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
    use super::{
        parse_cgroup_cpu_max, parse_cpu_allowed_list_count, parse_cpu_list_count,
        parse_cpu_max_quota,
    };

    #[test]
    fn cpu_list_count_handles_ranges_singletons_and_empty_input() {
        assert_eq!(parse_cpu_list_count("0-3"), Some(4));
        assert_eq!(parse_cpu_list_count("0-1,4,7-9"), Some(6));
        assert_eq!(parse_cpu_list_count(" 2 , 5-6 "), Some(3));
        assert_eq!(parse_cpu_list_count(""), Some(0));
        assert_eq!(parse_cpu_list_count("5-5"), Some(1));
    }

    #[test]
    fn cpu_list_count_rejects_invalid_or_overflowing_ranges() {
        assert_eq!(parse_cpu_list_count("4-2"), None);
        assert_eq!(parse_cpu_list_count("0,nope"), None);
        assert_eq!(parse_cpu_list_count("0-18446744073709551615"), None);
    }

    #[test]
    fn cpu_allowed_list_extracts_the_status_field_and_rejects_empty_masks() {
        assert_eq!(
            parse_cpu_allowed_list_count("Name:\trestream\nCpus_allowed_list:\t0-1,4\n"),
            Some(3)
        );
        assert_eq!(parse_cpu_allowed_list_count("Cpus_allowed_list:\t\n"), None);
    }

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

    #[test]
    fn startup_cpu_quota_rounds_up_and_treats_max_as_unlimited() {
        assert_eq!(parse_cpu_max_quota("max 100000"), None);
        assert_eq!(parse_cpu_max_quota("100000 100000"), Some(1));
        assert_eq!(parse_cpu_max_quota("150000 100000"), Some(2));
        assert_eq!(parse_cpu_max_quota("250000 100000"), Some(3));
    }
}
