//! CPU accounting and thread placement: `/proc/<pid>/stat` tick parsing,
//! tick deltas, per-unit ratios, `/proc` thread-`comm` discovery of the egress
//! shard threads, and `sched_setaffinity` set/read-back.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::super::*;

/// `comm` is `PROC_COMM_LEN - 1` bytes on Linux; `egress-shard-{n}` fits up to
/// `n = 99`, and a longer index arrives truncated.
pub(super) const COMM_MAX_BYTES: usize = 15;

/// The production egress shard thread name prefix (`egress-{ShardId}`, and
/// `ShardId` renders as `shard-{n}`).
pub(super) const EGRESS_SHARD_PREFIX: &str = "egress-shard-";

/// Pin a thread set to an exact CPU set (`pid == 0` = the calling thread), and
/// report the mask the kernel actually holds afterwards.
pub(super) fn set_affinity_mask(pid: u32, cpus: &BTreeSet<u32>) -> Result<Vec<u32>, String> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::CPU_ZERO(&mut set);
    }
    for cpu in cpus {
        unsafe { libc::CPU_SET(*cpu as usize, &mut set) };
    }
    let rc = unsafe {
        libc::sched_setaffinity(
            pid as libc::pid_t,
            std::mem::size_of::<libc::cpu_set_t>(),
            &set,
        )
    };
    if rc != 0 {
        return Err(format!(
            "sched_setaffinity({pid}, {cpus:?}): {}",
            std::io::Error::last_os_error()
        ));
    }
    observe_affinity(pid)
}

/// One `/proc/<pid>/task/<tid>/comm` reading, already trimmed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ThreadComm {
    pub(super) tid: u32,
    pub(super) comm: String,
}

/// An `egress-shard-<n>` thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct EgressShardThread {
    pub(super) tid: u32,
    pub(super) index: u32,
    /// `comm` is at the kernel's 15-byte limit, so `index` may be a prefix of a
    /// longer shard index (`egress-shard-120` and `egress-shard-12` are
    /// indistinguishable once truncated).
    pub(super) comm_truncated: bool,
}

impl EgressShardThread {
    pub(super) fn json(&self) -> Value {
        json!({
            "tid": self.tid,
            "index": self.index,
            "commTruncated": self.comm_truncated,
        })
    }
}

/// The shard index a thread `comm` names, if it is an egress shard thread.
pub(super) fn parse_egress_shard_comm(comm: &str) -> Option<(u32, bool)> {
    let comm = comm.trim();
    let digits = comm.strip_prefix(EGRESS_SHARD_PREFIX)?;
    if digits.is_empty() || !digits.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let index = digits.parse::<u32>().ok()?;
    Some((index, comm.len() >= COMM_MAX_BYTES))
}

/// Select the egress shard threads from a process's thread list, sorted by
/// (index, tid). Non-egress threads are ignored; a comm that is a *suffix*
/// match on a longer name (`x-egress-shard-0`) is rejected because the prefix
/// must start the name.
pub(super) fn select_egress_shard_threads(threads: &[ThreadComm]) -> Vec<EgressShardThread> {
    let mut selected: Vec<EgressShardThread> = threads
        .iter()
        .filter_map(|thread| {
            parse_egress_shard_comm(&thread.comm).map(|(index, comm_truncated)| EgressShardThread {
                tid: thread.tid,
                index,
                comm_truncated,
            })
        })
        .collect();
    selected.sort_by_key(|thread| (thread.index, thread.tid));
    selected
}

pub(super) fn read_thread_comms(pid: u32) -> Result<Vec<ThreadComm>, String> {
    let task_dir = PathBuf::from(format!("/proc/{pid}/task"));
    let entries = std::fs::read_dir(&task_dir)
        .map_err(|error| format!("read {}: {error}", task_dir.display()))?;
    let mut threads = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|error| error.to_string())?;
        let Some(tid) = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u32>().ok())
        else {
            continue;
        };
        let comm = std::fs::read_to_string(entry.path().join("comm")).unwrap_or_default();
        threads.push(ThreadComm {
            tid,
            comm: comm.trim().to_string(),
        });
    }
    Ok(threads)
}

// ---------------------------------------------------------------------------
// CPU accounting
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(super) struct CpuTicks {
    pub(super) user: u64,
    pub(super) system: u64,
}

impl CpuTicks {
    pub(super) fn total(self) -> u64 {
        self.user.saturating_add(self.system)
    }
}

