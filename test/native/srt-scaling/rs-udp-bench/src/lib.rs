//! Shared helpers for the Rust raw-UDP scaling benchmarks, mirroring the
//! primitives `udp_sender.c`/`udp_sink.c` converged on after several
//! iterations (see `test/native/srt-scaling/README.md`): CPU-pinned worker
//! threads, `rdtscp`-based timing calibrated against `CLOCK_MONOTONIC`, and
//! a fixed payload size matching SRT's live-mode ceiling for a fair
//! comparison. x86_64/Linux only, same assumption the C tools already make.

use std::arch::x86_64::__rdtscp;
use std::mem::{self, MaybeUninit};
use std::time::{Duration, Instant};

/// Matches libsrt's live-mode payload ceiling and the C benchmarks'
/// `PAYLOAD_SIZE`, so results are comparable apples-to-apples.
pub const PAYLOAD_SIZE: usize = 1316;
pub const WHEEL_SLOTS: usize = 64;

pub fn pin_to_cpu(cpu: usize) {
    unsafe {
        let nproc = libc::sysconf(libc::_SC_NPROCESSORS_ONLN);
        if nproc <= 0 {
            return;
        }
        let cpu = cpu % nproc as usize;
        let mut set: libc::cpu_set_t = mem::zeroed();
        libc::CPU_ZERO(&mut set);
        libc::CPU_SET(cpu, &mut set);
        libc::sched_setaffinity(0, mem::size_of::<libc::cpu_set_t>(), &set);
    }
}

#[inline(always)]
pub fn rdtsc_now() -> u64 {
    unsafe {
        let mut aux: u32 = 0;
        __rdtscp(&mut aux)
    }
}

/// Calibrate TSC frequency against `CLOCK_MONOTONIC` once at startup, same
/// approach as `udp_sender.c`'s `calibrate_tsc_hz()`. Call this from the
/// pinned thread that will use the result, so migration doesn't skew it.
pub fn calibrate_tsc_hz() -> u64 {
    let t0 = Instant::now();
    let tsc0 = rdtsc_now();
    std::thread::sleep(Duration::from_millis(50));
    let elapsed = t0.elapsed();
    let tsc1 = rdtsc_now();
    let elapsed_s = elapsed.as_secs_f64();
    ((tsc1 - tsc0) as f64 / elapsed_s) as u64
}

/// Build an `AF_INET` UDP socket. `connect_to`, if given, connects the
/// socket (used by the receive side's per-peer isolation experiments, not
/// the batched sender, which stays unconnected so it can address a
/// different peer per message within one `sendmmsg()` batch).
pub fn make_udp_socket() -> i32 {
    unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0) }
}

pub fn set_reuseaddr(fd: i32) {
    unsafe {
        let one: i32 = 1;
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_REUSEADDR,
            &one as *const _ as *const libc::c_void,
            mem::size_of::<i32>() as u32,
        );
    }
}

pub fn set_rcvbuf(fd: i32, bytes: i32) {
    if bytes <= 0 {
        return;
    }
    unsafe {
        libc::setsockopt(
            fd,
            libc::SOL_SOCKET,
            libc::SO_RCVBUF,
            &bytes as *const _ as *const libc::c_void,
            mem::size_of::<i32>() as u32,
        );
    }
}

pub fn sockaddr_in(ip_be: u32, port: u16) -> libc::sockaddr_in {
    let mut sa: libc::sockaddr_in = unsafe { mem::zeroed() };
    sa.sin_family = libc::AF_INET as libc::sa_family_t;
    sa.sin_port = port.to_be();
    sa.sin_addr.s_addr = ip_be;
    sa
}

/// Returns the address already in network byte order, ready to store
/// directly into `sockaddr_in.sin_addr.s_addr`.
pub fn parse_ipv4(host: &str) -> u32 {
    let addr: std::net::Ipv4Addr = host.parse().expect("invalid IPv4 address");
    u32::from_ne_bytes(addr.octets())
}

pub fn bind_v4(fd: i32, port: u16) -> bool {
    let sa = sockaddr_in(libc::INADDR_ANY, port);
    let r = unsafe {
        libc::bind(
            fd,
            &sa as *const _ as *const libc::sockaddr,
            mem::size_of::<libc::sockaddr_in>() as u32,
        )
    };
    r == 0
}

