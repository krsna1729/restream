//! Restream CPU per thread group over a rated window, so a run shows where
//! the time went: Tokio workers and blocking pool (`restream-tokio`), the
//! RTMP ingress owner (`restream-rtmp-c`, the kernel's 15-byte truncation of
//! `restream-rtmp-compio-owner`), the SRT ingress Owner (`srt-in`), egress
//! shards (`egress`) and the rest. Read from `/proc/<pid>/task/*/stat` at the
//! window's start and end only, never per packet.

use std::collections::BTreeMap;

/// Thread name with a trailing `-<digits>` removed, so `egress-3` and
/// `srt-in-32354` group with their siblings.
pub(super) fn thread_group(comm: &str) -> String {
    let comm = comm.trim();
    match comm.rsplit_once('-') {
        Some((head, tail)) if !tail.is_empty() && tail.bytes().all(|b| b.is_ascii_digit()) => {
            head.to_string()
        }
        _ => comm.to_string(),
    }
}

/// Cumulative user+system clock ticks per thread group. Threads that exit
/// between readings simply stop contributing.
pub(super) fn thread_group_ticks(pid: u32) -> BTreeMap<String, u64> {
    let mut groups = BTreeMap::new();
    let Ok(tasks) = std::fs::read_dir(format!("/proc/{pid}/task")) else {
        return groups;
    };
    for task in tasks.flatten() {
        let Ok(stat) = std::fs::read_to_string(task.path().join("stat")) else {
            continue;
        };
        if let Some((comm, ticks)) = parse_task_stat(&stat) {
            *groups.entry(thread_group(comm)).or_default() += ticks;
        }
    }
    groups
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
    start: &BTreeMap<String, u64>,
    end: &BTreeMap<String, u64>,
    seconds: f64,
) -> BTreeMap<String, f64> {
    // SAFETY: sysconf has no preconditions.
    let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
    if seconds <= 0.0 || clk_tck <= 0.0 {
        return BTreeMap::new();
    }
    end.iter()
        .map(|(group, ticks)| {
            let before = start.get(group).copied().unwrap_or(0);
            let pct = 100.0 * ticks.saturating_sub(before) as f64 / clk_tck / seconds;
            (group.clone(), (pct * 10.0).round() / 10.0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn groups_strip_a_trailing_number_only() {
        assert_eq!(thread_group("egress-3"), "egress");
        assert_eq!(thread_group("srt-in-32354"), "srt-in");
        assert_eq!(thread_group("restream-tokio"), "restream-tokio");
        assert_eq!(thread_group("restream-rtmp-c"), "restream-rtmp-c");
        assert_eq!(thread_group("sqlx-sqlite-wor"), "sqlx-sqlite-wor");
    }

    #[test]
    fn stat_fields_are_counted_after_the_name() {
        let stat = "4242 (odd name) 1) S 1 2 3 4 5 6 7 8 9 10 111 222 0 0 20 0 1 0";
        assert_eq!(parse_task_stat(stat), Some(("odd name) 1", 333)));
    }

    #[test]
    fn percent_is_per_group_delta() {
        let start = BTreeMap::from([("egress".to_string(), 100)]);
        let end = BTreeMap::from([
            ("egress".to_string(), 100 + 4 * 100),
            ("srt-in".to_string(), 50),
        ]);
        let pct = thread_group_cpu_pct(&start, &end, 2.0);
        // SAFETY: sysconf has no preconditions.
        let clk_tck = unsafe { libc::sysconf(libc::_SC_CLK_TCK) } as f64;
        assert!((pct["egress"] - (40_000.0 / clk_tck / 2.0 * 10.0).round() / 10.0).abs() < 1e-9);
        assert!(pct["srt-in"] > 0.0);
    }
}
