//! Stage B: production `TxEngine` / `Owner::service` execution driven by
//! benchmark-only pre-materialized injection.
//!
//! This arm prices the Owner/transport execution path above the raw Compio floor
//! Stage A measures. It is **not** the production attach path: benchmark caller
//! legs are built with `OwnerCallerSide::new_single` and attached with the
//! benchmark-only `with_caller`, which bypasses every check `Owner::connect`
//! performs, and datagrams are queued with `bench_push_pending` instead of being
//! materialized by the protocol engine.
//!
//! What the Owner-drive CPU scope therefore contains, and is labelled as
//! containing: caller scheduling and pending-output handling, the legacy
//! compatibility copy of each 1316-byte packet into the reserved TxPool slot, the
//! deallocation of the pending `Vec<u8>` that copy came from, TxPool
//! reservation/commit, and TxEngine submission plus completion reaping.
//!
//! Stage B is valid only when the accumulated Owner reports prove it:
//! `tx_in_flight == 0`, zero send failures, actual submitted == actual completed,
//! and — because this stage excludes protocol generation — actual submitted ==
//! injected synthetic datagrams. Anything else is contamination, and the run must
//! say so rather than relaxing the fence.

#![cfg(feature = "wi3-owner-bench")]

use std::os::fd::AsRawFd;
use std::sync::atomic::Ordering;
use std::time::Duration;

use bytes::Bytes;
use srt_proto::{ConnectionOptions, ConnectionOutput, SrtConnection, Timestamp};
use srt_transport::advanced::caller::CallerLeg;
use srt_transport::compio::{Owner, OwnerCallerSide, OwnerServiceBudget};

use super::config::{SEND_BUFFER_BYTES, SubstrateConfig, set_send_buffer};
use super::sender::SenderHandles;
use super::sender::ServiceTotals;

/// Datagrams queued per injection batch; injection CPU is measured over these
/// batches and never enters the Owner-drive denominator.
fn injection_batch() -> usize {
    std::env::var("SUBSTRATE_OWNER_INJECT_BATCH")
        .ok()
        .and_then(|value| value.trim().parse::<usize>().ok())
        .unwrap_or(64)
        .max(1)
}

/// A connected in-process caller with no residual protocol output left behind.
/// Mirrors the pinned upstream benchmark helper, then drains the handshake's own
/// leftovers so the leg starts silent.
fn quiet_connected_caller(socket_id: u32) -> SrtConnection {
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
    // Residual handshake and timer outputs are protocol generation, not benchmark
    // traffic: leave none of them in the connection.
    while caller.poll_output().expect("output").is_some() {}
    caller
}

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

