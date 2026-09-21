//! The submission arms, held identical in payload, destination sequence,
//! socket model, queue depth and completion semantics.

use std::net::SocketAddr;
use std::os::fd::AsRawFd;
use std::pin::Pin;
use std::sync::atomic::Ordering;

use bytes::Bytes;
use futures_util::stream::{FuturesUnordered, StreamExt};

use super::config::*;
use super::sender::{SenderCounters, SenderHandles};

/// One Compio send in flight: the completion future for a datagram plus the
/// buffer it borrowed. Borrows the socket for the duration of the window.
type SendFuture<'a> = Pin<Box<dyn Future<Output = compio::BufResult<usize, Bytes>> + 'a>>;

/// One fixed send slot: the destination sockaddr, its iovec and the msghdr the
/// ring reads. Wired up in place, never moved afterwards.
struct SendSlot {
    sockaddr: libc::sockaddr_in,
    iovec: libc::iovec,
    msg: libc::msghdr,
}

pub(crate) fn run_compio(
    config: &SubstrateConfig,
    payload: Bytes,
    handles: &SenderHandles,
) -> Result<(), String> {
    let runtime = compio::runtime::Runtime::new().map_err(|e| format!("compio runtime: {e}"))?;
    runtime.block_on(async {
        // Bind through std so both variants get the same socket model (one
        // unconnected wildcard socket, one send buffer size) before Compio
        // adopts the fd.
        let std_socket =
            std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("udp bind: {e}"))?;
        set_send_buffer(std_socket.as_raw_fd(), SEND_BUFFER_BYTES)?;
        let socket = compio::net::UdpSocket::from_std(std_socket)
            .map_err(|e| format!("compio adopt udp socket: {e}"))?;

        let mut in_flight: FuturesUnordered<SendFuture<'_>> = FuturesUnordered::new();
        let mut next = 0_usize;
        while !handles.stop.load(Ordering::Relaxed) {
            handles.wait_if_paused();
            while in_flight.len() < config.queue_depth {
                let destination = config.destinations[next % config.destinations.len()];
                next += 1;
                handles.counters.submitted.fetch_add(1, Ordering::Relaxed);
                in_flight.push(Box::pin(socket.send_to(payload.clone(), destination)));
            }
            handles
                .counters
                .max_in_flight
                .fetch_max(in_flight.len() as u64, Ordering::Relaxed);
            handles.counters.batches.fetch_add(1, Ordering::Relaxed);
            match in_flight.next().await {
                Some(compio::BufResult(result, _)) => {
                    handles.counters.completed.fetch_add(1, Ordering::Relaxed);
                    if let Err(error) = result {
                        handles.counters.errors.fetch_add(1, Ordering::Relaxed);
                        return Err(format!("compio send_to: {error}"));
                    }
                }
                None => break,
            }
        }
        Ok(())
    })
}

/// WI3.6 Stage A control: the Compio pipeline without per-datagram boxing.
///
/// Every send produces the *same* future type, so `FuturesUnordered` holds them
/// directly — the shape pinned srt-rs uses in `udp_datapath_floor` and the shape
/// the production Owner TX machinery eliminated `Box::pin` for. Comparing this
/// arm against the frozen `compio` arm isolates Stage A's own harness overhead
/// from any Owner-side difference.
pub(crate) fn run_compio_pipeline(
    config: &SubstrateConfig,
    payload: Bytes,
    handles: &SenderHandles,
) -> Result<(), String> {
    let runtime = compio::runtime::Runtime::new().map_err(|e| format!("compio runtime: {e}"))?;
    runtime.block_on(async {
        let std_socket =
            std::net::UdpSocket::bind("0.0.0.0:0").map_err(|e| format!("udp bind: {e}"))?;
        set_send_buffer(std_socket.as_raw_fd(), SEND_BUFFER_BYTES)?;
        let socket = compio::net::UdpSocket::from_std(std_socket)
            .map_err(|e| format!("compio adopt udp socket: {e}"))?;

        // No type annotation and no boxing: the futures are homogeneous.
        let mut in_flight = FuturesUnordered::new();
        let mut next = 0_usize;
        while !handles.stop.load(Ordering::Relaxed) {
            handles.wait_if_paused();
            while in_flight.len() < config.queue_depth {
                let destination = config.destinations[next % config.destinations.len()];
                next += 1;
                handles.counters.submitted.fetch_add(1, Ordering::Relaxed);
                in_flight.push(socket.send_to(payload.clone(), destination));
            }
            handles
                .counters
                .max_in_flight
                .fetch_max(in_flight.len() as u64, Ordering::Relaxed);
            handles.counters.batches.fetch_add(1, Ordering::Relaxed);
            match in_flight.next().await {
                Some(compio::BufResult(result, _)) => {
                    handles.counters.completed.fetch_add(1, Ordering::Relaxed);
                    if let Err(error) = result {
                        handles.counters.errors.fetch_add(1, Ordering::Relaxed);
                        return Err(format!("compio send_to: {error}"));
                    }
                }
                None => break,
            }
        }
        Ok(())
    })
}

