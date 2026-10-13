//! Restream CPU per thread group over a rated window, so a run shows where
//! the time went: Tokio workers and blocking pool (`restream-tokio`), the
//! RTMP ingress owner (`restream-rtmp-c`, the kernel's 15-byte truncation of
//! `restream-rtmp-compio-owner`), the SRT ingress Owner (`srt-in`), egress
//! shards (`egress`) and the rest; and how long each group's runnable threads
//! waited for a CPU (`/proc/<pid>/task/*/schedstat`, the run-queue delay
//! `perf sched` would report, without its privileges). Read from `/proc` at
//! the window's start and end only, never per packet.

use std::collections::BTreeMap;

/// Thread name with its trailing `-<digits>` segments removed, so `egress-3`,
/// `srt-in-32354` and the multi-Owner `srt-in-32354-1` group with their
/// siblings.
pub(super) fn thread_group(comm: &str) -> String {
    let mut comm = comm.trim();
    while let Some((head, tail)) = comm.rsplit_once('-')
        && !tail.is_empty()
        && tail.bytes().all(|b| b.is_ascii_digit())
    {
        comm = head;
    }
    comm.to_string()
}

/// One reading of a cumulative per-thread counter: thread ID to its group
/// and value. Deltas are taken per thread, so a thread that exits during the
/// window drops only its own in-window share instead of its whole history.
pub(super) type ThreadCounters = BTreeMap<u32, (String, u64)>;

/// Cumulative user+system clock ticks per thread.
pub(super) fn thread_ticks(pid: u32) -> ThreadCounters {
    read_tasks(pid, |task| {
        let stat = std::fs::read_to_string(task.join("stat")).ok()?;
        let (comm, ticks) = parse_task_stat(&stat)?;
        Some((thread_group(comm), ticks))
    })
}

/// Cumulative run-queue wait in nanoseconds per thread: the second field of
/// `/proc/<pid>/task/<tid>/schedstat` (time runnable but not running).
pub(super) fn thread_runqueue_ns(pid: u32) -> ThreadCounters {
    read_tasks(pid, |task| {
        let comm = std::fs::read_to_string(task.join("comm")).ok()?;
        let schedstat = std::fs::read_to_string(task.join("schedstat")).ok()?;
        let wait = schedstat.split_whitespace().nth(1)?.parse().ok()?;
        Some((thread_group(&comm), wait))
    })
}

fn read_tasks(
    pid: u32,
    read: impl Fn(&std::path::Path) -> Option<(String, u64)>,
) -> ThreadCounters {
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return ThreadCounters::new();
    };
    tasks
        .flatten()
        .filter_map(|task| {
            let tid = task.file_name().to_str()?.parse().ok()?;
            Some((tid, read(&task.path())?))
        })
        .collect()
}

/// Run-queue wait per group between two readings `seconds` apart, as % of
/// one thread's wall time (summed over the group's threads).
pub(super) fn thread_group_runqueue_pct(
    start: &ThreadCounters,
    end: &ThreadCounters,
    seconds: f64,
) -> BTreeMap<String, f64> {
    per_group_pct(start, end, seconds, 1e9)
}

/// `(comm, utime + stime)` from one `/proc/.../stat` line. The name sits in
/// parentheses and may contain spaces, so fields are counted after the last
/// `)`: state is field 3, utime field 14, stime field 15.
fn parse_task_stat(stat: &str) -> Option<(&str, u64)> {
    let open = stat.find('(')?;
    let close = stat.rfind(')')?;
    let comm = stat.get(open + 1..close)?;
    let mut rest = stat.get(close + 1..)?.split_whitespace();
    let utime: u64 = rest.nth(11)?.parse().ok()?;
    let stime: u64 = rest.next()?.parse().ok()?;
    Some((comm, utime + stime))
}

/// CPU % of one core per group between two readings `seconds` apart.
pub(super) fn thread_group_cpu_pct(
    start: &ThreadCounters,
    end: &ThreadCounters,
    seconds: f64,
) -> BTreeMap<String, f64> {
    // SAFETY: sysconf has no preconditions.
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    per_group_pct(start, end, seconds, clk_tck)
}

/// Each thread's counter delta between two readings `seconds` apart, summed
/// per group, as % of one thread's wall time, for a counter running at
/// `units_per_second`. A thread new since `start` counts from zero.
fn per_group_pct(
    start: &ThreadCounters,
    end: &ThreadCounters,
    seconds: f64,
    units_per_second: f64,
) -> BTreeMap<String, f64> {
    if seconds <= 0.0 || units_per_second <= 0.0 {
        return BTreeMap::new();
    }
    let mut units = BTreeMap::<String, u64>::new();
    for (tid, (group, value)) in end {
        let before = start.get(tid).map_or(0, |(_, value)| *value);
        *units.entry(group.clone()).or_default() += value.saturating_sub(before);
    }
    units
        .into_iter()
        .map(|(group, units)| {
            let pct = 100.0 * units as f64 / units_per_second / seconds;
            (group, (pct * 10.0).round() / 10.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_strip_trailing_numbers_only() {
        assert_eq!(thread_group("egress-3"), "egress");
        assert_eq!(thread_group("srt-in-32354"), "srt-in");
        assert_eq!(thread_group("srt-in-32354-1"), "srt-in");
        assert_eq!(thread_group("restream-tokio"), "restream-tokio");
        assert_eq!(thread_group("restream-rtmp-c"), "restream-rtmp-c");
        assert_eq!(thread_group("sqlx-sqlite-wor"), "sqlx-sqlite-wor");
    }

    #[test]
    fn stat_fields_are_counted_after_the_name() {
        let stat = "4242 (odd name) 1) S 1 2 3 4 5 6 7 8 9 10 111 222 0 0 20 0 1 0";
        assert_eq!(parse_task_stat(stat), Some(("odd name) 1", 333)));
    }

    fn counters(rows: &[(u32, &str, u64)]) -> ThreadCounters {
        rows.iter()
            .map(|(tid, group, value)| (*tid, (group.to_string(), *value)))
            .collect()
    }

    #[test]
    fn percent_is_per_group_delta() {
        let start = counters(&[(1, "egress", 100)]);
        let end = counters(&[(1, "egress", 100 + 4 * 100), (2, "srt-in", 50)]);
        let pct = thread_group_cpu_pct(&start, &end, 2.0);
        // SAFETY: sysconf has no preconditions.
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
        assert!((pct["egress"] - (40_000.0 / clk_tck / 2.0 * 10.0).round() / 10.0).abs() < 1e-9);
        assert!(pct["srt-in"] > 0.0);
    }

    #[test]
    fn runqueue_wait_is_nanoseconds_per_second_of_window() {
        let start = counters(&[(7, "restream-tokio", 1_000_000_000)]);
        let end = counters(&[(7, "restream-tokio", 1_500_000_000)]);
        // 0.5 s of waiting over a 2 s window.
        let pct = thread_group_runqueue_pct(&start, &end, 2.0);
        assert_eq!(pct["restream-tokio"], 25.0);
    }

    /// A blocking-pool thread that exits mid-window must not erase its
    /// group's in-window time from the threads that stayed.
    #[test]
    fn an_exited_thread_does_not_erase_its_siblings_window() {
        let start = counters(&[
            (1, "restream-tokio", 1_000_000_000),
            (2, "restream-tokio", 9_000_000_000),
        ]);
        let end = counters(&[(1, "restream-tokio", 1_500_000_000)]);
        let pct = thread_group_runqueue_pct(&start, &end, 2.0);
        assert_eq!(pct["restream-tokio"], 25.0);
    }
}
