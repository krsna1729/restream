//! One-owner UDP readiness over `io_uring`.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::time::Duration;

use io_uring::{IoUring, opcode, types};

use crate::{FixedFileTable, MAX_TAG_SLOTS, OpKind, OpTag, build_ring};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UdpInterest {
    pub readable: bool,
    pub writable: bool,
}

impl UdpInterest {
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
pub struct UdpReadyEvent {
    pub fd: RawFd,
    pub slot: u32,
    pub generation: u32,
    pub readable: bool,
    pub writable: bool,
}

/// Completion of one owner-thread UDP send. The caller retains the submitted
/// datagram until this result is drained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpSendCompletion {
    pub slot: u32,
    pub generation: u32,
    pub result: i32,
}

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct UdpPollerMetrics {
    pub completions: u64,
    pub stale_completions: u64,
    pub ready_overflows: u64,
    pub poll_errors: u64,
}

#[derive(Debug, Clone, Copy)]
struct Registration {
    fd: RawFd,
    generation: u32,
    armed: bool,
    fixed: Option<types::Fixed>,
}

struct PendingSend {
    generation: u32,
    active: bool,
    destination: libc::sockaddr_storage,
    destination_len: libc::socklen_t,
    iovec: libc::iovec,
    message: libc::msghdr,
}

// SAFETY: the poller is single-owner. The raw pointers are only populated
// immediately before an SQE is submitted and the owner retains the datagram
// until the matching CQE.
unsafe impl Send for PendingSend {}

impl PendingSend {
    fn new() -> Self {
        Self {
            generation: 0,
            active: false,
            destination: unsafe { std::mem::zeroed() },
            destination_len: 0,
            iovec: libc::iovec {
                iov_base: std::ptr::null_mut(),
                iov_len: 0,
            },
            message: unsafe { std::mem::zeroed() },
        }
    }

    fn prepare(&mut self, peer: SocketAddr, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "cannot submit an empty UDP send",
            ));
        }
        let (destination, destination_len) = sockaddr(peer);
        self.destination = destination;
        self.destination_len = destination_len;
        self.iovec = libc::iovec {
            iov_base: bytes.as_ptr().cast_mut().cast(),
            iov_len: bytes.len(),
        };
        self.message = libc::msghdr {
            msg_name: (&mut self.destination as *mut libc::sockaddr_storage).cast(),
            msg_namelen: self.destination_len,
            msg_iov: &mut self.iovec,
            msg_iovlen: 1,
            msg_control: std::ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: 0,
        };
        Ok(())
    }
}

/// Single-owner, fixed-registration UDP readiness poller.
///
/// UDP payloads stay in protocol-owned fixed buffers. This type only reports
/// readiness into caller-owned storage; it never allocates on `poll`.
pub struct UringUdpPoller {
    ring: IoUring,
    registrations: Box<[Option<Registration>]>,
    fixed_files: Option<FixedFileTable>,
    pending_sends: Box<[PendingSend]>,
    send_completions: Box<[Option<UdpSendCompletion>]>,
    send_completion_order: VecDeque<u32>,
    timeout_armed: bool,
    metrics: UdpPollerMetrics,
}