/// Variant B: a purpose-built native ring. One `IoUring`, a fixed window of
/// `SendTo` SQEs with preconstructed sockaddrs, one `submit()` per batch and a
/// blocking enter only when the completion queue came back empty.
pub(crate) fn run_io_uring(
    config: &SubstrateConfig,
    payload: Bytes,
    handles: &SenderHandles,
) -> Result<(), String> {
    use io_uring::{IoUring, opcode, types};

    let entries = config.queue_depth.next_power_of_two().max(8) as u32;
    let mut ring = IoUring::new(entries).map_err(|e| format!("io_uring setup: {e}"))?;
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!("socket(): {}", std::io::Error::last_os_error()));
    }
    set_send_buffer(fd, SEND_BUFFER_BYTES)?;
    // Fixed slots: one preconstructed msghdr + iovec per destination, wired to
    // the shared payload after the vector can no longer reallocate.
    let mut slots: Vec<SendSlot> = Vec::with_capacity(config.destinations.len());
    for destination in &config.destinations {
        let SocketAddr::V4(v4) = destination else {
            unreachable!("destinations are IPv4");
        };
        slots.push(SendSlot {
            sockaddr: libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from(*v4.ip()).to_be(),
                },
                sin_zero: [0; 8],
            },
            iovec: libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            },
            msg: unsafe { std::mem::zeroed() },
        });
    }
    for slot in slots.iter_mut() {
        slot.iovec = libc::iovec {
            iov_base: payload.as_ptr() as *mut libc::c_void,
            iov_len: payload.len(),
        };
        let mut msg: libc::msghdr = unsafe { std::mem::zeroed() };
        msg.msg_name = &slot.sockaddr as *const libc::sockaddr_in as *mut libc::c_void;
        msg.msg_namelen = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        msg.msg_iov = &slot.iovec as *const libc::iovec as *mut libc::iovec;
        msg.msg_iovlen = 1;
        slot.msg = msg;
    }

    let mut in_flight = 0_usize;
    let mut next = 0_usize;
    let mut result: Result<(), String> = Ok(());

    while !handles.stop.load(Ordering::Relaxed) && result.is_ok() {
        handles.wait_if_paused();
        let mut pushed = 0_usize;
        while in_flight < config.queue_depth && pushed < entries as usize {
            let slot = &slots[next % slots.len()];
            next += 1;
            let entry = opcode::SendMsg::new(types::Fd(fd), &slot.msg as *const libc::msghdr)
                .build()
                .user_data(pushed as u64);
            // Safety: the payload and sockaddr live for the whole loop, and the
            // SQ has room because the window never exceeds the ring size.
            unsafe {
                if let Err(error) = ring.submission().push(&entry) {
                    result = Err(format!("io_uring submission queue full: {error}"));
                    break;
                }
            }
            pushed += 1;
            in_flight += 1;
        }
        if pushed > 0 {
            handles
                .counters
                .submitted
                .fetch_add(pushed as u64, Ordering::Relaxed);
            handles
                .counters
                .max_in_flight
                .fetch_max(in_flight as u64, Ordering::Relaxed);
            handles.counters.batches.fetch_add(1, Ordering::Relaxed);
        }
        if pushed > 0 {
            ring.submit().map_err(|e| format!("io_uring submit: {e}"))?;
            handles.counters.ring_enters.fetch_add(1, Ordering::Relaxed);
        }
        // Sliding window, matching the Compio variant: one completion per
        // iteration, one refill, then submit again. Reaping the whole window at
        // once would be a different completion pattern and a different
        // experiment.
        let mut reaped = match config.reap_mode {
            ReapMode::Sliding => reap_one(&mut ring, &handles.counters, &mut result),
            ReapMode::Window => reap_ready(&mut ring, &handles.counters, &mut result),
        };
        if reaped == 0 && in_flight > 0 {
            ring.submit_and_wait(1)
                .map_err(|e| format!("io_uring submit_and_wait: {e}"))?;
            handles.counters.ring_enters.fetch_add(1, Ordering::Relaxed);
            reaped = match config.reap_mode {
                ReapMode::Sliding => reap_one(&mut ring, &handles.counters, &mut result),
                ReapMode::Window => reap_ready(&mut ring, &handles.counters, &mut result),
            };
        }
        in_flight = in_flight.saturating_sub(reaped);
    }

    // Drain anything still in flight so the counts are complete.
    while in_flight > 0 {
        let _ = ring.submit_and_wait(1);
        let reaped = reap_one(&mut ring, &handles.counters, &mut result);
        if reaped == 0 {
            break;
        }
        in_flight = in_flight.saturating_sub(reaped);
    }
    unsafe { libc::close(fd) };
    result
}