/// One outstanding `sendmmsg()` batch buffer, pre-allocated once and reused
/// every wheel-slot fire: this is the "best of Rust" lever the C tools
/// never got to (see their README's "what's still open" section) — turning
/// N per-connection `send()` syscalls into one syscall per thread per tick.
/// `iov_base` is set once, up front, to a shared read-only payload buffer:
/// every simulated stream sends bit-identical content, so there's nothing
/// to fill in per message except the destination address.
pub struct SendBatch {
    msgs: Vec<libc::mmsghdr>,
    addrs: Vec<libc::sockaddr_in>,
    iovecs: Vec<libc::iovec>,
    len: usize,
}

impl SendBatch {
    pub fn new(capacity: usize, payload: &'static [u8; PAYLOAD_SIZE]) -> Self {
        let iovecs: Vec<libc::iovec> = (0..capacity)
            .map(|_| libc::iovec {
                iov_base: payload.as_ptr() as *mut libc::c_void,
                iov_len: PAYLOAD_SIZE,
            })
            .collect();
        let addrs = vec![unsafe { mem::zeroed::<libc::sockaddr_in>() }; capacity];
        let msgs = vec![unsafe { mem::zeroed::<libc::mmsghdr>() }; capacity];
        SendBatch { msgs, addrs, iovecs, len: 0 }
    }

    #[inline]
    pub fn clear(&mut self) {
        self.len = 0;
    }

    /// Queue one destination into the batch. Safe: `addrs`/`iovecs`/`msgs`
    /// are all pre-sized to capacity by `new()`, and the pointers stored in
    /// `msgs[i]` are fixed up right before the syscall in `send`, once the
    /// backing `Vec`s can no longer reallocate (no push after this point).
    #[inline]
    pub fn push(&mut self, dest: libc::sockaddr_in) {
        self.addrs[self.len] = dest;
        self.len += 1;
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.len >= self.addrs.len()
    }

    #[inline]
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    #[inline]
    pub fn len(&self) -> usize {
        self.len
    }

    /// Fire the batch as one `sendmmsg()` call. Returns
    /// `(messages_sent, bytes_sent)`; on `EAGAIN`/error the whole remaining
    /// batch is reported as not-sent (matches the per-message `EAGAIN`
    /// accounting the C sender does).
    pub fn send(&mut self, fd: i32) -> (usize, u64) {
        if self.len == 0 {
            return (0, 0);
        }
        for i in 0..self.len {
            self.msgs[i].msg_hdr.msg_name = &mut self.addrs[i] as *mut _ as *mut libc::c_void;
            self.msgs[i].msg_hdr.msg_namelen = mem::size_of::<libc::sockaddr_in>() as u32;
            self.msgs[i].msg_hdr.msg_iov = &mut self.iovecs[i] as *mut _;
            self.msgs[i].msg_hdr.msg_iovlen = 1;
            self.msgs[i].msg_hdr.msg_control = std::ptr::null_mut();
            self.msgs[i].msg_hdr.msg_controllen = 0;
            self.msgs[i].msg_hdr.msg_flags = 0;
            self.msgs[i].msg_len = 0;
        }
        let r = unsafe {
            libc::sendmmsg(fd, self.msgs.as_mut_ptr(), self.len as u32, libc::MSG_DONTWAIT)
        };
        if r <= 0 {
            return (0, 0);
        }
        let sent = r as usize;
        let mut bytes = 0u64;
        for m in &self.msgs[..sent] {
            bytes += m.msg_len as u64;
        }
        (sent, bytes)
    }

    /// Control path for isolating the batching variable: same shared,
    /// unconnected per-thread socket as `send`, but one `sendto()` syscall
    /// per queued message instead of one `sendmmsg()` for the whole batch.
    /// Architecture otherwise identical to `send` -- this is what separates
    /// "batching helped" from "the shared-socket-per-thread design helped"
    /// when compared against both `send` and the C tools' per-stream-socket
    /// design.
    pub fn send_unbatched(&mut self, fd: i32) -> (usize, u64) {
        let mut sent = 0usize;
        let mut bytes = 0u64;
        for i in 0..self.len {
            let r = unsafe {
                libc::sendto(
                    fd,
                    self.iovecs[i].iov_base,
                    PAYLOAD_SIZE,
                    libc::MSG_DONTWAIT,
                    &self.addrs[i] as *const _ as *const libc::sockaddr,
                    mem::size_of::<libc::sockaddr_in>() as u32,
                )
            };
            if r > 0 {
                sent += 1;
                bytes += r as u64;
            }
        }
        (sent, bytes)
    }
}