/// Parse `/proc/<pid>/stat` (or `/proc/<pid>/task/<tid>/stat`) CPU ticks. The
/// second field is a parenthesised `comm` that may itself contain spaces and
/// parentheses, so fields are counted from the last `)` — a whitespace split
/// silently shifts utime/stime on such a name.
pub(super) fn parse_proc_stat_cpu(stat: &str) -> Result<CpuTicks, String> {
    let tail = stat
        .rfind(')')
        .map(|index| &stat[index + 1..])
        .ok_or_else(|| "proc stat missing comm field".to_string())?;
    // Fields after comm start at index 0 == state (field 3 overall); utime is
    // field 14 overall, i.e. index 11 here, stime index 12.
    let fields: Vec<&str> = tail.split_whitespace().collect();
    let read = |index: usize, name: &str| {
        fields
            .get(index)
            .and_then(|value| value.parse::<u64>().ok())
            .ok_or_else(|| format!("proc stat missing {name}"))
    };
    Ok(CpuTicks {
        user: read(11, "utime")?,
        system: read(12, "stime")?,
    })
}

pub(super) fn read_proc_stat_cpu(path: &Path) -> Result<CpuTicks, String> {
    let stat = std::fs::read_to_string(path)
        .map_err(|error| format!("read {}: {error}", path.display()))?;
    parse_proc_stat_cpu(&stat)
}

pub(super) fn clock_ticks_per_sec() -> u64 {
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks > 0 { ticks as u64 } else { 100 }
}

/// `current - previous`; `None` when the counter went backwards, so a reset or
/// a recycled pid is never reported as a delta.
pub(super) fn ticks_delta(current: u64, previous: u64) -> Option<u64> {
    (current >= previous).then(|| current - previous)
}

/// Microseconds of CPU per counter unit. `None` when the window carried no
/// units: a ratio over an empty window is undefined, not zero.
pub(super) fn micros_per_unit(seconds: f64, units: u64) -> Option<f64> {
    (units > 0).then(|| seconds * 1_000_000.0 / units as f64)
}

/// CPU seconds between two tick readings, rounded to nanoseconds.
pub(super) fn cpu_secs_between(
    before: CpuTicks,
    after: CpuTicks,
    ticks_per_sec: u64,
) -> Option<f64> {
    if ticks_per_sec == 0 {
        return None;
    }
    let ticks = ticks_delta(after.total(), before.total())?;
    Some(ticks as f64 / ticks_per_sec as f64)
}

/// Round to 6 decimal places: enough for microsecond-per-datagram ratios
/// without printing float noise into the artifact.
pub(super) fn round6(value: f64) -> f64 {
    (value * 1_000_000.0).round() / 1_000_000.0
}

pub(super) fn opt_round6(value: Option<f64>) -> Value {
    value.map_or(Value::Null, |value| json!(round6(value)))
}

pub(super) fn read_cpus_allowed_list(pid: u32) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/{pid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .map(|value| value.trim().to_string())
}

/// Observed affinities of a tid, as a CPU list.
pub(super) fn observe_affinity(tid: u32) -> Result<Vec<u32>, String> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    let rc = unsafe {
        libc::sched_getaffinity(
            tid as libc::pid_t,
            std::mem::size_of::<libc::cpu_set_t>(),
            &mut set,
        )
    };
    if rc != 0 {
        return Err(format!(
            "sched_getaffinity({tid}): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok((0..libc::CPU_SETSIZE as usize)
        .filter(|cpu| unsafe { libc::CPU_ISSET(*cpu, &set) })
        .map(|cpu| cpu as u32)
        .collect())
}

/// Pin one thread to exactly `cpu` with `sched_setaffinity`, falling back to
/// `/usr/bin/taskset -pc` when the kernel refuses (a same-UID child is granted
/// this, but the fallback keeps the mode honest if it is not).
pub(super) fn set_thread_affinity(tid: u32, cpu: u32) -> Result<(Vec<u32>, &'static str), String> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe {
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu as usize, &mut set);
    }
    let rc = unsafe {
        libc::sched_setaffinity(
            tid as libc::pid_t,
            std::mem::size_of::<libc::cpu_set_t>(),
            &set,
        )
    };
    if rc == 0 {
        return Ok((observe_affinity(tid)?, "sched_setaffinity"));
    }
    let syscall_error = std::io::Error::last_os_error();
    let fallback = std::process::Command::new("/usr/bin/taskset")
        .args(["-pc", &cpu.to_string(), &tid.to_string()])
        .output();
    match fallback {
        Ok(output) if output.status.success() => Ok((observe_affinity(tid)?, "taskset-fallback")),
        Ok(output) => Err(format!(
            "sched_setaffinity({tid}, {cpu}) failed ({syscall_error}); taskset -pc fallback failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        )),
        Err(error) => Err(format!(
            "sched_setaffinity({tid}, {cpu}) failed ({syscall_error}); taskset fallback could not run: {error}"
        )),
    }
}
