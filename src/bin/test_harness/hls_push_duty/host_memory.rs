//! Host memory-management counters around the rated window, so a run can
//! tell a multi-millisecond poll spent in glibc from one spent in the kernel's
//! memory management behind it: `/proc/vmstat` direct compaction and direct
//! reclaim stalls, the Restream cgroup's `memory.events`, and the THP mode.
//! The parsers are pure and unit tested; only `capture` and `thp_settings`
//! read the host.

use serde_json::{Value, json};
use std::collections::BTreeMap;

/// Counters read at one instant.
#[derive(Debug, Default)]
pub(crate) struct HostMemorySample {
    vmstat: BTreeMap<String, u64>,
    cgroup: Option<String>,
    memory_events: BTreeMap<String, u64>,
}

/// The `/proc/vmstat` counters the split needs: direct compaction stalls,
/// direct reclaim stalls per zone, THP faults, and major faults.
pub(crate) fn vmstat_key_kept(key: &str) -> bool {
    matches!(key, "compact_stall" | "thp_fault_alloc" | "pgmajfault")
        || key.starts_with("allocstall_")
}

/// `name value` lines (the `/proc/vmstat` and `memory.events` format) whose
/// name `keep` accepts. Malformed lines are skipped.
pub(crate) fn parse_counters(text: &str, keep: impl Fn(&str) -> bool) -> BTreeMap<String, u64> {
    text.lines()
        .filter_map(|line| {
            let (name, value) = line.split_once(' ')?;
            let value = value.trim().parse().ok()?;
            keep(name).then(|| (name.to_string(), value))
        })
        .collect()
}

/// What each counter in `after` moved since `before` (a counter absent before
/// counts from zero; a counter that went backwards reads zero).
pub(crate) fn counter_deltas(
    before: &BTreeMap<String, u64>,
    after: &BTreeMap<String, u64>,
) -> BTreeMap<String, u64> {
    after
        .iter()
        .map(|(name, value)| {
            let earlier = before.get(name).copied().unwrap_or(0);
            (name.clone(), value.saturating_sub(earlier))
        })
        .collect()
}

/// The cgroup v2 path from a `/proc/<pid>/cgroup` listing (`0::/path`).
pub(crate) fn cgroup_v2_path(proc_cgroup: &str) -> Option<String> {
    proc_cgroup
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::to_string)
}

/// The selected mode of a THP setting file (`always [madvise] never`).
pub(crate) fn thp_mode(text: &str) -> Option<String> {
    let start = text.find('[')? + 1;
    let end = start + text[start..].find(']')?;
    Some(text[start..end].to_string())
}

/// Read the host's counters, and the cgroup of `pid` (Restream's process).
pub(crate) fn capture(pid: Option<u32>) -> HostMemorySample {
    let vmstat = std::fs::read_to_string("/proc/vmstat")
        .map(|text| parse_counters(&text, vmstat_key_kept))
        .unwrap_or_default();
    let cgroup = pid
        .and_then(|pid| std::fs::read_to_string(format!("/proc/{pid}/cgroup")).ok())
        .and_then(|text| cgroup_v2_path(&text));
    let memory_events = cgroup
        .as_ref()
        .and_then(|path| {
            std::fs::read_to_string(format!("/sys/fs/cgroup{path}/memory.events")).ok()
        })
        .map(|text| parse_counters(&text, |_| true))
        .unwrap_or_default();
    HostMemorySample {
        vmstat,
        cgroup,
        memory_events,
    }
}

/// The host's THP `enabled` and `defrag` modes.
pub(crate) fn thp_settings() -> Value {
    let mode = |name: &str| {
        std::fs::read_to_string(format!("/sys/kernel/mm/transparent_hugepage/{name}"))
            .ok()
            .and_then(|text| thp_mode(&text))
    };
    json!({"enabled": mode("enabled"), "defrag": mode("defrag")})
}

/// The window's deltas: `vmstat` and `memoryEvents` (empty where the host
/// does not expose them), the cgroup they were read from, and `thp`.
pub(crate) fn host_memory_report(
    before: &HostMemorySample,
    after: &HostMemorySample,
    thp: Value,
) -> Value {
    json!({
        "thp": thp,
        "vmstat": counter_deltas(&before.vmstat, &after.vmstat),
        "cgroup": after.cgroup,
        "memoryEvents": counter_deltas(&before.memory_events, &after.memory_events),
    })
}
