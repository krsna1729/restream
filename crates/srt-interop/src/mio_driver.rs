//! Shared mio-based low-level helpers for the sustained-throughput
//! loss-caller/loss-listener binaries.
//!
//! driver.rs's blocking-socket + fixed-poll-timeout pattern is fine for the
//! short Phase 3 interop checks it was built for, but it is the wrong tool
//! for Phase 4's differential loss/latency matrix: a fixed poll timeout (or
//! worse, `thread::sleep`) quantizes every pacing interval upward and adds
//! real wall-clock jitter to RTT measurements, confounding the very
//! libsrt-vs-Rust-Core comparison this matrix exists to make (observed
//! directly: ~35-40% throughput shortfall and 3-5x inflated baseline RTT
//! versus libsrt under otherwise-identical conditions). mio's epoll-based
//! readiness polling with a *precisely computed* timeout -- block until
//! exactly the next due send or timer, not a guessed fixed interval --
//! removes that confound without building a full production Driver (that is
//! docs/srt-pure-rust-plan.md Phase 6/7's swappable mio/tokio/monoio
//! architecture, out of scope for a differential test harness).

use mio::net::UdpSocket;
use shiguredo_srt::{ConnectionOutput, SrtConnection, TimerId, Timestamp};
use std::collections::HashMap;

pub fn drain_outputs(
    conn: &mut SrtConnection,
    socket: &UdpSocket,
    timers: &mut HashMap<TimerId, Timestamp>,
    now: Timestamp,
) {
    while let Some(out) = conn.poll_output() {
        match out {
            ConnectionOutput::SendPacket(bytes) => {
                let _ = socket.send(&bytes);
            }
            ConnectionOutput::SetTimer {
                id,
                duration_micros,
            } => {
                timers.insert(id, now.add_micros(duration_micros));
            }
            ConnectionOutput::ClearTimer { id } => {
                timers.remove(&id);
            }
        }
    }
}

/// Timer IDs whose deadline has passed by `now`; removed from `timers`
/// (the caller is expected to re-arm via the next `drain_outputs` if the
/// Core schedules a follow-up).
pub fn due_timers(timers: &mut HashMap<TimerId, Timestamp>, now: Timestamp) -> Vec<TimerId> {
    let due: Vec<TimerId> = timers
        .iter()
        .filter(|(_, deadline)| now.as_micros() >= deadline.as_micros())
        .map(|(id, _)| *id)
        .collect();
    for id in &due {
        timers.remove(id);
    }
    due
}

/// Microseconds until the earliest-due armed timer (e.g. the 10ms ACK
/// timer), or `default_us` if none are armed yet. Used by the
/// loss-listener binaries so their poll wait tracks the next real timer
/// deadline instead of a fixed interval -- a fixed wait adds up to its own
/// duration of jitter to when ACKs actually go out, which pollutes the RTT
/// measurement this whole differential matrix exists to make.
pub fn time_until_earliest_timer(
    timers: &HashMap<TimerId, Timestamp>,
    now: Timestamp,
    default_us: u64,
) -> u64 {
    timers
        .values()
        .map(|deadline| deadline.as_micros().saturating_sub(now.as_micros()))
        .min()
        .unwrap_or(default_us)
}