/// Pre-allocated `recvmmsg()` batch: `capacity` independent scratch buffers
/// (one per potential message, unlike the sender's shared read-only
/// buffer) so the kernel can deliver distinct datagrams into each slot in
/// one syscall.
pub struct RecvBatch {
    msgs: Vec<libc::mmsghdr>,
    iovecs: Vec<libc::iovec>,
    bufs: Vec<u8>,
    buf_stride: usize,
    peer_addrs: Vec<libc::sockaddr_in>,
    capacity: usize,
}

impl RecvBatch {
    pub fn new(capacity: usize, buf_stride: usize) -> Self {
        let mut bufs = vec![0u8; capacity * buf_stride];
        let mut iovecs = vec![unsafe { mem::zeroed::<libc::iovec>() }; capacity];
        for i in 0..capacity {
            iovecs[i].iov_base = unsafe { bufs.as_mut_ptr().add(i * buf_stride) } as *mut libc::c_void;
            iovecs[i].iov_len = buf_stride;
        }
        let peer_addrs = vec![unsafe { mem::zeroed::<libc::sockaddr_in>() }; capacity];
        let msgs = vec![unsafe { mem::zeroed::<libc::mmsghdr>() }; capacity];
        RecvBatch { msgs, iovecs, bufs, buf_stride, peer_addrs, capacity }
    }

    /// Drain everything currently queued on `fd` via `recvmmsg()`, looping
    /// batch-by-batch until `EAGAIN`. Returns `(messages, bytes)`.
    pub fn drain(&mut self, fd: i32) -> (u64, u64) {
        let mut total_msgs = 0u64;
        let mut total_bytes = 0u64;
        loop {
            for i in 0..self.capacity {
                self.msgs[i].msg_hdr.msg_name = &mut self.peer_addrs[i] as *mut _ as *mut libc::c_void;
                self.msgs[i].msg_hdr.msg_namelen = mem::size_of::<libc::sockaddr_in>() as u32;
                self.msgs[i].msg_hdr.msg_iov = &mut self.iovecs[i] as *mut _;
                self.msgs[i].msg_hdr.msg_iovlen = 1;
                self.msgs[i].msg_hdr.msg_control = std::ptr::null_mut();
                self.msgs[i].msg_hdr.msg_controllen = 0;
                self.msgs[i].msg_hdr.msg_flags = 0;
                self.msgs[i].msg_len = 0;
            }
            let r = unsafe {
                libc::recvmmsg(
                    fd,
                    self.msgs.as_mut_ptr(),
                    self.capacity as u32,
                    libc::MSG_DONTWAIT,
                    std::ptr::null_mut(),
                )
            };
            if r <= 0 {
                break;
            }
            let n = r as usize;
            total_msgs += n as u64;
            for m in &self.msgs[..n] {
                total_bytes += m.msg_len as u64;
            }
            if n < self.capacity {
                // Short batch: queue almost certainly drained.
                break;
            }
        }
        // buf_stride kept for documentation/debug use even though the hot
        // path never re-reads message contents (throughput-only benchmark).
        let _ = &self.bufs;
        let _ = self.buf_stride;
        (total_msgs, total_bytes)
    }

    /// Control path matching `SendBatch::send_unbatched`: one `recvfrom()`
    /// syscall per message instead of one `recvmmsg()` per batch, same
    /// shared-listener-socket architecture otherwise.
    pub fn drain_unbatched(&mut self, fd: i32) -> (u64, u64) {
        let mut total_msgs = 0u64;
        let mut total_bytes = 0u64;
        let buf_ptr = self.iovecs[0].iov_base;
        loop {
            let r = unsafe {
                libc::recvfrom(
                    fd,
                    buf_ptr,
                    self.buf_stride,
                    libc::MSG_DONTWAIT,
                    std::ptr::null_mut(),
                    std::ptr::null_mut(),
                )
            };
            if r <= 0 {
                break;
            }
            total_msgs += 1;
            total_bytes += r as u64;
        }
        (total_msgs, total_bytes)
    }
}

pub fn epoll_create() -> i32 {
    unsafe { libc::epoll_create1(0) }
}

pub fn epoll_add_readable(epfd: i32, fd: i32) {
    let mut ev = MaybeUninit::<libc::epoll_event>::zeroed();
    unsafe {
        (*ev.as_mut_ptr()).events = libc::EPOLLIN as u32;
        (*ev.as_mut_ptr()).u64 = fd as u64;
        libc::epoll_ctl(epfd, libc::EPOLL_CTL_ADD, fd, ev.as_mut_ptr());
    }
}
