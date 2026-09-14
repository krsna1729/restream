//! One-owner UDP readiness over `io_uring`.

use std::collections::VecDeque;
use std::io;
use std::net::SocketAddr;
use std::os::fd::RawFd;
use std::time::Duration;

use io_uring::{IoUring, opcode, types};

use crate::{FixedFileTable, MAX_TAG_SLOTS, OpKind, OpTag, build_ring};

pub use crate::udp_recv::{UdpRecvBuffers, UdpRecvDatagram, UringUdpReceiver};

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
    pub cq_overflows: u64,
    pub poll_errors: u64,
}

#[derive(Debug, Clone, Copy)]
pub(crate) struct Registration {
    pub(crate) fd: RawFd,
    pub(crate) generation: u32,
    pub(crate) armed: bool,
    pub(crate) fixed: Option<types::Fixed>,
}

pub(crate) struct PendingSend {
    pub(crate) registration_slot: u32,
    pub(crate) generation: u32,
    pub(crate) active: bool,
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
            registration_slot: 0,
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
///
/// One ring per owner thread: readiness polls, UDP sends, and the timeout
/// share this poller's ring. Multishot receives live on a separate
/// [`UringUdpReceiver`] ring today; the planned [`UringUdpDriver`] merges
/// them so a shard that services IPv4 and IPv6 holds both descriptors as two
/// fixed slots on one ring instead of one poller per family.
pub struct UringUdpPoller {
    pub(crate) ring: IoUring,
    pub(crate) registrations: Box<[Option<Registration>]>,
    pub(crate) fixed_files: Option<FixedFileTable>,
    pub(crate) pending_sends: Box<[PendingSend]>,
    pub(crate) send_completions: Box<[Option<UdpSendCompletion>]>,
    pub(crate) send_completion_order: VecDeque<u32>,
    pub(crate) timeout_armed: bool,
    pub(crate) metrics: UdpPollerMetrics,
}

impl UringUdpPoller {
    pub fn new(max_slots: usize, ring_entries: u32) -> io::Result<Self> {
        Self::new_with_send_capacity(max_slots, ring_entries, max_slots)
    }

