//! One-owner TCP readiness over `io_uring`.

use std::collections::VecDeque;
use std::io;
use std::os::fd::{FromRawFd, OwnedFd, RawFd};
use std::time::Duration;

use io_uring::{IoUring, opcode, types};

use crate::{FixedFile, FixedFileTable, MAX_TAG_SLOTS, OpKind, OpTag, build_ring};

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

/// Completion of one owner-thread TCP send. The caller retains the submitted
/// buffer until this result is drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TcpSendCompletion {
    pub slot: u32,
    pub generation: u32,
    pub result: i32,
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
    files: FixedFileTable,
    listener_file: FixedFile,
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
        let ring = build_ring(ring_entries)?;
        let mut files = FixedFileTable::new(1)?;
        files.register(&ring.submitter())?;
        let listener_file = files.install(&ring.submitter(), listener_fd)?;
        Ok(Self {
            ring,
            files,
            listener_file,
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
                self.files
                    .get(self.listener_file)
                    .expect("listener fixed file remains installed"),
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
    fixed: Option<FixedFile>,
}

#[derive(Debug, Clone, Copy)]
struct PendingSend {
    generation: u32,
    active: bool,
    iovecs: [libc::iovec; 16],
    message: libc::msghdr,
}

// SAFETY: the poller is single-owner. The raw pointers are only populated
// immediately before an SQE is submitted and are never dereferenced by Rust;
// the owner keeps the pointed-to buffers alive until the matching CQE.
unsafe impl Send for PendingSend {}

impl PendingSend {
    fn new() -> Self {
        Self {
            generation: 0,
            active: false,
            iovecs: [libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            }; 16],
            message: unsafe { std::mem::zeroed() },
        }
    }

    fn prepare(&mut self, buffers: &[&[u8]]) -> io::Result<usize> {
        if buffers.len() > self.iovecs.len() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "too many TCP send vectors",
            ));
        }
        let mut count = 0;
        let mut total = 0usize;
        for buffer in buffers {
            if buffer.is_empty() {
                continue;
            }
            self.iovecs[count] = libc::iovec {
                iov_base: buffer.as_ptr().cast_mut().cast(),
                iov_len: buffer.len(),
            };
            count += 1;
            total = total.saturating_add(buffer.len());
        }
        if count == 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot submit an empty TCP send",
            ));
        }
        self.message = libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: 0,
            msg_iov: self.iovecs.as_mut_ptr(),
            msg_iovlen: count,
            msg_control: std::ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: 0,
        };
        Ok(total)
    }
}

#[derive(Debug, Clone, Copy)]
struct FixedRelease {
    file: FixedFile,
    poll_cancel_pending: bool,
    tx_cancel_pending: bool,
}

