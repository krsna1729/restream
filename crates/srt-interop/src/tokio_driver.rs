//! Shared tokio-based low-level helpers for the sustained-throughput
//! loss-caller/loss-listener binaries -- the tokio entry in the
//! docs/srt-pure-rust-plan.md Phase 4 driver-framework bake-off. See
//! mio_driver.rs's doc comment for why a blocking-socket/thread::sleep
//! driver isn't good enough, and Cargo.toml's doc comment for why each
//! backend gets its own small driver module rather than one shared trait.

use shiguredo_srt::{ConnectionOutput, SrtConnection, TimerId, Timestamp};
use std::collections::HashMap;
use tokio::net::UdpSocket;

pub async fn drain_outputs(
    conn: &mut SrtConnection,
    socket: &UdpSocket,
    timers: &mut HashMap<TimerId, Timestamp>,
    now: Timestamp,
) {
    while let Some(out) = conn.poll_output() {
        match out {
            ConnectionOutput::SendPacket(bytes) => {
                let _ = socket.send(&bytes).await;
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

/// Microseconds until the earliest-due armed timer, or `default_us` if none
/// are armed yet -- see mio_driver.rs's copy of this function for why the
/// loss-listener binaries need this instead of a fixed poll interval.
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