impl UringUdpPoller {
    pub fn new(max_slots: usize, ring_entries: u32) -> io::Result<Self> {
        if max_slots == 0 || max_slots >= MAX_TAG_SLOTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid UDP registration capacity",
            ));
        }
        if !ring_entries.is_power_of_two() || ring_entries < 8 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "io_uring entries must be a power of two >= 8",
            ));
        }
        let ring = build_ring(ring_entries)?;
        Ok(Self {
            ring,
            registrations: vec![None; max_slots].into_boxed_slice(),
            fixed_files: None,
            pending_sends: std::iter::repeat_with(PendingSend::new)
                .take(max_slots)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            send_completions: vec![None; max_slots].into_boxed_slice(),
            send_completion_order: VecDeque::with_capacity(max_slots),
            timeout_armed: false,
            metrics: UdpPollerMetrics::default(),
        })
    }

    /// Construct a poller whose registrations use the ring's fixed file
    /// table. Fixed slots live for the poller's lifetime; this matches native
    /// shard sockets, which are opened once and owned by one thread.
    pub fn new_fixed(max_slots: usize, ring_entries: u32) -> io::Result<Self> {
        if max_slots == 0 || max_slots >= MAX_TAG_SLOTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid UDP registration capacity",
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
            pending_sends: std::iter::repeat_with(PendingSend::new)
                .take(max_slots)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            send_completions: vec![None; max_slots].into_boxed_slice(),
            send_completion_order: VecDeque::with_capacity(max_slots),
            timeout_armed: false,
            metrics: UdpPollerMetrics::default(),
        })
    }

    pub fn register(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u32,
        interest: UdpInterest,
    ) -> io::Result<()> {
        let index = self.registration_index(slot)?;
        let send_in_flight =
            self.pending_sends[index].active || self.send_completions[index].is_some();
        if send_in_flight
            && !self.registrations[index]
                .is_some_and(|previous| previous.fd == fd && previous.generation == generation)
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "UDP send must be drained before registration reuse",
            ));
        }
        if let Some(previous) = self.registrations[index]
            && previous.armed
        {
            self.cancel_poll(slot, previous.generation)?;
        }
        self.registrations[index] = Some(Registration {
            fd,
            generation,
            armed: !interest.is_empty(),
            fixed: self.registrations[index].and_then(|registration| registration.fixed),
        });
        if !interest.is_empty() {
            self.push_poll(
                fd,
                self.registrations[index].and_then(|registration| registration.fixed),
                slot,
                generation,
                interest,
            )?;
        }
        Ok(())
    }

    pub fn register_fixed(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u32,
        interest: UdpInterest,
    ) -> io::Result<()> {
        let index = self.registration_index(slot)?;
        let files = self.fixed_files.as_mut().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "poller has no fixed file table",
            )
        })?;
        if self.registrations[index].is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "UDP fixed registration slot already used",
            ));
        }
        let file = files.install(&self.ring.submitter(), fd)?;
        let fixed = files
            .get(file)
            .expect("installed fixed UDP file remains valid");
        self.registrations[index] = Some(Registration {
            fd,
            generation,
            armed: !interest.is_empty(),
            fixed: Some(fixed),
        });
        if !interest.is_empty() {
            self.push_poll(fd, Some(fixed), slot, generation, interest)?;
        }
        Ok(())
    }

    pub fn remove(&mut self, slot: u32) -> io::Result<()> {
        let index = self.registration_index(slot)?;
        if self.pending_sends[index].active || self.send_completions[index].is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "UDP send must be drained before registration removal",
            ));
        }
        let Some(previous) = self.registrations[index].take() else {
            return Ok(());
        };
        if previous.armed {
            self.cancel_poll(slot, previous.generation)?;
        }
        Ok(())
    }

    pub fn poll(&mut self, timeout: Duration, ready: &mut [UdpReadyEvent]) -> io::Result<usize> {
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
                    OpKind::Timeout => self.timeout_armed = false,
                    OpKind::PollCancel => {}
                    OpKind::UdpTx => {
                        let Some(operation) = self.pending_sends.get_mut(tag.slot as usize) else {
                            self.metrics.stale_completions += 1;
                            continue;
                        };
                        operation.active = false;
                        if operation.generation != tag.generation {
                            self.metrics.stale_completions += 1;
                            continue;
                        }
                        self.send_completions[tag.slot as usize] = Some(UdpSendCompletion {
                            slot: tag.slot,
                            generation: tag.generation,
                            result: completion.result(),
                        });
                        self.send_completion_order.push_back(tag.slot);
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
                        ready[ready_count] = UdpReadyEvent {
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

    pub fn metrics(&self) -> UdpPollerMetrics {
        self.metrics
    }

    /// Submit one caller-owned datagram against a registered socket. The
    /// caller must keep `bytes` alive and unchanged until the matching
    /// completion is drained. One send is allowed per registration slot.
    pub fn submit_send(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u32,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> io::Result<()> {
        let index = self.registration_index(slot)?;
        let registration = self.registrations[index].ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "UDP registration is not live")
        })?;
        if registration.fd != fd || registration.generation != generation {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP send generation or descriptor mismatch",
            ));
        }
        if self.pending_sends[index].active || self.send_completions[index].is_some() {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "UDP send is still in flight",
            ));
        }
        self.pending_sends[index].prepare(peer, bytes)?;
        let message = &self.pending_sends[index].message as *const libc::msghdr;
        let tag = OpTag::new(OpKind::UdpTx, slot, generation)
            .expect("validated UDP send slot")
            .encode();
        let entry = match registration.fixed {
            Some(fixed) => opcode::SendMsg::new(fixed, message).build().user_data(tag),
            None => opcode::SendMsg::new(types::Fd(fd), message)
                .build()
                .user_data(tag),
        };
        if let Err(error) = unsafe { self.push(&entry) } {
            self.pending_sends[index].active = false;
            return Err(error);
        }
        self.pending_sends[index].generation = generation;
        self.pending_sends[index].active = true;
        Ok(())
    }

    /// Drain send completions without scanning all registered sockets.
    pub fn drain_send_completions(&mut self, completions: &mut [UdpSendCompletion]) -> usize {
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

    fn registration_index(&self, slot: u32) -> io::Result<usize> {
        let index = slot as usize;
        if index >= self.registrations.len() {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP registration slot out of range",
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
        interest: UdpInterest,
    ) -> io::Result<()> {
        let user_data = OpTag::new(OpKind::UdpRx, slot, generation)
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
        let target = OpTag::new(OpKind::UdpRx, slot, generation)
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

fn sockaddr(peer: SocketAddr) -> (libc::sockaddr_storage, libc::socklen_t) {
    match peer {
        SocketAddr::V4(peer) => {
            let address = libc::sockaddr_in {
                sin_family: libc::AF_INET as libc::sa_family_t,
                sin_port: peer.port().to_be(),
                sin_addr: libc::in_addr {
                    s_addr: u32::from_ne_bytes(peer.ip().octets()),
                },
                sin_zero: [0; 8],
            };
            let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
            unsafe {
                std::ptr::write(
                    (&mut storage as *mut libc::sockaddr_storage).cast(),
                    address,
                );
            }
            (
                storage,
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        }
        SocketAddr::V6(peer) => {
            let address = libc::sockaddr_in6 {
                sin6_family: libc::AF_INET6 as libc::sa_family_t,
                sin6_port: peer.port().to_be(),
                sin6_flowinfo: peer.flowinfo(),
                sin6_addr: libc::in6_addr {
                    s6_addr: peer.ip().octets(),
                },
                sin6_scope_id: peer.scope_id(),
            };
            let mut storage: libc::sockaddr_storage = unsafe { std::mem::zeroed() };
            unsafe {
                std::ptr::write(
                    (&mut storage as *mut libc::sockaddr_storage).cast(),
                    address,
                );
            }
            (
                storage,
                std::mem::size_of::<libc::sockaddr_in6>() as libc::socklen_t,
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

    fn udp_pair() -> Option<(OwnedFd, OwnedFd)> {
        let receiver =
            unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        let sender =
            unsafe { libc::socket(libc::AF_INET, libc::SOCK_DGRAM | libc::SOCK_CLOEXEC, 0) };
        if receiver < 0 || sender < 0 {
            if receiver >= 0 {
                unsafe { libc::close(receiver) };
            }
            if sender >= 0 {
                unsafe { libc::close(sender) };
            }
            return None;
        }
        let mut address: libc::sockaddr_in = unsafe { std::mem::zeroed() };
        address.sin_family = libc::AF_INET as libc::sa_family_t;
        address.sin_addr = libc::in_addr {
            s_addr: u32::from_ne_bytes([127, 0, 0, 1]),
        };
        address.sin_port = 0;
        let result = unsafe {
            libc::bind(
                receiver,
                (&address as *const libc::sockaddr_in).cast(),
                std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t,
            )
        };
        if result != 0 {
            unsafe {
                libc::close(receiver);
                libc::close(sender);
            }
            return None;
        }
        let mut length = std::mem::size_of::<libc::sockaddr_in>() as libc::socklen_t;
        let result = unsafe {
            libc::getsockname(
                receiver,
                (&mut address as *mut libc::sockaddr_in).cast(),
                &mut length,
            )
        };
        if result != 0
            || unsafe {
                libc::connect(
                    sender,
                    (&address as *const libc::sockaddr_in).cast(),
                    length,
                )
            } != 0
        {
            unsafe {
                libc::close(receiver);
                libc::close(sender);
            }
            return None;
        }
        Some((unsafe { OwnedFd::from_raw_fd(receiver) }, unsafe {
            OwnedFd::from_raw_fd(sender)
        }))
    }

    fn poller() -> Option<UringUdpPoller> {
        match UringUdpPoller::new(4, 32) {
            Ok(poller) => Some(poller),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                ) =>
            {
                None
            }
            Err(error) => panic!("io_uring unavailable: {error}"),
        }
    }

    fn fixed_poller() -> Option<UringUdpPoller> {
        match UringUdpPoller::new_fixed(4, 32) {
            Ok(poller) => Some(poller),
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                ) =>
            {
                None
            }
            Err(error) => panic!("fixed-file io_uring unavailable: {error}"),
        }
    }

    #[test]
    fn poll_reports_udp_readiness_with_generation() {
        let Some((receiver, sender)) = udp_pair() else {
            return;
        };
        let Some(mut poller) = poller() else { return };
        poller
            .register(receiver.as_raw_fd(), 1, 7, UdpInterest::READ)
            .unwrap();
        let byte = [7_u8];
        assert_eq!(
            unsafe { libc::send(sender.as_raw_fd(), byte.as_ptr().cast(), 1, 0) },
            1
        );
        let mut ready = [UdpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        }; 2];
        assert_eq!(poller.poll(Duration::ZERO, &mut ready).unwrap(), 1);
        assert_eq!(ready[0].slot, 1);
        assert_eq!(ready[0].generation, 7);
        assert!(ready[0].readable);
    }

    #[test]
    fn fixed_file_poller_reports_udp_readiness() {
        let Some((receiver, sender)) = udp_pair() else {
            return;
        };
        let Some(mut poller) = fixed_poller() else {
            return;
        };
        poller
            .register_fixed(receiver.as_raw_fd(), 1, 7, UdpInterest::READ)
            .unwrap();
        let byte = [7_u8];
        assert_eq!(
            unsafe { libc::send(sender.as_raw_fd(), byte.as_ptr().cast(), 1, 0) },
            1
        );
        let mut ready = [UdpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        }; 2];
        assert_eq!(poller.poll(Duration::ZERO, &mut ready).unwrap(), 1);
        assert_eq!(ready[0].slot, 1);
        assert_eq!(ready[0].generation, 7);
        assert!(ready[0].readable);
    }

    #[test]
    fn fixed_file_poller_completes_owner_thread_send() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut poller = match UringUdpPoller::new_fixed(1, 32) {
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
            .register_fixed(receiver.as_raw_fd(), 0, 7, UdpInterest::READ)
            .unwrap();
        poller
            .submit_send(
                receiver.as_raw_fd(),
                0,
                7,
                sender.local_addr().unwrap(),
                b"native",
            )
            .unwrap();
        assert_eq!(
            poller.remove(0).unwrap_err().kind(),
            io::ErrorKind::WouldBlock
        );

        let mut ready = [UdpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        }; 1];
        let mut completions = [UdpSendCompletion {
            slot: 0,
            generation: 0,
            result: 0,
        }; 1];
        for _ in 0..10 {
            poller.poll(Duration::from_millis(1), &mut ready).unwrap();
            if poller.drain_send_completions(&mut completions) == 1 {
                assert_eq!(completions[0].slot, 0);
                assert_eq!(completions[0].generation, 7);
                assert_eq!(completions[0].result, 6);
                let mut received = [0_u8; 6];
                let (count, peer) = sender.recv_from(&mut received).unwrap();
                assert_eq!(count, 6);
                assert_eq!(peer, receiver.local_addr().unwrap());
                assert_eq!(&received, b"native");
                return;
            }
        }
        panic!("owner-thread UDP send did not complete");
    }

    #[test]
    fn stale_udp_completion_is_ignored_after_slot_reuse() {
        let Some((receiver, sender)) = udp_pair() else {
            return;
        };
        let Some(mut poller) = poller() else { return };
        poller
            .register(receiver.as_raw_fd(), 0, 1, UdpInterest::READ)
            .unwrap();
        poller.remove(0).unwrap();
        poller
            .register(receiver.as_raw_fd(), 0, 2, UdpInterest::READ)
            .unwrap();
        let byte = [8_u8];
        assert_eq!(
            unsafe { libc::send(sender.as_raw_fd(), byte.as_ptr().cast(), 1, 0) },
            1
        );
        let mut ready = [UdpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        }; 2];
        assert_eq!(poller.poll(Duration::ZERO, &mut ready).unwrap(), 1);
        assert_eq!(ready[0].generation, 2);
    }
}