/// Single-owner, fixed-registration TCP readiness poller.
///
/// Registration and completion state are mutated only by the thread that
/// owns this value. `poll` writes into caller-owned storage so the ready path
/// does not need a `Vec` growth or a population scan.
pub struct UringTcpPoller {
    ring: IoUring,
    registrations: Box<[Option<Registration>]>,
    fixed_files: Option<FixedFileTable>,
    fixed_releases: Box<[Option<FixedRelease>]>,
    release_slots: Vec<u32>,
    pending_sends: Box<[PendingSend]>,
    send_completions: Box<[Option<TcpSendCompletion>]>,
    send_completion_order: VecDeque<u32>,
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
            fixed_files: None,
            fixed_releases: vec![None; max_slots].into_boxed_slice(),
            release_slots: Vec::with_capacity(max_slots),
            pending_sends: std::iter::repeat_with(PendingSend::new)
                .take(max_slots)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            send_completions: vec![None; max_slots].into_boxed_slice(),
            send_completion_order: VecDeque::with_capacity(max_slots),
            timeout_armed: false,
            metrics: TcpPollerMetrics::default(),
        })
    }

    /// Construct a poller whose registrations use the ring's fixed file
    /// table. Fixed slots are released only after the matching poll-cancel
    /// completion, so a descriptor cannot be reused while the kernel still
    /// owns an outstanding poll operation.
    pub fn new_fixed(max_slots: usize, ring_entries: u32) -> io::Result<Self> {
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
        let ring = build_ring(ring_entries)?;
        let files = FixedFileTable::new(max_slots)?;
        files.register(&ring.submitter())?;
        Ok(Self {
            ring,
            registrations: vec![None; max_slots].into_boxed_slice(),
            fixed_files: Some(files),
            fixed_releases: vec![None; max_slots].into_boxed_slice(),
            release_slots: Vec::with_capacity(max_slots),
            pending_sends: std::iter::repeat_with(PendingSend::new)
                .take(max_slots)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            send_completions: vec![None; max_slots].into_boxed_slice(),
            send_completion_order: VecDeque::with_capacity(max_slots),
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
            fixed: None,
        });
        if !interest.is_empty() {
            self.push_poll(fd, None, slot, generation, interest)?;
        }
        Ok(())
    }

    pub fn register_fixed(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u32,
        interest: TcpInterest,
    ) -> io::Result<()> {
        let slot_index = self.registration_index(slot)?;
        if self.fixed_files.is_none() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "poller has no fixed file table",
            ));
        }
        if self.registrations[slot_index].is_some() || self.fixed_releases[slot_index].is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "TCP fixed registration slot is still in use",
            ));
        }
        let fixed = self
            .fixed_files
            .as_mut()
            .expect("fixed file table checked above")
            .install(&self.ring.submitter(), fd)?;
        self.registrations[slot_index] = Some(Registration {
            fd,
            generation,
            armed: !interest.is_empty(),
            fixed: Some(fixed),
        });
        if !interest.is_empty()
            && let Err(error) = self.push_poll(
                fd,
                self.fixed_files.as_ref().and_then(|files| files.get(fixed)),
                slot,
                generation,
                interest,
            )
        {
            self.registrations[slot_index] = None;
            let _ = self
                .fixed_files
                .as_mut()
                .expect("fixed file table remains installed")
                .remove(&self.ring.submitter(), fixed);
            return Err(error);
        }
        Ok(())
    }

    pub fn remove(&mut self, slot: u32) -> io::Result<()> {
        let slot_index = self.registration_index(slot)?;
        let Some(previous) = self.registrations[slot_index].take() else {
            return Ok(());
        };
        let poll_cancel_pending = previous.armed;
        if poll_cancel_pending {
            self.cancel_poll(slot, previous.generation)?;
        }
        let tx_cancel_pending = self.pending_sends[slot_index].active;
        if tx_cancel_pending {
            self.cancel_send(slot, previous.generation)?;
        }
        if let Some(fixed) = previous.fixed {
            self.fixed_releases[slot_index] = Some(FixedRelease {
                file: fixed,
                poll_cancel_pending,
                tx_cancel_pending,
            });
            self.release_fixed_if_ready(slot as usize)?;
        }
        Ok(())
    }

    /// Submit one bounded send against a registered socket. The caller must
    /// keep `bytes` alive and unchanged until the matching completion is
    /// drained. One send is allowed per leaf slot, which bounds kernel-owned
    /// TX state and makes generation reuse explicit.
    pub fn submit_send(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u32,
        bytes: &[u8],
    ) -> io::Result<()> {
        if bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot submit an empty TCP send",
            ));
        }
        let index = self.registration_index(slot)?;
        let registration = self.registrations[index].ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "TCP registration is not live")
        })?;
        if registration.fd != fd || registration.generation != generation {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TCP send generation or descriptor mismatch",
            ));
        }
        if self.pending_sends[index].active {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "TCP send already in flight",
            ));
        }
        self.submit_send_vectored(fd, slot, generation, std::slice::from_ref(&bytes))
    }

    /// Submit up to sixteen caller-owned buffers as one native `SENDMSG`.
    /// The buffer owners must remain unchanged until the matching completion.
    pub fn submit_send_vectored(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u32,
        buffers: &[&[u8]],
    ) -> io::Result<()> {
        let index = self.registration_index(slot)?;
        let registration = self.registrations[index].ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "TCP registration is not live")
        })?;
        if registration.fd != fd || registration.generation != generation {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "TCP send generation or descriptor mismatch",
            ));
        }
        if self.pending_sends[index].active {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "TCP send already in flight",
            ));
        }
        if self.send_completions[index].is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "TCP send completion must be drained first",
            ));
        }
        let message = {
            let operation = &mut self.pending_sends[index];
            let _total = operation.prepare(buffers)?;
            &operation.message as *const libc::msghdr
        };
        let entry = match registration
            .fixed
            .and_then(|fixed| self.fixed_files.as_ref().and_then(|files| files.get(fixed)))
        {
            Some(fixed) => opcode::SendMsg::new(fixed, message).build().user_data(
                OpTag::new(OpKind::TcpTx, slot, generation)
                    .expect("validated TCP send slot")
                    .encode(),
            ),
            None => opcode::SendMsg::new(types::Fd(fd), message)
                .build()
                .user_data(
                    OpTag::new(OpKind::TcpTx, slot, generation)
                        .expect("validated TCP send slot")
                        .encode(),
                ),
        };
        unsafe { self.push(&entry)? };
        self.pending_sends[index].generation = generation;
        self.pending_sends[index].active = true;
        Ok(())
    }

    /// Drain send completions into caller-owned storage without scanning all
    /// registrations.
    pub fn drain_send_completions(&mut self, completions: &mut [TcpSendCompletion]) -> usize {
        let mut count = 0;
        while count < completions.len() {
            let Some(slot) = self.send_completion_order.pop_front() else {
                break;
            };
            let Some(completion) = self.send_completions[slot as usize].take() else {
                continue;
            };
            completions[count] = completion;
            count += 1;
        }
        count
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
                    OpKind::TimeoutCancel => {}
                    OpKind::PollCancel => {
                        if let Some(release) = self
                            .fixed_releases
                            .get_mut(tag.slot as usize)
                            .and_then(Option::as_mut)
                        {
                            release.poll_cancel_pending = false;
                            self.release_slots.push(tag.slot);
                        }
                    }
                    OpKind::TcpTxCancel => {
                        if let Some(release) = self
                            .fixed_releases
                            .get_mut(tag.slot as usize)
                            .and_then(Option::as_mut)
                        {
                            release.tx_cancel_pending = false;
                            self.release_slots.push(tag.slot);
                        }
                    }
                    OpKind::TcpTx => {
                        let Some(pending) = self.pending_sends.get_mut(tag.slot as usize) else {
                            self.metrics.stale_completions += 1;
                            continue;
                        };
                        if !pending.active {
                            self.metrics.stale_completions += 1;
                            continue;
                        }
                        pending.active = false;
                        if pending.generation != tag.generation {
                            self.metrics.stale_completions += 1;
                            continue;
                        }
                        let Some(completion_slot) =
                            self.send_completions.get_mut(tag.slot as usize)
                        else {
                            self.metrics.stale_completions += 1;
                            continue;
                        };
                        if completion_slot.is_some() {
                            self.metrics.stale_completions += 1;
                            continue;
                        }
                        *completion_slot = Some(TcpSendCompletion {
                            slot: tag.slot,
                            generation: tag.generation,
                            result: completion.result(),
                        });
                        self.send_completion_order.push_back(tag.slot);
                        if self.fixed_releases[tag.slot as usize].is_some() {
                            self.release_slots.push(tag.slot);
                        }
                    }
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

        let mut release_slots = std::mem::take(&mut self.release_slots);
        while let Some(slot) = release_slots.pop() {
            self.release_fixed_if_ready(slot as usize)?;
        }
        self.release_slots = release_slots;

        if self.timeout_armed {
            let entry =
                opcode::TimeoutRemove::new(OpTag::new(OpKind::Timeout, 0, 0).unwrap().encode())
                    .build()
                    .user_data(OpTag::new(OpKind::TimeoutCancel, 0, 0).unwrap().encode());
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
        fixed: Option<types::Fixed>,
        slot: u32,
        generation: u32,
        interest: TcpInterest,
    ) -> io::Result<()> {
        let user_data = OpTag::new(OpKind::TcpRx, slot, generation)
            .unwrap()
            .encode();
        let entry = match fixed {
            Some(fixed) => opcode::PollAdd::new(fixed, interest.poll_flags())
                .build()
                .user_data(user_data),
            None => opcode::PollAdd::new(types::Fd(fd), interest.poll_flags())
                .build()
                .user_data(user_data),
        };
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

    fn cancel_send(&mut self, slot: u32, generation: u32) -> io::Result<()> {
        let target = OpTag::new(OpKind::TcpTx, slot, generation)
            .unwrap()
            .encode();
        let entry = opcode::AsyncCancel::new(target).build().user_data(
            OpTag::new(OpKind::TcpTxCancel, slot, generation)
                .unwrap()
                .encode(),
        );
        unsafe { self.push(&entry) }
    }

    fn release_fixed_if_ready(&mut self, slot: usize) -> io::Result<()> {
        let Some(release) = self.fixed_releases[slot] else {
            return Ok(());
        };
        if release.poll_cancel_pending
            || release.tx_cancel_pending
            || self.pending_sends[slot].active
        {
            return Ok(());
        }
        self.fixed_releases[slot] = None;
        let _ = self
            .fixed_files
            .as_mut()
            .expect("fixed file table remains installed")
            .remove(&self.ring.submitter(), release.file)?;
        Ok(())
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
    fn fixed_file_poller_reports_socket_readiness_and_releases_after_cancel() {
        let Some((receiver, sender)) = socket_pair() else {
            return;
        };
        let mut poller = match UringTcpPoller::new_fixed(2, 32) {
            Ok(poller) => poller,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                ) =>
            {
                return;
            }
            Err(error) => panic!("fixed-file io_uring unavailable: {error}"),
        };
        poller
            .register_fixed(receiver.as_raw_fd(), 0, 1, TcpInterest::READ)
            .unwrap();
        let byte = [9_u8];
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
        assert_eq!(poller.poll(Duration::ZERO, &mut ready).unwrap(), 1);
        assert_eq!(ready[0].generation, 1);

        poller.remove(0).unwrap();
        poller
            .register_fixed(receiver.as_raw_fd(), 0, 2, TcpInterest::READ)
            .unwrap();
        poller.remove(0).unwrap();
        assert!(
            poller
                .register_fixed(receiver.as_raw_fd(), 0, 3, TcpInterest::READ)
                .is_err()
        );
        let _ = poller.poll(Duration::ZERO, &mut ready).unwrap();
        poller
            .register_fixed(receiver.as_raw_fd(), 0, 3, TcpInterest::READ)
            .unwrap();
    }

    #[test]
    fn fixed_file_poller_completes_owner_thread_send() {
        let Some((receiver, sender)) = socket_pair() else {
            return;
        };
        let mut poller = match UringTcpPoller::new_fixed(2, 32) {
            Ok(poller) => poller,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                ) =>
            {
                return;
            }
            Err(error) => panic!("fixed-file io_uring unavailable: {error}"),
        };
        poller
            .register_fixed(receiver.as_raw_fd(), 0, 7, TcpInterest::READ)
            .unwrap();
        let buffers: [&[u8]; 2] = [b"nat", b"ive"];
        poller
            .submit_send_vectored(receiver.as_raw_fd(), 0, 7, &buffers)
            .unwrap();

        let mut ready = [TcpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        }; 2];
        let mut completions = [TcpSendCompletion {
            slot: 0,
            generation: 0,
            result: 0,
        }; 2];
        for _ in 0..10 {
            poller.poll(Duration::from_millis(1), &mut ready).unwrap();
            if poller.drain_send_completions(&mut completions) == 1 {
                assert_eq!(completions[0].slot, 0);
                assert_eq!(completions[0].generation, 7);
                assert_eq!(completions[0].result, 6);
                let mut received = [0_u8; 6];
                let count = unsafe {
                    libc::read(
                        sender.as_raw_fd(),
                        received.as_mut_ptr().cast::<libc::c_void>(),
                        received.len(),
                    )
                };
                assert_eq!(count, 6);
                assert_eq!(&received, b"native");
                return;
            }
        }
        panic!("owner-thread send did not complete");
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
