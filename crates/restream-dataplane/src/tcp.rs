//! One-owner TCP readiness over `io_uring`.

use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

use io_uring::{IoUring, opcode, types};

use crate::{MAX_TAG_SLOTS, OpKind, OpTag, build_ring};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct TcpInterest {
    pub readable: bool,
    pub writable: bool,
}

impl TcpInterest {
    pub const READ: Self = Self {
        readable: true,
        writable: false,
    };

    pub const WRITE: Self = Self {
        readable: false,
        writable: true,
    };

    pub const READ_WRITE: Self = Self {
        readable: true,
        writable: true,
    };

    pub fn is_empty(self) -> bool {
        !self.readable && !self.writable
    }

    fn poll_flags(self) -> u32 {
        let mut flags = 0;
        if self.readable {
            flags |= libc::POLLIN as u32;
        }
        if self.writable {
            flags |= libc::POLLOUT as u32;
        }
        flags | libc::POLLERR as u32 | libc::POLLHUP as u32
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpReadyEvent {
    pub fd: RawFd,
    pub slot: u32,
    pub generation: u32,
    pub readable: bool,
    pub writable: bool,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TcpPollerMetrics {
    pub completions: u64,
    pub stale_completions: u64,
    pub ready_overflows: u64,
    pub poll_errors: u64,
}

/// One accepted socket returned by the owner-thread acceptor.
#[derive(Debug)]
pub struct AcceptedTcp {
    fd: OwnedFd,
}

impl AcceptedTcp {
    /// Take ownership of the accepted socket as a nonblocking standard
    /// library stream. The caller remains responsible for runtime adoption.
    pub fn into_std(self) -> std::net::TcpStream {
        self.fd.into()
    }
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct TcpAcceptorMetrics {
    pub accepted: u64,
    pub errors: u64,
    pub stale_completions: u64,
}

/// Single-owner TCP acceptor backed by one `io_uring`.
///
/// The accepted socket is returned with `CLOEXEC|NONBLOCK` already set, so a
/// bounded handoff can adopt it into an async runtime without a second setup
/// syscall or a blocking accept loop.
pub struct UringTcpAcceptor {
    ring: IoUring,
    listener_fd: RawFd,
    armed: bool,
    generation: u32,
    metrics: TcpAcceptorMetrics,
}

impl UringTcpAcceptor {
    pub fn new(listener_fd: RawFd, ring_entries: u32) -> io::Result<Self> {
        if !ring_entries.is_power_of_two() || ring_entries < 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "io_uring entries must be a power of two >= 8",
            ));
        }
        Ok(Self {
            ring: build_ring(ring_entries)?,
            listener_fd,
            armed: false,
            generation: 0,
            metrics: TcpAcceptorMetrics::default(),
        })
    }

    /// Wait for one connection and write it into caller-owned storage.
    pub fn accept(&mut self, accepted: &mut [Option<AcceptedTcp>]) -> io::Result<usize> {
        if accepted.is_empty() {
            return Ok(0);
        }
        if !self.armed {
            self.generation = self.generation.wrapping_add(1);
            let tag = OpTag::new(OpKind::Accept, 0, self.generation)
                .expect("accept tag slot is fixed")
                .encode();
            let entry = opcode::Accept::new(
                types::Fd(self.listener_fd),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
            )
            .flags(libc::SOCK_CLOEXEC | libc::SOCK_NONBLOCK)
            .build()
            .user_data(tag);
            unsafe { self.push(&entry)? };
            self.armed = true;
        }

        self.ring.submit_and_wait(1)?;
        let mut count = 0;
        {
            let cq = self.ring.completion();
            for completion in cq {
                let Some(tag) = OpTag::decode(completion.user_data()) else {
                    self.metrics.stale_completions += 1;
                    continue;
                };
                if tag.kind != OpKind::Accept || tag.generation != self.generation {
                    self.metrics.stale_completions += 1;
                    continue;
                }
                self.armed = false;
                let result = completion.result();
                if result < 0 {
                    self.metrics.errors += 1;
                    let error = io::Error::from_raw_os_error(-result);
                    if matches!(
                        error.raw_os_error(),
                        Some(libc::EINTR | libc::ECONNABORTED | libc::EPROTO)
                    ) {
                        return Ok(0);
                    }
                    return Err(error);
                }
                let fd = unsafe { OwnedFd::from_raw_fd(result) };
                accepted[count] = Some(AcceptedTcp { fd });
                count += 1;
                self.metrics.accepted += 1;
                break;
            }
        }
        Ok(count)
    }

    pub fn metrics(&self) -> TcpAcceptorMetrics {
        self.metrics
    }

    unsafe fn push(&mut self, entry: &io_uring::squeue::Entry) -> io::Result<()> {
        unsafe { self.ring.submission().push(entry) }
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full"))
    }
}

#[derive(Debug, Clone, Copy)]
struct Registration {
    fd: RawFd,
    generation: u32,
    armed: bool,
}

/// Single-owner, fixed-registration TCP readiness poller.
///
/// Registration and completion state are mutated only by the thread that
/// owns this value. `poll` writes into caller-owned storage so the ready path
/// does not need a `Vec` growth or a population scan.
pub struct UringTcpPoller {
    ring: IoUring,
    registrations: Box<[Option<Registration>]>,
    timeout_armed: bool,
    metrics: TcpPollerMetrics,
}

impl UringTcpPoller {
    pub fn new(max_slots: usize, ring_entries: u32) -> io::Result<Self> {
        if max_slots == 0 || max_slots >= MAX_TAG_SLOTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid TCP registration capacity",
            ));
        }
        if !ring_entries.is_power_of_two() || ring_entries < 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "io_uring entries must be a power of two >= 8",
            ));
        }
        Ok(Self {
            ring: build_ring(ring_entries)?,
            registrations: vec![None; max_slots].into_boxed_slice(),
            timeout_armed: false,
            metrics: TcpPollerMetrics::default(),
        })
    }

    pub fn register(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u32,
        interest: TcpInterest,
    ) -> io::Result<()> {
        let slot_index = self.registration_index(slot)?;
        if let Some(previous) = self.registrations[slot_index]
            && previous.armed
        {
            self.cancel_poll(slot, previous.generation)?;
        }
        self.registrations[slot_index] = Some(Registration {
            fd,
            generation,
            armed: !interest.is_empty(),
        });
        if !interest.is_empty() {
            self.push_poll(fd, slot, generation, interest)?;
        }
        Ok(())
    }

    pub fn remove(&mut self, slot: u32) -> io::Result<()> {
        let slot_index = self.registration_index(slot)?;
        let Some(previous) = self.registrations[slot_index].take() else {
            return Ok(());
        };
        if previous.armed {
            self.cancel_poll(slot, previous.generation)?;
        }
        Ok(())
    }

    pub fn poll(&mut self, timeout: Duration, ready: &mut [TcpReadyEvent]) -> io::Result<usize> {
        let timespec = types::Timespec::from(timeout);
        if !timeout.is_zero() && !self.timeout_armed {
            let entry = opcode::Timeout::new(&timespec)
                .build()
                .user_data(OpTag::new(OpKind::Timeout, 0, 0).unwrap().encode());
            unsafe { self.push(&entry)? };
            self.timeout_armed = true;
        }
        self.ring.submit_and_wait(usize::from(!timeout.is_zero()))?;

        let mut ready_count = 0;
        {
            let cq = self.ring.completion();
            for completion in cq {
                self.metrics.completions += 1;
                let Some(tag) = OpTag::decode(completion.user_data()) else {
                    self.metrics.stale_completions += 1;
                    continue;
                };
                match tag.kind {
                    OpKind::Timeout => {
                        self.timeout_armed = false;
                    }
                    OpKind::PollCancel => {}
                    _ => {
                        let Some(registration) = self
                            .registrations
                            .get_mut(tag.slot as usize)
                            .and_then(Option::as_mut)
                        else {
                            self.metrics.stale_completions += 1;
                            continue;
                        };
                        if registration.generation != tag.generation {
                            self.metrics.stale_completions += 1;
                            continue;
                        }
                        registration.armed = false;
                        if completion.result() < 0 {
                            self.metrics.poll_errors += 1;
                            return Err(io::Error::from_raw_os_error(-completion.result()));
                        }
                        let events = completion.result() as u32;
                        let errored = events & (libc::POLLERR | libc::POLLHUP) as u32 != 0;
                        if ready_count == ready.len() {
                            self.metrics.ready_overflows += 1;
                            continue;
                        }
                        ready[ready_count] = TcpReadyEvent {
                            fd: registration.fd,
                            slot: tag.slot,
                            generation: tag.generation,
                            readable: errored || events & libc::POLLIN as u32 != 0,
                            writable: errored || events & libc::POLLOUT as u32 != 0,
                        };
                        ready_count += 1;
                    }
                }
            }
        }

        if self.timeout_armed {
            let entry =
                opcode::TimeoutRemove::new(OpTag::new(OpKind::Timeout, 0, 0).unwrap().encode())
                    .build()
                    .user_data(OpTag::new(OpKind::PollCancel, 0, 0).unwrap().encode());
            unsafe { self.push(&entry)? };
            self.ring.submit()?;
            self.timeout_armed = false;
        }
        Ok(ready_count)
    }

    pub fn metrics(&self) -> TcpPollerMetrics {
        self.metrics
    }

    fn registration_index(&self, slot: u32) -> io::Result<usize> {
        let index = slot as usize;
        if index >= self.registrations.len() {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TCP registration slot out of range",
            ))
        } else {
            Ok(index)
        }
    }

    fn push_poll(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u32,
        interest: TcpInterest,
    ) -> io::Result<()> {
        let entry = opcode::PollAdd::new(types::Fd(fd), interest.poll_flags())
            .build()
            .user_data(
                OpTag::new(OpKind::TcpRx, slot, generation)
                    .unwrap()
                    .encode(),
            );
        unsafe { self.push(&entry) }
    }

    fn cancel_poll(&mut self, slot: u32, generation: u32) -> io::Result<()> {
        let target = OpTag::new(OpKind::TcpRx, slot, generation)
            .unwrap()
            .encode();
        let entry = opcode::PollRemove::new(target).build().user_data(
            OpTag::new(OpKind::PollCancel, slot, generation)
                .unwrap()
                .encode(),
        );
        unsafe { self.push(&entry) }
    }

    unsafe fn push(&mut self, entry: &io_uring::squeue::Entry) -> io::Result<()> {
        unsafe { self.ring.submission().push(entry) }
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    fn socket_pair() -> Option<(OwnedFd, OwnedFd)> {
        let mut fds = [0; 2];
        let result = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_STREAM | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        };
        (result == 0)
            .then(|| unsafe { (OwnedFd::from_raw_fd(fds[0]), OwnedFd::from_raw_fd(fds[1])) })
    }

    #[test]
    fn acceptor_returns_nonblocking_loopback_socket() {
        let listener = match std::net::TcpListener::bind("127.0.0.1:0") {
            Ok(listener) => listener,
            Err(_) => return,
        };
        let address = listener.local_addr().unwrap();
        let _client = std::net::TcpStream::connect(address).unwrap();
        let mut acceptor = match UringTcpAcceptor::new(listener.as_raw_fd(), 32) {
            Ok(acceptor) => acceptor,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                ) =>
            {
                return;
            }
            Err(error) => panic!("io_uring unavailable: {error}"),
        };
        let mut accepted = [None];
        assert_eq!(acceptor.accept(&mut accepted).unwrap(), 1);
        let stream = accepted[0].take().unwrap().into_std();
        stream.set_nonblocking(true).unwrap();
        assert!(stream.peer_addr().is_ok());
        assert_eq!(acceptor.metrics().accepted, 1);
    }

    #[test]
    fn poll_reports_socket_readiness_with_generation() {
        let Some((receiver, sender)) = socket_pair() else {
            return;
        };
        let mut poller = match UringTcpPoller::new(4, 32) {
            Ok(poller) => poller,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                ) =>
            {
                return;
            }
            Err(error) => panic!("io_uring unavailable: {error}"),
        };
        poller
            .register(receiver.as_raw_fd(), 2, 9, TcpInterest::READ)
            .unwrap();
        let byte = [7_u8];
        let written = unsafe {
            libc::write(
                sender.as_raw_fd(),
                byte.as_ptr().cast::<libc::c_void>(),
                byte.len(),
            )
        };
        assert_eq!(written, 1);
        let mut ready = [TcpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        }; 4];
        assert_eq!(poller.poll(Duration::ZERO, &mut ready).unwrap(), 1);
        assert_eq!(ready[0].slot, 2);
        assert_eq!(ready[0].generation, 9);
        assert!(ready[0].readable);
    }

    #[test]
    fn stale_completion_is_ignored_after_slot_reuse() {
        let Some((receiver, sender)) = socket_pair() else {
            return;
        };
        let mut poller = match UringTcpPoller::new(2, 32) {
            Ok(poller) => poller,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                ) =>
            {
                return;
            }
            Err(error) => panic!("io_uring unavailable: {error}"),
        };
        poller
            .register(receiver.as_raw_fd(), 0, 1, TcpInterest::READ)
            .unwrap();
        poller.remove(0).unwrap();
        poller
            .register(receiver.as_raw_fd(), 0, 2, TcpInterest::READ)
            .unwrap();
        let byte = [8_u8];
        assert_eq!(
            unsafe {
                libc::write(
                    sender.as_raw_fd(),
                    byte.as_ptr().cast::<libc::c_void>(),
                    byte.len(),
                )
            },
            1
        );
        let mut ready = [TcpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        }; 2];
        let count = poller.poll(Duration::ZERO, &mut ready).unwrap();
        assert_eq!(count, 1);
        assert_eq!(ready[0].generation, 2);
    }
}
