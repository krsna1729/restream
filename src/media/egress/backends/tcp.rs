//! Raw-epoll TCP readiness backend for the RTMP/RTMPS fabric.
//!
//! Mirrors `src/media/srt/egress_poller.rs`'s shape (one epoll container per
//! shard, generation-tagged registration, an `Ops` trait so the native
//! syscalls can be faked in tests) but talks to a real Linux `epoll` instance
//! via `libc` instead of libsrt's own epoll wrapper, since a TCP fd has no
//! native poller of its own.

use std::collections::HashMap;
use std::os::raw::c_int;
use std::os::unix::io::RawFd;

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