pub(crate) fn run_owner_tx(
    config: &SubstrateConfig,
    payload: Bytes,
    handles: &SenderHandles,
) -> Result<(), String> {
    if config.destinations.is_empty() {
        return Err("stage B needs at least one destination".to_string());
    }
    let batch = injection_batch();
    let prebuilt: Vec<Vec<u8>> = (0..batch).map(|_| payload.to_vec()).collect();

    // The sender thread must own the Compio runtime before any socket it
    // touches: the caller side registers its poll fd on the current runtime.
    let runtime = compio::runtime::Runtime::new().map_err(|e| format!("compio runtime: {e}"))?;
    runtime.block_on(async move {
        let std_socket =
            std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("udp bind: {e}"))?;
        set_send_buffer(std_socket.as_raw_fd(), SEND_BUFFER_BYTES)?;
        let socket = compio::net::UdpSocket::from_std(std_socket)
            .map_err(|e| format!("compio adopt udp socket: {e}"))?;

        // The caller side owns its sockets on the Compio thread that drives them:
        // construct it inside the async block on the sender thread, not before it.
        let caller_side = OwnerCallerSide::new_single(socket);
        let mut owner = Owner::new(config.queue_depth).with_caller(caller_side);

        // One benchmark caller leg per configured destination, with unique socket
        // ids, so the Stage-B fanout matches Stage A. Deadlines are cleared on
        // every leg: protocol time is frozen and timer generation is excluded.
        let mut leg_ids = Vec::with_capacity(config.destinations.len());
        for (index, destination) in config.destinations.iter().enumerate() {
            let id = owner
                .bench_caller_table_mut()
                .ok_or("bench caller table unavailable")?
                .add_direct(CallerLeg {
                    peer: *destination,
                    connection: quiet_connected_caller(5001 + index as u32 * 97),
                })
                .map_err(|e| format!("add_direct: {e}"))?;
            {
                let table = owner
                    .bench_caller_table_mut()
                    .ok_or("bench caller table unavailable")?;
                table.bench_clear_deadline(id);
            }
            leg_ids.push(id);
        }
        // Drain any initial connection events so the caller table's event queue
        // starts clean and does not falsely signal pending work.
        let mut initial_events = Vec::new();
        owner.poll_caller_events(&mut initial_events);

        let start_instant = std::time::Instant::now();
        let budget = OwnerServiceBudget::default();
        let mut totals = ServiceTotals::default();
        let mut injected = 0_u64;
        let mut round_robin = 0_usize;
        // The pool hosts exactly one caller with exactly one in-flight service
        // operation. A second concurrent visitor is a bug, not backpressure: prove
        // it cannot happen by panicking on entry instead of serializing on a lock.
        let entered = std::sync::atomic::AtomicBool::new(false);
        while !handles.stop.load(Ordering::Relaxed) {
            if handles.pause_requested() {
                drain_to_quiescence(&mut owner, budget, start_instant, &mut totals).await?;
                let submitted = totals.submitted();
                let completed = totals.completed_ok();
                handles
                    .counters
                    .submitted
                    .store(submitted, Ordering::Relaxed);
                handles
                    .counters
                    .completed
                    .store(completed, Ordering::Relaxed);
                // Validity is a proven equality, not an assumption: quiescence
                // must leave every injected datagram submitted and completed.
                if submitted != injected || completed != injected {
                    return Err(format!(
                        "stage B quiescence invalid: injected={injected} submitted={submitted} \
                         completed={completed} (protocol excess or dropped work; see owner reports)"
                    ));
                }
                handles.acknowledge_pause();
                continue;
            }

            // Injection scope: queue up to free capacity in the pool,
            // distributed round-robin across the legs so the fanout matches Stage A.
            let free = owner.tx_pool().free_count();
            if free > 0 {
                let to_inject = free.min(batch);
                let inject_start = thread_cpu_secs();
                for _ in 0..to_inject {
                    let position = round_robin % leg_ids.len();
                    let id = leg_ids[position];
                    round_robin += 1;
                    owner
                        .bench_caller_table_mut()
                        .ok_or("bench caller table unavailable")?
                        .bench_push_pending(
                            id,
                            config.destinations[position],
                            prebuilt[round_robin % batch].clone(),
                        );
                    injected += 1;
                }
                handles
                    .counters
                    .submitted
                    .store(injected, Ordering::Relaxed);
                handles.counters.injection_cpu_micros.fetch_add(
                    ((thread_cpu_secs() - inject_start) * 1e6) as u64,
                    Ordering::Relaxed,
                );
            }

            assert!(
                !entered.swap(true, std::sync::atomic::Ordering::SeqCst),
                "owner service re-entered while a service operation is in flight"
            );

            // Owner-drive scope: service and wait for activity.
            let drive_start = thread_cpu_secs();
            let now = Timestamp::from_micros(start_instant.elapsed().as_micros() as u64);
            let report = owner.service(now, budget).await;
            totals.absorb(&report, &handles.counters);
            if owner.tx_in_flight() > 0 {
                let wait_timeout = if owner.tx_pool().free_count() == 0 {
                    Duration::from_millis(1)
                } else {
                    Duration::ZERO
                };
                owner.wait_for_activity(wait_timeout).await;
            }
            handles.counters.owner_drive_cpu_micros.fetch_add(
                ((thread_cpu_secs() - drive_start) * 1e6) as u64,
                Ordering::Relaxed,
            );
            entered.store(false, std::sync::atomic::Ordering::SeqCst);
        }
        // Totals cross into the shared handles here because `totals` is moved
        // below; quiescence counters were already settled at the last pause.
        totals_in_report(handles, std::mem::take(&mut totals));
        Ok(())
    })
}

/// Move the finished accumulation onto the shared handles, so `sender_thread`
/// can attach it to the report and the mode's `stageBTotals` block is
/// Owner-report truth rather than injected-bookkeeping.
fn totals_in_report(handles: &SenderHandles, totals: ServiceTotals) {
    if let Ok(mut guard) = handles.finished_totals.lock() {
        *guard = Some(totals);
    }
}

/// Drain every queued datagram through the Owner and reap until nothing is in
/// flight. Returns Err if the bound expires: a moving boundary must fail the run,
/// never acknowledge a non-quiescent one.
async fn drain_to_quiescence(
    owner: &mut Owner,
    budget: OwnerServiceBudget,
    start_instant: std::time::Instant,
    totals: &mut ServiceTotals,
) -> Result<(), String> {
    const MAX_DRAIN_VISITS: usize = 100_000;
    // Times out if the drain loop cannot make progress: with no receiver answer
    // possible for this arm, progress is local queue work only.
    for _ in 0..MAX_DRAIN_VISITS {
        let now = Timestamp::from_micros(start_instant.elapsed().as_micros() as u64);
        let report = owner.service(now, budget).await;
        totals.absorb_report_only(&report);
        if owner.tx_in_flight() == 0
            && !report.work_remaining
            && report.tx_packets_submitted == 0
            && report.completions_reaped == 0
        {
            return Ok(());
        }
        owner.wait_for_activity(Duration::from_millis(1)).await;
    }
    Err(format!(
        "stage B quiescence bound exhausted after {MAX_DRAIN_VISITS} visits with in-flight work \
         remaining: tx_in_flight={} (boundary is not quiescent)",
        owner.tx_in_flight()
    ))
}
