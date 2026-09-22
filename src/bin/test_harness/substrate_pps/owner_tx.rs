//! Stage B: production `TxEngine` / `Owner::service` execution driven by
//! benchmark-only pre-materialized injection.
//!
//! This arm prices the Owner/transport execution path above the raw Compio floor
//! Stage A measures. It is **not** the production attach path: the caller side is
//! built with `OwnerCallerSide::new_single` and attached with the benchmark-only
//! `with_caller`, which bypasses every check `Owner::connect` performs, and
//! datagrams are queued with `bench_push_pending` instead of being materialized by
//! the protocol engine.
//!
//! What the Owner-drive CPU scope therefore contains, and is labelled as
//! containing: caller scheduling and pending-output handling, the legacy
//! compatibility copy of each 1316-byte packet into the reserved TxPool slot, the
//! deallocation of the pending `Vec<u8>` that copy came from, TxPool
//! reservation/commit, and TxEngine submission plus completion reaping.

#![cfg(feature = "wi3-owner-bench")]

use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytes::Bytes;
use srt_proto::{ConnectionOptions, ConnectionOutput, SrtConnection, Timestamp};
use srt_transport::advanced::caller::CallerLeg;
use srt_transport::compio::{Owner, OwnerCallerSide, OwnerServiceBudget};

use super::config::{SEND_BUFFER_BYTES, SubstrateConfig, set_send_buffer};
use super::sender::SenderHandles;

/// Thread CPU consumed since an arbitrary point, from
/// `CLOCK_THREAD_CPUTIME_ID` — the sender thread's own CPU, not wall time.
fn thread_cpu_secs() -> f64 {
    let mut spec = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut spec) };
    if rc != 0 {
        return 0.0;
    }
    spec.tv_sec as f64 + spec.tv_nsec as f64 / 1e9
}

/// Datagrams queued per injection batch; injection CPU is measured over these
/// batches and never enters the Owner-drive denominator.
fn injection_batch() -> usize {
    std::env::var("SUBSTRATE_OWNER_INJECT_BATCH")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(64)
        .max(1)
}

/// A connected in-process caller, so the benchmark has a caller leg to inject
/// into without a live SRT peer. Mirrors the pinned upstream benchmark helper.
fn connected_caller(socket_id: u32) -> SrtConnection {
    let mut caller = SrtConnection::new_caller(ConnectionOptions {
        socket_id,
        ..ConnectionOptions::default()
    });
    let mut listener = SrtConnection::new_listener(ConnectionOptions {
        socket_id: socket_id.wrapping_add(100_000).max(1),
        ..ConnectionOptions::default()
    });
    caller.connect(Timestamp::default()).expect("connect");
    for i in 0..10 {
        let now = Timestamp::from_micros(i * 10_000);
        while let Some(output) = caller
            .poll_output()
            .expect("exact-size output materializes")
        {
            if let ConnectionOutput::SendPacket(data) = output {
                let _ = listener.feed_recv_buf(&data, now);
            }
        }
        while let Some(output) = listener
            .poll_output()
            .expect("exact-size output materializes")
        {
            if let ConnectionOutput::SendPacket(data) = output {
                let _ = caller.feed_recv_buf(&data, now);
            }
        }
        if caller.state() == srt_proto::ConnectionState::Connected {
            break;
        }
    }
    assert_eq!(caller.state(), srt_proto::ConnectionState::Connected);
    caller
}

pub(crate) fn run_owner_tx(
    config: &SubstrateConfig,
    payload: Bytes,
    handles: &SenderHandles,
) -> Result<(), String> {
    let target: SocketAddr = config
        .destinations
        .first()
        .copied()
        .ok_or("stage B needs at least one destination")?;
    let batch = injection_batch();
    let prebuilt: Vec<Vec<u8>> = (0..batch).map(|_| payload.to_vec()).collect();

    let runtime = compio::runtime::Runtime::new().map_err(|e| format!("compio runtime: {e}"))?;
    runtime.block_on(async {
        let std_socket =
            std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("udp bind: {e}"))?;
        set_send_buffer(std_socket.as_raw_fd(), SEND_BUFFER_BYTES)?;
        let socket = compio::net::UdpSocket::from_std(std_socket)
            .map_err(|e| format!("compio adopt udp socket: {e}"))?;

        let caller_side = OwnerCallerSide::new_single(socket);
        let mut owner = Owner::new(64).with_caller(caller_side);
        let id = owner
            .bench_caller_table_mut()
            .ok_or("bench caller table unavailable")?
            .add_direct(CallerLeg {
                peer: target,
                connection: connected_caller(5001),
            })
            .map_err(|e| format!("add_direct: {e}"))?;

        let budget = OwnerServiceBudget::default();
        let mut now = Timestamp::from_micros(1_000_000);
        let mut injected = 0_u64;
        let mut drive_cpu = 0.0_f64;

        // Quiescence: stop injecting, drain every queued datagram through the
        // Owner, and reap until nothing is in flight.
        async fn quiesce(
            owner: &mut Owner,
            budget: OwnerServiceBudget,
            now: &mut Timestamp,
            drive_cpu: &mut f64,
        ) {
            for _ in 0..100_000 {
                let start = thread_cpu_secs();
                *now = Timestamp::from_micros(now.as_micros() + 200);
                let _ = owner.service(*now, budget).await;
                owner.wait_for_activity(Duration::from_millis(1)).await;
                *drive_cpu += thread_cpu_secs() - start;
                if owner.tx_in_flight() == 0 && !owner.has_pending_work(*now) {
                    // In-flight zero *and* no due work left: every injected
                    // datagram has been submitted and reaped.
                    break;
                }
            }
        }

        while !handles.stop.load(Ordering::Relaxed) {
            if handles.pause_requested() {
                quiesce(&mut owner, budget, &mut now, &mut drive_cpu).await;
                // At quiescence every injected datagram has been submitted and
                // reaped (in-flight zero, no due work), so injected == completed.
                handles
                    .counters
                    .submitted
                    .store(injected, Ordering::Relaxed);
                handles
                    .counters
                    .completed
                    .store(injected, Ordering::Relaxed);
                handles
                    .counters
                    .owner_drive_cpu_micros
                    .store((drive_cpu * 1e6) as u64, Ordering::Relaxed);
                handles.acknowledge_pause();
                continue;
            }

            let inject_start = thread_cpu_secs();
            {
                let table = owner
                    .bench_caller_table_mut()
                    .ok_or("bench caller table unavailable")?;
                for packet in &prebuilt {
                    table.bench_push_pending(id, target, packet.clone());
                }
            }
            injected += batch as u64;
            handles.counters.injection_cpu_micros.fetch_add(
                ((thread_cpu_secs() - inject_start) * 1e6) as u64,
                Ordering::Relaxed,
            );
            handles
                .counters
                .submitted
                .store(injected, Ordering::Relaxed);

            // Owner-drive scope: the production service rhythm.
            let drive_start = thread_cpu_secs();
            for _ in 0..batch {
                now = Timestamp::from_micros(now.as_micros() + 200);
                let _ = owner.service(now, budget).await;
                owner.wait_for_activity(Duration::from_millis(1)).await;
            }
            drive_cpu += thread_cpu_secs() - drive_start;
            handles.counters.owner_drive_cpu_micros.fetch_add(
                ((thread_cpu_secs() - drive_start) * 1e6) as u64,
                Ordering::Relaxed,
            );
        }
        Ok(())
    })
}