/// Variant C: blocking `libc::sendto` in this process. Same payload, same
/// preconstructed destination array, same socket options, same one-CPU
/// measurement machinery — the control that decides whether a measured rate is
/// a submission-API property or the kernel/UDP stack.
pub(crate) fn run_sendto(
    config: &SubstrateConfig,
    payload: Bytes,
    handles: &SenderHandles,
) -> Result<(), String> {
    let fd = unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
    if fd < 0 {
        return Err(format!("socket(): {}", std::io::Error::last_os_error()));
    }
    set_send_buffer(fd, SEND_BUFFER_BYTES)?;
    let sockaddrs: Vec<libc::sockaddr_in> = config
        .destinations
        .iter()
        .map(|destination| match destination {
            SocketAddr::V4(v4) => libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: v4.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from(*v4.ip()).to_be(),
                },
                sin_zero: [0; 8],
            },
            SocketAddr::V6(_) => unreachable!("destinations are IPv4"),
        })
        .collect();
    let sockaddr_len = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
    let mut next = 0_usize;
    let mut result = Ok(());
    while !handles.stop.load(Ordering::Relaxed) && result.is_ok() {
        handles.wait_if_paused();
        let destination = &sockaddrs[next % sockaddrs.len()];
        next += 1;
        handles.counters.submitted.fetch_add(1, Ordering::Relaxed);
        // Safety: the payload and the destination live for the whole loop, and
        // sendto only reads them.
        let rc = unsafe {
            libc::sendto(
                fd,
                payload.as_ptr() as *const libc::c_void,
                payload.len(),
                0,
                destination as *const libc::sockaddr_in as *const libc::sockaddr,
                sockaddr_len,
            )
        };
        if rc >= 0 {
            handles.counters.completed.fetch_add(1, Ordering::Relaxed);
        } else {
            handles.counters.errors.fetch_add(1, Ordering::Relaxed);
            result = Err(format!("sendto: {}", std::io::Error::last_os_error()));
        }
    }
    unsafe { libc::close(fd) };
    result
}

/// Consume every completion the ring has ready, recording their results.
fn reap_ready(
    ring: &mut io_uring::IoUring,
    counters: &SenderCounters,
    result: &mut Result<(), String>,
) -> usize {
    let mut reaped = 0;
    while reap_one(ring, counters, result) == 1 {
        reaped += 1;
    }
    reaped
}

/// Consume at most one completion from the ring, recording its result. Returns
/// 1 when a completion was reaped, 0 when the queue was empty.
fn reap_one(
    ring: &mut io_uring::IoUring,
    counters: &SenderCounters,
    result: &mut Result<(), String>,
) -> usize {
    let mut completion = ring.completion();
    completion.sync();
    let Some(entry) = completion.next() else {
        return 0;
    };
    counters.completed.fetch_add(1, Ordering::Relaxed);
    let rc = entry.result();
    if rc < 0 {
        counters.errors.fetch_add(1, Ordering::Relaxed);
        *result = Err(format!(
            "io_uring sendmsg: {}",
            std::io::Error::from_raw_os_error(-rc)
        ));
    }
    1
}
