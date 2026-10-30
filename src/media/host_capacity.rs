//! Host CPU capacity read once at boot, and the execution-class sizes derived
//! from it.

/// The host's CPUs, read once at boot (`effective_cpu_count`: affinity mask
/// and root cgroup `cpu.max`), and the default size of every resident
/// execution class derived from them. Every sizing law takes its CPU count
/// from here, so no two laws see different hosts and none re-reads `/proc` or
/// `/sys` after boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostCapacity {
    pub cpus: usize,
}

impl HostCapacity {
    pub fn get() -> Self {
        static HOST: std::sync::OnceLock<HostCapacity> = std::sync::OnceLock::new();
        *HOST.get_or_init(|| Self {
            cpus: effective_cpu_count(),
        })
    }

    /// CONTROL (Tokio) workers: all CPUs up to 2, else `ceil(C/3)` in `[2, 8]`.
    pub fn tokio_workers(self) -> usize {
        let cpus = self.cpus.max(1);
        if cpus <= 2 {
            cpus
        } else {
            cpus.div_ceil(3).clamp(2, 8)
        }
    }

    /// MEDIA (`restream-media`) workers: `clamp(C, 1, 4)`.
    pub fn media_workers(self) -> usize {
        self.cpus.clamp(1, 4)
    }

    /// Egress fabric shard threads scale with host cores instead of a flat
    /// constant. On a 6-CPU host at a
    /// combined 1,140 RTMP + 60 SRT workload, a flat 4 shards cost ~12% more
    /// avg CPU and ~23% more peak CPU than shards matched to the core count,
    /// measured across three live `perf` captures at shards=2, 4, and 6. This is the ceiling of one shard pool: `clamp(C, 2, 8)`.
    pub fn egress_shards_per_pool(self) -> u32 {
        self.cpus.clamp(2, 8) as u32
    }
}

pub(crate) fn proc_status_value(status: &str, key: &str) -> Option<String> {
    status.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        (name == key).then(|| value.trim().to_string())
    })
}

#[cfg(target_os = "linux")]
pub(crate) fn parse_cpu_list_count(list: &str) -> Option<u64> {
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

/// The CPUs runtime sizing uses, read once at boot by
/// [`HostCapacity::get`]; call that.
///
/// The root `cpu.max` lookup intentionally preserves the historical startup
/// behavior. The operational snapshot (`system_sampling`) resolves the process's nested
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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::{parse_cpu_allowed_list_count, parse_cpu_list_count, parse_cpu_max_quota};

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
    fn startup_cpu_quota_rounds_up_and_treats_max_as_unlimited() {
        assert_eq!(parse_cpu_max_quota("max 100000"), None);
        assert_eq!(parse_cpu_max_quota("100000 100000"), Some(1));
        assert_eq!(parse_cpu_max_quota("150000 100000"), Some(2));
        assert_eq!(parse_cpu_max_quota("250000 100000"), Some(3));
    }
}
