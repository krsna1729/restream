//! Shared helpers for harness peer processes and measurement threads.
//!
//! A peer process (`srt-sink`, `udp-drain`) or a pinned measurement thread has
//! to report the same things the same way: the CPU mask it actually observed,
//! its own kernel UDP/NIC drop counters, a run identity, and a `GET /state`
//! endpoint the measuring side differences per window. These live here so the
//! state schema cannot drift between peers.

use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;

/// This process's own CPU affinity, as `/proc/self/status` reports it. Recorded
/// so the measuring host sees the peer's *observed* mask, not what was asked
/// for.
pub(crate) fn cpus_allowed_list() -> Option<String> {
    let status = std::fs::read_to_string("/proc/self/status").ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .map(|value| value.trim().to_string())
}

/// One thread's own CPU affinity. `/proc/self/status` reports the thread-group
/// leader, so a thread that pinned itself needs its own task entry instead.
pub(crate) fn thread_cpus_allowed_list(tid: libc::pid_t) -> Option<String> {
    let status = std::fs::read_to_string(format!("/proc/self/task/{tid}/status")).ok()?;
    status
        .lines()
        .find_map(|line| line.strip_prefix("Cpus_allowed_list:"))
        .map(|value| value.trim().to_string())
}

/// Pin the calling thread (and every thread it spawns afterwards, which
/// inherits the creating thread's mask) to `mask`, e.g. `2-5` or `0,2-3`.
pub(crate) fn pin_to_cpuset(mask: &str) -> Result<(), String> {
    let mut set: libc::cpu_set_t = unsafe { std::mem::zeroed() };
    unsafe { libc::CPU_ZERO(&mut set) };
    for part in mask.split(',') {
        let part = part.trim();
        if part.is_empty() {
            continue;
        }
        let (start, end) = match part.split_once('-') {
            Some((start, end)) => (
                start.parse::<usize>().map_err(|e| e.to_string())?,
                end.parse::<usize>().map_err(|e| e.to_string())?,
            ),
            None => {
                let cpu = part.parse::<usize>().map_err(|e| e.to_string())?;
                (cpu, cpu)
            }
        };
        if end < start || end >= libc::CPU_SETSIZE as usize {
            return Err(format!("cpu mask {mask:?} is out of range"));
        }
        for cpu in start..=end {
            unsafe { libc::CPU_SET(cpu, &mut set) };
        }
    }
    let rc = unsafe { libc::sched_setaffinity(0, std::mem::size_of::<libc::cpu_set_t>(), &set) };
    if rc != 0 {
        return Err(format!(
            "sched_setaffinity({mask:?}): {}",
            std::io::Error::last_os_error()
        ));
    }
    Ok(())
}

/// Identity of one peer process: a restarted peer must be visible as a new run,
/// because its cumulative counters restart too.
pub(crate) fn run_id() -> String {
    let started_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis())
        .unwrap_or(0);
    format!("{:x}-{started_ms:x}", std::process::id())
}

/// `InErrors`, `RcvbufErrors` and `SndbufErrors` from `/proc/net/snmp`'s `Udp:`
/// row: the kernel counters that show a peer dropping datagrams it could not
/// buffer. `None` when the file or any column is unavailable, so a missing
/// sensor never reads as zero drops.
pub(crate) fn host_udp_drop_counters() -> Option<(u64, u64, u64)> {
    let snmp = std::fs::read_to_string("/proc/net/snmp").ok()?;
    let mut lines = snmp
        .lines()
        .filter_map(|line| line.strip_prefix("Udp:"))
        .map(str::split_whitespace);
    let (header, values) = (lines.next()?, lines.next()?);
    let column = |name: &str| -> Option<u64> {
        header
            .clone()
            .position(|field| field == name)
            .and_then(|index| values.clone().nth(index))
            .and_then(|value| value.parse::<u64>().ok())
    };
    Some((
        column("InErrors")?,
        column("RcvbufErrors")?,
        column("SndbufErrors")?,
    ))
}

