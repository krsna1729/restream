//! TCP readiness backends for the RTMP/RTMPS fabric.
//!
//! Production uses one `io_uring` poller per shard. The legacy epoll
//! implementation remains as a deterministic differential seam while the
//! cutover settles. Both use generation-tagged registration and an `Ops`
//! trait so the epoll syscalls can be faked in tests. SRT egress has no
//! equivalent poller:
//! `srt-rs` connections have no epoll-style readiness to multiplex (see
//! `src/media/egress/backends/srt.rs`'s `poll_ready` — every leaf is simply
//! visited each pass), so there is nothing here for it to mirror.

use std::collections::HashMap;
use std::os::raw::c_int;
use std::os::unix::io::RawFd;
use std::time::Duration;

use crate::media::egress::scheduler::LeafKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct TcpEgressInterest {
    pub readable: bool,
    pub writable: bool,
}

impl TcpEgressInterest {
    // Only constructed by tests; production only ever registers WRITE
    // interest (the fabric writes to already-connected TCP sockets).
    #[cfg(test)]
    pub const READ: Self = Self {
        readable: true,
        writable: false,
    };
    pub const WRITE: Self = Self {
        readable: false,
        writable: true,
    };
    #[cfg(test)]
    pub const READ_WRITE: Self = Self {
        readable: true,
        writable: true,
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct TcpReadyLeaf {
    pub fd: RawFd,
    pub key: LeafKey,
    pub generation: u64,
    pub readable: bool,
    pub writable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TcpEgressPollError {
    pub operation: &'static str,
    pub code: c_int,
    pub message: String,
}

impl TcpEgressPollError {
    fn new(operation: &'static str, code: c_int, message: String) -> Self {
        Self {
            operation,
            code,
            message,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TcpRegisteredLeaf {
    key: LeafKey,
    generation: u64,
}

pub(crate) struct TcpEgressPoller<O = LibcTcpPollOps>
where
    O: TcpPollOps,
{
    epoll_fd: RawFd,
    ops: O,
    events: Vec<libc::epoll_event>,
    registered: HashMap<RawFd, TcpRegisteredLeaf>,
}

/// Production RTMP readiness backend. The epoll implementation above remains
/// available to deterministic tests while the native dataplane cutover is
/// staged one protocol at a time.
pub(crate) struct IoUringTcpPoller {
    inner: restream_dataplane::tcp::UringTcpPoller,
    ready: Box<[restream_dataplane::tcp::TcpReadyEvent]>,
    registrations: HashMap<RawFd, u32>,
}

impl IoUringTcpPoller {
    pub(crate) fn new(max_events: usize) -> Result<Self, TcpEgressPollError> {
        let max_events = max_events.max(1);
        let ring_entries = max_events
            .next_power_of_two()
            .max(32)
            .try_into()
            .map_err(|_| {
                TcpEgressPollError::new(
                    "io_uring_setup",
                    libc::EINVAL,
                    "io_uring entry count overflow".to_owned(),
                )
            })?;
        let inner = restream_dataplane::tcp::UringTcpPoller::new(max_events, ring_entries)
            .map_err(|error| {
                TcpEgressPollError::new(
                    "io_uring_setup",
                    error.raw_os_error().unwrap_or(libc::EINVAL),
                    error.to_string(),
                )
            })?;
        Ok(Self {
            inner,
            ready: vec![
                restream_dataplane::tcp::TcpReadyEvent {
                    fd: -1,
                    slot: 0,
                    generation: 0,
                    readable: false,
                    writable: false,
                };
                max_events
            ]
            .into_boxed_slice(),
            registrations: HashMap::with_capacity(max_events),
        })
    }

    pub(crate) fn register_leaf(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        interest: TcpEgressInterest,
    ) -> Result<(), TcpEgressPollError> {
        let generation = u32::try_from(generation).map_err(|_| {
            TcpEgressPollError::new(
                "io_uring_register",
                libc::EINVAL,
                "leaf generation exceeds io_uring tag width".to_owned(),
            )
        })?;
        let slot = u32::try_from(key.0).map_err(|_| {
            TcpEgressPollError::new(
                "io_uring_register",
                libc::EINVAL,
                "leaf slot exceeds io_uring tag width".to_owned(),
            )
        })?;
        self.inner
            .register(
                fd,
                slot,
                generation,
                restream_dataplane::tcp::TcpInterest {
                    readable: interest.readable,
                    writable: interest.writable,
                },
            )
            .map_err(|error| Self::error("io_uring_register", error))?;
        self.registrations.insert(fd, slot);
        Ok(())
    }

    pub(crate) fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError> {
        let Some(slot) = self.registrations.remove(&fd) else {
            return Ok(());
        };
        self.inner
            .remove(slot)
            .map_err(|error| Self::error("io_uring_remove", error))
    }

    pub(crate) fn poll_leaves(
        &mut self,
        timeout_ms: i32,
        ready: &mut Vec<TcpReadyLeaf>,
    ) -> Result<usize, TcpEgressPollError> {
        ready.clear();
        let count = self
            .inner
            .poll(
                Duration::from_millis(timeout_ms.max(0) as u64),
                &mut self.ready,
            )
            .map_err(|error| Self::error("io_uring_poll", error))?;
        for event in self.ready.iter().take(count) {
            ready.push(TcpReadyLeaf {
                fd: event.fd,
                key: LeafKey(event.slot as usize),
                generation: event.generation as u64,
                readable: event.readable,
                writable: event.writable,
            });
        }
        Ok(count)
    }

    fn error(operation: &'static str, error: std::io::Error) -> TcpEgressPollError {
        TcpEgressPollError::new(
            operation,
            error.raw_os_error().unwrap_or(libc::EIO),
            error.to_string(),
        )
    }
}

impl TcpEgressPoller<LibcTcpPollOps> {
    pub(crate) fn new(max_events: usize) -> Result<Self, TcpEgressPollError> {
        Self::with_ops(max_events, LibcTcpPollOps)
    }
}

impl<O> TcpEgressPoller<O>
where
    O: TcpPollOps,
{
    pub(crate) fn with_ops(max_events: usize, ops: O) -> Result<Self, TcpEgressPollError> {
        let epoll_fd = ops.create();
        if epoll_fd < 0 {
            return Err(ops.error("epoll_create1"));
        }

        Ok(Self {
            epoll_fd,
            ops,
            events: vec![empty_event(); max_events.max(1)],
            registered: HashMap::new(),
        })
    }

    /// Register or update interest for `fd`. Registration is keyed by the
    /// leaf's `(key, generation)` so a stale readiness event delivered after
    /// the fd slot has been reused for a newer leaf generation is dropped by
    /// the caller rather than misattributed.
    pub(crate) fn register_leaf(
        &mut self,
        fd: RawFd,
        key: LeafKey,
        generation: u64,
        interest: TcpEgressInterest,
    ) -> Result<(), TcpEgressPollError> {
        let events = events_for(interest);
        let result = if self.registered.contains_key(&fd) {
            self.ops.ctl_mod(self.epoll_fd, fd, events)
        } else {
            self.ops.ctl_add(self.epoll_fd, fd, events)
        };

        if result < 0 {
            return Err(self.ops.error("epoll_ctl"));
        }

        self.registered
            .insert(fd, TcpRegisteredLeaf { key, generation });
        Ok(())
    }

    /// Deregister `fd`. Must be called before the fd is closed: a closed fd
    /// is silently dropped from the epoll set by the kernel, but registering
    /// a *new* fd that happens to reuse the same integer value before this
    /// call would otherwise inherit stale interest.
    pub(crate) fn remove(&mut self, fd: RawFd) -> Result<(), TcpEgressPollError> {
        if !self.registered.contains_key(&fd) {
            return Ok(());
        }

        if self.ops.ctl_del(self.epoll_fd, fd) < 0 {
            return Err(self.ops.error("epoll_ctl_del"));
        }

        self.registered.remove(&fd);
        Ok(())
    }

    pub(crate) fn poll_leaves(
        &mut self,
        timeout_ms: i32,
        ready: &mut Vec<TcpReadyLeaf>,
    ) -> Result<usize, TcpEgressPollError> {
        ready.clear();

        let count = self.ops.wait(self.epoll_fd, &mut self.events, timeout_ms);
        if count < 0 {
            return Err(self.ops.error("epoll_wait"));
        }

        for event in self.events.iter().take(count as usize) {
            let fd = event.u64 as RawFd;
            let Some(registered) = self.registered.get(&fd) else {
                // Deregistered between the wait call returning and this
                // pass (e.g. removed by another visit this same tick).
                continue;
            };
            let readable = (event.events & libc::EPOLLIN as u32) != 0;
            let writable = (event.events & libc::EPOLLOUT as u32) != 0;
            let errored = (event.events & (libc::EPOLLERR | libc::EPOLLHUP) as u32) != 0;
            ready.push(TcpReadyLeaf {
                fd,
                key: registered.key,
                generation: registered.generation,
                // Surface an error/hangup on both directions so the
                // engine's next visit observes the failure via a real
                // read() or send() rather than the poller silently
                // dropping the event.
                readable: readable || errored,
                writable: writable || errored,
            });
        }

        Ok(ready.len())
    }
}

impl<O> Drop for TcpEgressPoller<O>
where
    O: TcpPollOps,
{
    fn drop(&mut self) {
        self.ops.close(self.epoll_fd);
    }
}

fn empty_event() -> libc::epoll_event {
    libc::epoll_event { events: 0, u64: 0 }
}

fn events_for(interest: TcpEgressInterest) -> u32 {
    let mut events = (libc::EPOLLERR | libc::EPOLLHUP) as u32;
    if interest.readable {
        events |= libc::EPOLLIN as u32;
    }
    if interest.writable {
        events |= libc::EPOLLOUT as u32;
    }
    events
}

pub(crate) trait TcpPollOps {
    fn create(&self) -> RawFd;
    fn ctl_add(&self, epoll_fd: RawFd, fd: RawFd, events: u32) -> c_int;
    fn ctl_mod(&self, epoll_fd: RawFd, fd: RawFd, events: u32) -> c_int;
    fn ctl_del(&self, epoll_fd: RawFd, fd: RawFd) -> c_int;
    fn wait(&self, epoll_fd: RawFd, events: &mut [libc::epoll_event], timeout_ms: i32) -> c_int;
    fn close(&self, epoll_fd: RawFd) -> c_int;
    fn error(&self, operation: &'static str) -> TcpEgressPollError;
}

pub(crate) struct LibcTcpPollOps;

impl TcpPollOps for LibcTcpPollOps {
    fn create(&self) -> RawFd {
        // SAFETY: no arguments to validate; returns a fresh epoll fd or -1.
        unsafe { libc::epoll_create1(0) }
    }

    fn ctl_add(&self, epoll_fd: RawFd, fd: RawFd, events: u32) -> c_int {
        ctl(epoll_fd, libc::EPOLL_CTL_ADD, fd, events)
    }

    fn ctl_mod(&self, epoll_fd: RawFd, fd: RawFd, events: u32) -> c_int {
        ctl(epoll_fd, libc::EPOLL_CTL_MOD, fd, events)
    }

    fn ctl_del(&self, epoll_fd: RawFd, fd: RawFd) -> c_int {
        // SAFETY: `epoll_fd` and `fd` are live descriptors owned by the
        // caller; EPOLL_CTL_DEL ignores the event pointer, but the kernel
        // still requires a non-null one on pre-2.6.9 kernels, so pass a
        // valid stack address.
        let mut event = empty_event();
        unsafe { libc::epoll_ctl(epoll_fd, libc::EPOLL_CTL_DEL, fd, &mut event) }
    }

    fn wait(&self, epoll_fd: RawFd, events: &mut [libc::epoll_event], timeout_ms: i32) -> c_int {
        // SAFETY: `events` is a valid buffer for its length; `epoll_fd` is a
        // live descriptor owned by the caller for the duration of the call.
        unsafe {
            libc::epoll_wait(
                epoll_fd,
                events.as_mut_ptr(),
                events.len() as c_int,
                timeout_ms,
            )
        }
    }

    fn close(&self, epoll_fd: RawFd) -> c_int {
        // SAFETY: closes an owned descriptor at most once (called only from
        // `Drop`).
        unsafe { libc::close(epoll_fd) }
    }

    fn error(&self, operation: &'static str) -> TcpEgressPollError {
        let code = std::io::Error::last_os_error().raw_os_error().unwrap_or(-1);
        let message = std::io::Error::last_os_error().to_string();
        TcpEgressPollError::new(operation, code, message)
    }
}

fn ctl(epoll_fd: RawFd, op: c_int, fd: RawFd, events: u32) -> c_int {
    // SAFETY: `epoll_fd` and `fd` are live descriptors owned by the caller.
    // `u64` is set to the raw fd value so `poll_leaves` can recover it from
    // the returned event without a second lookup table; this is safe for
    // any fd since `RawFd` is `i32` and fits losslessly in `u64`.
    let mut event = libc::epoll_event {
        events,
        u64: fd as u64,
    };
    unsafe { libc::epoll_ctl(epoll_fd, op, fd, &mut event) }
}

#[cfg(test)]
#[path = "tcp_tests.rs"]
mod tests;