    /// Construct a readiness poller with an independent bounded UDP send
    /// slot pool. Send slots may exceed registration slots when one socket
    /// should keep several datagrams in flight.
    pub fn new_with_send_capacity(
        max_slots: usize,
        ring_entries: u32,
        send_capacity: usize,
    ) -> io::Result<Self> {
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
        if send_capacity == 0 || send_capacity >= MAX_TAG_SLOTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid UDP send capacity",
            ));
        }
        let ring = build_ring(ring_entries)?;
        Ok(Self {
            ring,
            registrations: vec![None; max_slots].into_boxed_slice(),
            fixed_files: None,
            pending_sends: std::iter::repeat_with(PendingSend::new)
                .take(send_capacity)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            send_completions: vec![None; send_capacity].into_boxed_slice(),
            send_completion_order: VecDeque::with_capacity(send_capacity),
            timeout_armed: false,
            metrics: UdpPollerMetrics::default(),
        })
    }

    /// Construct a poller whose registrations use the ring's fixed file
    /// table. Fixed slots live for the poller's lifetime; this matches native
    /// shard sockets, which are opened once and owned by one thread.
    pub fn new_fixed(max_slots: usize, ring_entries: u32) -> io::Result<Self> {
        Self::new_fixed_with_send_capacity(max_slots, ring_entries, max_slots)
    }

    /// Fixed-file variant with an independent bounded UDP send slot pool.
    pub fn new_fixed_with_send_capacity(
        max_slots: usize,
        ring_entries: u32,
        send_capacity: usize,
    ) -> io::Result<Self> {
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
        if send_capacity == 0 || send_capacity >= MAX_TAG_SLOTS {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid UDP send capacity",
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
                .take(send_capacity)
                .collect::<Vec<_>>()
                .into_boxed_slice(),
            send_completions: vec![None; send_capacity].into_boxed_slice(),
            send_completion_order: VecDeque::with_capacity(send_capacity),
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
        if self.send_in_flight_for_registration(slot)
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
        if self.send_in_flight_for_registration(slot) {
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
            self.metrics.cq_overflows = u64::from(cq.overflow());
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
    /// completion is drained. The operation slot is independent from the
    /// registration slot, so one socket can have several sends in flight.
    pub fn submit_send(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u32,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> io::Result<()> {
        self.submit_send_on_slot(fd, slot, slot, generation, peer, bytes)
    }

    pub fn submit_send_on_slot(
        &mut self,
        fd: RawFd,
        registration_slot: u32,
        operation_slot: u32,
        generation: u32,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> io::Result<()> {
        let registration_index = self.registration_index(registration_slot)?;
        let operation_index = self.send_index(operation_slot)?;
        let registration = self.registrations[registration_index].ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotFound, "UDP registration is not live")
        })?;
        if registration.fd != fd || registration.generation != generation {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP send generation or descriptor mismatch",
            ));
        }
        if self.pending_sends[operation_index].active
            || self.send_completions[operation_index].is_some()
        {
            return Err(io::Error::new(
                io::ErrorKind::WouldBlock,
                "UDP send is still in flight",
            ));
        }
        self.pending_sends[operation_index].prepare(peer, bytes)?;
        let message = &self.pending_sends[operation_index].message as *const libc::msghdr;
        let tag = OpTag::new(OpKind::UdpTx, operation_slot, generation)
            .expect("validated UDP send slot")
            .encode();
        let entry = match registration.fixed {
            Some(fixed) => opcode::SendMsg::new(fixed, message).build().user_data(tag),
            None => opcode::SendMsg::new(types::Fd(fd), message)
                .build()
                .user_data(tag),
        };
        if let Err(error) = unsafe { self.push(&entry) } {
            self.pending_sends[operation_index].active = false;
            return Err(error);
        }
        self.pending_sends[operation_index].registration_slot = registration_slot;
        self.pending_sends[operation_index].generation = generation;
        self.pending_sends[operation_index].active = true;
        Ok(())
    }

    /// Drain send completions without scanning all registered sockets.
    pub fn drain_send_completions(&mut self, completions: &mut [UdpSendCompletion]) -> usize {
        let mut count = 0;
        while count < completions.len() {
            let Some(slot) = self.send_completion_order.pop_front() else {
                break;
            };
            let Some(completion) = self
                .send_completions
                .get_mut(slot as usize)
                .and_then(Option::take)
            else {
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

    fn send_index(&self, slot: u32) -> io::Result<usize> {
        let index = slot as usize;
        if index >= self.pending_sends.len() {
            Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP send slot out of range",
            ))
        } else {
            Ok(index)
        }
    }

    fn send_in_flight_for_registration(&self, registration_slot: u32) -> bool {
        self.pending_sends.iter().enumerate().any(|(index, send)| {
            send.registration_slot == registration_slot
                && (send.active || self.send_completions[index].is_some())
        })
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

/// One ring per owner thread: readiness polls, UDP sends, multishot receives
/// with provided buffers, and the timeout all share this driver's ring. A
/// shard that services IPv4 and IPv6 registers both descriptors as two fixed
/// slots on the same ring instead of owning one poller per family. Receive
/// buffers use one provided-buffer group per registration slot so buffer IDs
/// stay namespaced by slot; completion tags carry the slot in `OpTag::slot`
/// and use the receiver generations (0 = provided, 1 = receive) in
/// `OpKind::UdpRecvMulti`.
pub struct UringUdpDriver {
    poller: UringUdpPoller,
    recv: Vec<Option<DriverRecv>>,
    recv_message: Box<libc::msghdr>,
    buffers_per_slot: u16,
    buffer_size: usize,
}

struct DriverRecv {
    fd: RawFd,
    fixed: Option<types::Fixed>,
    buffers: std::sync::Arc<UdpRecvBuffers>,
    recv_armed: bool,
    provided: usize,
    recycle: VecDeque<u16>,
}

// SAFETY: single-owner like `PendingSend`. `recv_message` raw pointers are
// only populated while the owner retains the message box; `DriverRecv`
// holds no pointers, only the fd and buffer ownership.
unsafe impl Send for UringUdpDriver {}

/// Datagrams drained from [`UringUdpDriver::poll`] alongside readiness. The
/// buffer ID is namespaced by registration slot and must be returned with
/// [`UringUdpDriver::recycle`]; payload bytes are read from
/// [`UringUdpDriver::buffers`] without copying.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpDriverDatagram {
    pub slot: u32,
    pub buffer_id: u16,
    pub offset: usize,
    pub len: usize,
    pub peer: SocketAddr,
}

impl UringUdpDriver {
    /// One ring, `max_slots` fixed registrations, `send_capacity` in-flight
    /// sends, and `buffers_per_slot` provided receive buffers per slot.
    pub fn new_fixed(
        max_slots: usize,
        ring_entries: u32,
        send_capacity: usize,
        buffers_per_slot: u16,
        buffer_size: usize,
    ) -> io::Result<Self> {
        if buffers_per_slot == 0 || buffer_size < 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid UDP receive buffer geometry",
            ));
        }
        let poller =
            UringUdpPoller::new_fixed_with_send_capacity(max_slots, ring_entries, send_capacity)?;
        let mut recv = Vec::with_capacity(max_slots);
        recv.resize_with(max_slots, || None);
        Ok(Self {
            poller,
            recv: recv.into_boxed_slice().into_vec(),
            recv_message: Box::new(libc::msghdr {
                msg_name: std::ptr::null_mut(),
                msg_namelen: std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t,
                msg_iov: std::ptr::null_mut(),
                msg_iovlen: 0,
                msg_control: std::ptr::null_mut(),
                msg_controllen: 0,
                msg_flags: 0,
            }),
            buffers_per_slot,
            buffer_size,
        })
    }

    /// Register one descriptor as fixed slot `slot` and arm its multishot
    /// receive on the shared ring. Both families of one shard are two slots
    /// on this same ring.
    pub fn register_fixed(
        &mut self,
        fd: RawFd,
        slot: u32,
        generation: u32,
        interest: UdpInterest,
    ) -> io::Result<()> {
        let index = usize::try_from(slot)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "UDP slot out of range"))?;
        if index >= self.recv.len() || self.recv[index].is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP driver slot out of range or already used",
            ));
        }
        self.poller.register_fixed(fd, slot, generation, interest)?;
        let registration = self.poller.registrations[index]
            .as_ref()
            .expect("just-registered UDP slot remains live");
        let buffers = std::sync::Arc::new(UdpRecvBuffers::new_buffers(
            self.buffers_per_slot,
            self.buffer_size,
        ));
        let mut state = DriverRecv {
            fd,
            fixed: registration.fixed,
            buffers: buffers.clone(),
            recv_armed: false,
            provided: 0,
            recycle: VecDeque::with_capacity(usize::from(self.buffers_per_slot)),
        };
        self.provide_all(slot, &mut state)?;
        self.poller.ring.submit_and_wait(1)?;
        self.drain_provided(slot, &mut state, true)?;
        self.arm_recv(slot, &mut state)?;
        self.poller.ring.submit()?;
        self.recv[index] = Some(state);
        Ok(())
    }

    /// Shared receive storage for `slot`, for zero-copy payload views.
    pub fn buffers(&self, slot: u32) -> Option<std::sync::Arc<UdpRecvBuffers>> {
        self.recv
            .get(slot as usize)?
            .as_ref()
            .map(|state| state.buffers.clone())
    }

    /// Provided buffers still owned by the kernel for `slot`.
    pub fn available_buffers(&self, slot: u32) -> usize {
        self.recv
            .get(slot as usize)
            .and_then(Option::as_ref)
            .map_or(0, |state| state.provided)
    }

    /// Return a consumed buffer ID to the kernel's provided group.
    pub fn recycle(&mut self, slot: u32, buffer_id: u16) -> io::Result<()> {
        let Some(state) = self.recv.get_mut(slot as usize).and_then(Option::as_mut) else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP driver slot is not live",
            ));
        };
        if buffer_id >= self.buffers_per_slot {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP receive buffer id out of range",
            ));
        }
        state.recycle.push_back(buffer_id);
        self.flush_recycle(slot)
    }

    /// Drain one CQ batch: readiness into `ready`, receives into
    /// `datagrams`, send completions retained for `drain_send_completions`.
    /// Single `submit_and_wait`; one ring, one wait, one timeout.
    pub fn poll(
        &mut self,
        timeout: Duration,
        ready: &mut [UdpReadyEvent],
        datagrams: &mut [UdpDriverDatagram],
    ) -> io::Result<(usize, usize)> {
        for slot in 0..self.recv.len() as u32 {
            self.flush_recycle(slot)?;
            let needs_arm = self.recv[slot as usize]
                .as_ref()
                .is_some_and(|state| !state.recv_armed && state.provided != 0);
            if needs_arm {
                // Borrow dance: take raw parts so `arm_recv` can use `&mut self`
                // while `state` is borrowed; single-owner thread, no aliasing.
                let state_ptr = self.recv[slot as usize].as_mut().unwrap() as *mut DriverRecv;
                unsafe { self.arm_recv(slot, &mut *state_ptr)? };
            }
        }
        let timespec = types::Timespec::from(timeout);
        if !timeout.is_zero() && !self.poller.timeout_armed {
            let entry = opcode::Timeout::new(&timespec)
                .build()
                .user_data(OpTag::new(OpKind::Timeout, 0, 0).unwrap().encode());
            unsafe { self.poller.push(&entry)? };
            self.poller.timeout_armed = true;
        }
        self.poller
            .ring
            .submit_and_wait(usize::from(!timeout.is_zero()))?;
        let mut ready_count = 0;
        let mut datagram_count = 0;
        let mut recv_more = vec![false; self.recv.len()];
        {
            let cq = self.poller.ring.completion();
            self.poller.metrics.cq_overflows = u64::from(cq.overflow());
            for completion in cq {
                self.poller.metrics.completions += 1;
                let Some(tag) = OpTag::decode(completion.user_data()) else {
                    self.poller.metrics.stale_completions += 1;
                    continue;
                };
                match tag.kind {
                    OpKind::Timeout => self.poller.timeout_armed = false,
                    OpKind::PollCancel => {}
                    OpKind::UdpTx => {
                        let Some(operation) = self.poller.pending_sends.get_mut(tag.slot as usize)
                        else {
                            self.poller.metrics.stale_completions += 1;
                            continue;
                        };
                        operation.active = false;
                        if operation.generation != tag.generation {
                            self.poller.metrics.stale_completions += 1;
                            continue;
                        }
                        self.poller.send_completions[tag.slot as usize] = Some(UdpSendCompletion {
                            slot: tag.slot,
                            generation: tag.generation,
                            result: completion.result(),
                        });
                        self.poller.send_completion_order.push_back(tag.slot);
                    }
                    OpKind::UdpRecvMulti => {
                        let Some(state) = self
                            .recv
                            .get_mut(tag.slot as usize)
                            .and_then(Option::as_mut)
                        else {
                            self.poller.metrics.stale_completions += 1;
                            continue;
                        };
                        if tag.generation == crate::udp_recv::PROVIDED_GENERATION {
                            if completion.result() < 0 {
                                return Err(io::Error::from_raw_os_error(-completion.result()));
                            }
                            state.provided = state.provided.saturating_add(1);
                        } else if tag.generation == crate::udp_recv::RECV_GENERATION {
                            recv_more[tag.slot as usize] |=
                                io_uring::cqueue::more(completion.flags());
                            if completion.result() < 0 {
                                let error = io::Error::from_raw_os_error(-completion.result());
                                if error.raw_os_error() != Some(libc::ENOBUFS) {
                                    return Err(error);
                                }
                                continue;
                            }
                            state.provided = state.provided.saturating_sub(1);
                            let Some(buffer_id) =
                                io_uring::cqueue::buffer_select(completion.flags())
                            else {
                                return Err(io::Error::other(
                                    "UDP multishot completion did not select a buffer",
                                ));
                            };
                            if datagram_count == datagrams.len() {
                                state.recycle.push_back(buffer_id);
                                continue;
                            }
                            let result_len =
                                usize::try_from(completion.result()).map_err(|_| {
                                    io::Error::other("UDP receive completion length overflow")
                                })?;
                            let parsed = types::RecvMsgOut::parse(
                                &state.buffers.bytes[usize::from(buffer_id)
                                    * state.buffers.buffer_size
                                    ..usize::from(buffer_id) * state.buffers.buffer_size
                                        + result_len],
                                &self.recv_message,
                            )
                            .map_err(|_| {
                                io::Error::new(
                                    io::ErrorKind::InvalidData,
                                    "invalid UDP recvmsg output",
                                )
                            })?;
                            let Some(peer) = crate::udp_recv::parse_socket_addr(parsed.name_data())
                            else {
                                state.recycle.push_back(buffer_id);
                                continue;
                            };
                            let base =
                                state.buffers.bytes.as_ptr().wrapping_add(
                                    usize::from(buffer_id) * state.buffers.buffer_size,
                                ) as usize;
                            let payload = parsed.payload_data();
                            let offset = (payload.as_ptr() as usize).saturating_sub(base);
                            datagrams[datagram_count] = UdpDriverDatagram {
                                slot: tag.slot,
                                buffer_id,
                                offset,
                                len: payload.len(),
                                peer,
                            };
                            datagram_count += 1;
                        }
                    }
                    _ => {
                        let Some(registration) = self
                            .poller
                            .registrations
                            .get_mut(tag.slot as usize)
                            .and_then(Option::as_mut)
                        else {
                            self.poller.metrics.stale_completions += 1;
                            continue;
                        };
                        if registration.generation != tag.generation {
                            self.poller.metrics.stale_completions += 1;
                            continue;
                        }
                        registration.armed = false;
                        if completion.result() < 0 {
                            self.poller.metrics.poll_errors += 1;
                            return Err(io::Error::from_raw_os_error(-completion.result()));
                        }
                        let events = completion.result() as u32;
                        let errored = events & (libc::POLLERR | libc::POLLHUP) as u32 != 0;
                        if ready_count == ready.len() {
                            self.poller.metrics.ready_overflows += 1;
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
        if self.poller.timeout_armed {
            let entry =
                opcode::TimeoutRemove::new(OpTag::new(OpKind::Timeout, 0, 0).unwrap().encode())
                    .build()
                    .user_data(OpTag::new(OpKind::PollCancel, 0, 0).unwrap().encode());
            unsafe { self.poller.push(&entry)? };
            self.poller.ring.submit()?;
            self.poller.timeout_armed = false;
        }
        for slot in 0..self.recv.len() as u32 {
            self.flush_recycle(slot)?;
            let Some(state) = self.recv[slot as usize].as_mut() else {
                continue;
            };
            if !recv_more[slot as usize] {
                state.recv_armed = false;
            }
            let needs_arm = !state.recv_armed && state.provided != 0;
            if needs_arm {
                let state_ptr = state as *mut DriverRecv;
                unsafe { self.arm_recv(slot, &mut *state_ptr)? };
                self.poller.ring.submit()?;
            }
        }
        Ok((ready_count, datagram_count))
    }

    /// Submit one caller-owned datagram; caller retains `bytes` until the
    /// matching completion is drained.
    pub fn submit_send(
        &mut self,
        fd: RawFd,
        registration_slot: u32,
        operation_slot: u32,
        generation: u32,
        peer: SocketAddr,
        bytes: &[u8],
    ) -> io::Result<()> {
        self.poller.submit_send_on_slot(
            fd,
            registration_slot,
            operation_slot,
            generation,
            peer,
            bytes,
        )
    }

    /// Drain send completions without scanning registrations.
    pub fn drain_send_completions(&mut self, completions: &mut [UdpSendCompletion]) -> usize {
        self.poller.drain_send_completions(completions)
    }

    /// Re-arm readiness after a `poll` reported it (level-triggered PollAdd
    /// is one-shot; the owner re-arms per wake).
    pub fn rearm(&mut self, fd: RawFd, slot: u32, generation: u32) -> io::Result<()> {
        self.poller
            .register(fd, slot, generation, UdpInterest::READ_WRITE)
    }

    pub fn metrics(&self) -> UdpPollerMetrics {
        self.poller.metrics()
    }

    fn provide_all(&mut self, slot: u32, state: &mut DriverRecv) -> io::Result<()> {
        let group = self.group_for(slot);
        let entry = opcode::ProvideBuffers::new(
            state.buffers.bytes.as_ptr() as *mut u8,
            state.buffers.buffer_size as i32,
            self.buffers_per_slot,
            group,
            0,
        )
        .build()
        .user_data(
            OpTag::new(
                OpKind::UdpRecvMulti,
                slot,
                crate::udp_recv::PROVIDED_GENERATION,
            )
            .unwrap()
            .encode(),
        );
        unsafe { self.poller.push(&entry) }
    }

    fn flush_recycle(&mut self, slot: u32) -> io::Result<()> {
        let group = self.group_for(slot);
        let Some(state) = self.recv.get_mut(slot as usize).and_then(Option::as_mut) else {
            return Ok(());
        };
        while let Some(buffer_id) = state.recycle.front().copied() {
            let address = unsafe {
                state
                    .buffers
                    .bytes
                    .as_ptr()
                    .add(usize::from(buffer_id) * state.buffers.buffer_size)
                    as *mut u8
            };
            let entry = opcode::ProvideBuffers::new(
                address,
                state.buffers.buffer_size as i32,
                1,
                group,
                buffer_id,
            )
            .build()
            .user_data(
                OpTag::new(
                    OpKind::UdpRecvMulti,
                    slot,
                    crate::udp_recv::PROVIDED_GENERATION,
                )
                .unwrap()
                .encode(),
            );
            if unsafe { self.poller.push(&entry) }.is_err() {
                break;
            }
            state.recycle.pop_front();
        }
        if !state.recycle.is_empty() {
            self.poller.ring.submit()?;
        }
        Ok(())
    }

    fn drain_provided(
        &mut self,
        slot: u32,
        state: &mut DriverRecv,
        require_one: bool,
    ) -> io::Result<()> {
        let mut count = 0;
        let cq = self.poller.ring.completion();
        for completion in cq {
            let Some(tag) = OpTag::decode(completion.user_data()) else {
                continue;
            };
            if tag.kind == OpKind::UdpRecvMulti
                && tag.slot == slot
                && tag.generation == crate::udp_recv::PROVIDED_GENERATION
            {
                if completion.result() < 0 {
                    return Err(io::Error::from_raw_os_error(-completion.result()));
                }
                count += 1;
            }
        }
        if require_one && count == 0 {
            return Err(io::Error::other(
                "io_uring did not provide UDP receive buffers",
            ));
        }
        state.provided = if count == 0 {
            0
        } else {
            usize::from(self.buffers_per_slot)
        };
        Ok(())
    }

    fn arm_recv(&mut self, slot: u32, state: &mut DriverRecv) -> io::Result<()> {
        if state.recv_armed || state.provided == 0 {
            return Ok(());
        }
        let group = self.group_for(slot);
        let fd = match state.fixed {
            Some(fixed) => return self.arm_recv_fixed(slot, state, fixed, group),
            None => types::Fd(state.fd),
        };
        let _ = fd;
        let entry =
            opcode::RecvMsgMulti::new(types::Fd(state.fd), self.recv_message.as_ref(), group)
                .build()
                .user_data(
                    OpTag::new(OpKind::UdpRecvMulti, slot, crate::udp_recv::RECV_GENERATION)
                        .unwrap()
                        .encode(),
                );
        unsafe { self.poller.push(&entry)? };
        state.recv_armed = true;
        Ok(())
    }

    fn arm_recv_fixed(
        &mut self,
        slot: u32,
        state: &mut DriverRecv,
        fixed: types::Fixed,
        group: u16,
    ) -> io::Result<()> {
        let entry = opcode::RecvMsgMulti::new(fixed, self.recv_message.as_ref(), group)
            .build()
            .user_data(
                OpTag::new(OpKind::UdpRecvMulti, slot, crate::udp_recv::RECV_GENERATION)
                    .unwrap()
                    .encode(),
            );
        unsafe { self.poller.push(&entry)? };
        state.recv_armed = true;
        Ok(())
    }

    fn group_for(&self, slot: u32) -> u16 {
        // One provided-buffer group per registration slot; group 0 is
        // reserved so a zero group never aliases a live slot.
        (u32::from(u16::MAX - 1).min(slot) + 1) as u16
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
    fn fixed_file_poller_completes_batched_owner_thread_sends() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut poller = match UringUdpPoller::new_fixed_with_send_capacity(1, 32, 2) {
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
        let peer = sender.local_addr().unwrap();
        poller
            .submit_send_on_slot(receiver.as_raw_fd(), 0, 0, 7, peer, b"one")
            .unwrap();
        poller
            .submit_send_on_slot(receiver.as_raw_fd(), 0, 1, 7, peer, b"two")
            .unwrap();

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
        }; 2];
        for _ in 0..10 {
            poller.poll(Duration::from_millis(1), &mut ready).unwrap();
            if poller.drain_send_completions(&mut completions) == 2 {
                assert_eq!(
                    completions
                        .iter()
                        .map(|completion| (
                            completion.slot,
                            completion.generation,
                            completion.result
                        ))
                        .collect::<Vec<_>>(),
                    vec![(0, 7, 3), (1, 7, 3)]
                );
                let mut received = [[0_u8; 3]; 2];
                for packet in &mut received {
                    assert_eq!(sender.recv(&mut packet[..]).unwrap(), 3);
                }
                received.sort_unstable();
                assert_eq!(received, [*b"one", *b"two"]);
                return;
            }
        }
        panic!("owner-thread UDP sends did not complete");
    }

    #[test]
    fn driver_hosts_two_slots_and_multishot_receive_on_one_ring() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let second = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut driver = match UringUdpDriver::new_fixed(2, 64, 4, 8, 2_048) {
            Ok(driver) => driver,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                ) =>
            {
                return;
            }
            Err(error) => panic!("driver io_uring unavailable: {error}"),
        };
        driver
            .register_fixed(receiver.as_raw_fd(), 0, 1, UdpInterest::READ_WRITE)
            .unwrap();
        driver
            .register_fixed(second.as_raw_fd(), 1, 1, UdpInterest::READ_WRITE)
            .unwrap();
        // Both families share one ring: one poll observes both slots.
        sender
            .send_to(b"one", receiver.local_addr().unwrap())
            .unwrap();
        sender
            .send_to(b"two", second.local_addr().unwrap())
            .unwrap();
        let mut ready = [UdpReadyEvent {
            fd: -1,
            slot: 0,
            generation: 0,
            readable: false,
            writable: false,
        }; 4];
        let mut datagrams = [UdpDriverDatagram {
            slot: 0,
            buffer_id: 0,
            offset: 0,
            len: 0,
            peer: "0.0.0.0:0".parse().unwrap(),
        }; 4];
        let mut slots = std::collections::BTreeSet::new();
        for _ in 0..32 {
            let (_, received) = driver
                .poll(Duration::from_millis(5), &mut ready, &mut datagrams)
                .unwrap();
            for datagram in datagrams.iter().take(received) {
                let buffers = driver.buffers(datagram.slot).unwrap();
                let payload = buffers
                    .payload(datagram.buffer_id, datagram.offset, datagram.len)
                    .unwrap();
                assert!(payload == b"one" || payload == b"two");
                slots.insert(datagram.slot);
                driver.recycle(datagram.slot, datagram.buffer_id).unwrap();
            }
            if slots.len() == 2 {
                break;
            }
        }
        assert_eq!(slots, [0, 1].into_iter().collect());
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