/// Kernel UDP error counters since a peer started, for the human-readable log
/// line; the state endpoint serves the absolute values.
pub(crate) fn udp_drops_since_start(start: Option<(u64, u64, u64)>) -> Option<(u64, u64, u64)> {
    let (start_in, start_rcv, start_snd) = start?;
    let (in_errors, rcvbuf, sndbuf) = host_udp_drop_counters()?;
    Some((
        in_errors.saturating_sub(start_in),
        rcvbuf.saturating_sub(start_rcv),
        sndbuf.saturating_sub(start_snd),
    ))
}

/// Non-loopback interface drop counters, summed: a receiving NIC can drop
/// before UDP ever sees the packet, so a peer's losslessness evidence needs
/// them alongside the kernel UDP counters. `None` when no interface could be
/// read, so the measuring host sees a missing sensor rather than zero drops.
pub(crate) fn host_nic_drop_counters() -> (Option<u64>, Option<u64>) {
    let mut rx = None;
    let mut tx = None;
    if let Ok(entries) = std::fs::read_dir("/sys/class/net") {
        for entry in entries.flatten() {
            if entry.file_name() == "lo" {
                continue;
            }
            let statistics = entry.path().join("statistics");
            if let Some(value) = read_counter_file(&statistics.join("rx_dropped")) {
                rx = Some(rx.unwrap_or(0) + value);
            }
            if let Some(value) = read_counter_file(&statistics.join("tx_dropped")) {
                tx = Some(tx.unwrap_or(0) + value);
            }
        }
    }
    (rx, tx)
}

pub(crate) fn read_counter_file(path: &Path) -> Option<u64> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|value| value.trim().parse::<u64>().ok())
}

/// The calling thread's kernel thread id, for `/proc/self/task/<tid>/stat`.
pub(crate) fn current_thread_id() -> libc::pid_t {
    unsafe { libc::syscall(libc::SYS_gettid) as libc::pid_t }
}

/// One thread's CPU time split into user and system seconds, plus its
/// voluntary and involuntary context switches: the first cut of an on-CPU /
/// off-CPU attribution (which half of the time, and whether the thread is
/// being descheduled rather than working).
#[derive(Debug)]
pub(crate) struct ThreadCpu {
    pub(crate) user_secs: f64,
    pub(crate) system_secs: f64,
    pub(crate) voluntary_switches: u64,
    pub(crate) involuntary_switches: u64,
}

pub(crate) fn thread_cpu(tid: libc::pid_t) -> Option<ThreadCpu> {
    let stat = std::fs::read_to_string(format!("/proc/self/task/{tid}/stat")).ok()?;
    // The comm field may contain spaces and parentheses; fields after the last
    // ')' are positional.
    let after_comm = stat.rsplit_once(')')?.1;
    let fields: Vec<&str> = after_comm.split_whitespace().collect();
    let utime = fields.get(11)?.parse::<u64>().ok()?;
    let stime = fields.get(12)?.parse::<u64>().ok()?;
    let ticks = unsafe { libc::sysconf(libc::_SC_CLK_TCK) };
    if ticks <= 0 {
        return None;
    }
    let status = std::fs::read_to_string(format!("/proc/self/task/{tid}/status")).ok()?;
    let counter = |name: &str| -> Option<u64> {
        status
            .lines()
            .find_map(|line| line.strip_prefix(name))
            .and_then(|value| value.trim().parse::<u64>().ok())
    };
    Some(ThreadCpu {
        user_secs: utime as f64 / ticks as f64,
        system_secs: stime as f64 / ticks as f64,
        voluntary_switches: counter("voluntary_ctxt_switches:")?,
        involuntary_switches: counter("nonvoluntary_ctxt_switches:")?,
    })
}

/// Bind the peer's `GET /state` listener: dual-stack first (Linux maps IPv4
/// onto `[::]` by default), IPv4-only as the fallback, so a peer addressed by
/// raw IPv6 can still be polled.
pub(crate) async fn bind_state_listener(port: u16) -> Result<TcpListener, String> {
    match TcpListener::bind(format!("[::]:{port}")).await {
        Ok(listener) => Ok(listener),
        Err(dual_stack_error) => {
            TcpListener::bind(format!("0.0.0.0:{port}"))
                .await
                .map_err(|ipv4_error| {
                    format!("state endpoint on {port}: {dual_stack_error} / {ipv4_error}")
                })
        }
    }
}

/// Serve `GET /state` with whatever JSON the peer can report right now. One
/// request per connection: no keep-alive, no routing.
pub(crate) async fn serve_state_json(
    listener: TcpListener,
    state: Arc<dyn Fn() -> Value + Send + Sync>,
) {
    loop {
        let Ok((mut socket, _)) = listener.accept().await else {
            continue;
        };
        let body = state().to_string();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        tokio::spawn(async move {
            let mut request = [0_u8; 512];
            let _ = socket.read(&mut request).await;
            let _ = socket.write_all(response.as_bytes()).await;
            let _ = socket.shutdown().await;
        });
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;
    use std::time::Duration;

    use super::*;

    #[test]
    fn cpus_allowed_list_reports_this_process_mask() {
        // Linux always exposes the field; the harness depends on it to record
        // observed (not requested) placement.
        let mask = cpus_allowed_list().expect("Cpus_allowed_list");
        assert!(
            mask.chars()
                .all(|c| c.is_ascii_digit() || c == ',' || c == '-')
        );
    }

    #[test]
    fn thread_mask_is_observed_per_thread() {
        let tid = current_thread_id();
        let mask = thread_cpus_allowed_list(tid).expect("own thread mask");
        assert!(
            mask.chars()
                .all(|c| c.is_ascii_digit() || c == ',' || c == '-')
        );
        assert!(thread_cpus_allowed_list(-1).is_none());
    }

    #[test]
    fn thread_cpu_reads_the_calling_thread() {
        let tid = current_thread_id();
        assert!(tid > 0);
        let cpu = thread_cpu(tid).expect("own thread cpu time");
        assert!(cpu.user_secs >= 0.0 && cpu.system_secs >= 0.0, "{cpu:?}");
        // A thread that does not exist has no record.
        assert!(thread_cpu(-1).is_none());
    }

    #[test]
    fn thread_cpu_reads_another_thread() {
        // The substrate benchmark reads the sender thread's CPU time from the
        // harness thread, so cross-thread reads must work.
        let tid = std::sync::Arc::new(std::sync::atomic::AtomicI32::new(0));
        let handle = std::thread::spawn({
            let tid = std::sync::Arc::clone(&tid);
            move || {
                tid.store(current_thread_id(), Ordering::Relaxed);
                let deadline = std::time::Instant::now() + Duration::from_millis(300);
                let mut spin = 0_u64;
                while std::time::Instant::now() < deadline {
                    spin = spin.wrapping_add(1);
                }
                spin
            }
        });
        while tid.load(Ordering::Relaxed) == 0 {
            std::thread::yield_now();
        }
        // Let the thread accumulate measurable CPU time before reading it from
        // this thread.
        std::thread::sleep(Duration::from_millis(200));
        let cpu = thread_cpu(tid.load(Ordering::Relaxed));
        let _ = handle.join();
        let busy = cpu
            .as_ref()
            .is_some_and(|cpu| cpu.user_secs + cpu.system_secs > 0.0);
        assert!(busy, "{cpu:?}");
    }

    #[test]
    fn udp_drop_counters_are_absolute_and_monotonic() {
        let (in_errors, rcvbuf, _) = host_udp_drop_counters().expect("/proc/net/snmp Udp row");
        let delta = udp_drops_since_start(Some((in_errors, rcvbuf, 0))).expect("delta");
        // Differencing against the current value cannot go backwards.
        assert_eq!(delta.0, 0);
        assert_eq!(delta.1, 0);
        assert!(udp_drops_since_start(None).is_none());
    }

    #[test]
    fn pin_to_cpuset_rejects_out_of_range_masks() {
        assert!(pin_to_cpuset("99999").is_err());
        assert!(pin_to_cpuset("5-2").is_err());
        assert!(pin_to_cpuset("not-a-mask").is_err());
    }
}
