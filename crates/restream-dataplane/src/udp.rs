//! One-owner UDP readiness over `io_uring`.

use std::io;
use std::os::fd::RawFd;
use std::time::Duration;

use io_uring::{IoUring, opcode, types};

use crate::{MAX_TAG_SLOTS, OpKind, OpTag, build_ring};

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
}

/// Single-owner, fixed-registration UDP readiness poller.
///
/// UDP payloads stay in protocol-owned fixed buffers. This type only reports
/// readiness into caller-owned storage; it never allocates on `poll`.
pub struct UringUdpPoller {
    ring: IoUring,
    registrations: Box<[Option<Registration>]>,
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
        Ok(Self {
            ring: build_ring(ring_entries)?,
            registrations: vec![None; max_slots].into_boxed_slice(),
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
        if let Some(previous) = self.registrations[index]
            && previous.armed
        {
            self.cancel_poll(slot, previous.generation)?;
        }
        self.registrations[index] = Some(Registration {
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
        let index = self.registration_index(slot)?;
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
        slot: u32,
        generation: u32,
        interest: UdpInterest,
    ) -> io::Result<()> {
        let entry = opcode::PollAdd::new(types::Fd(fd), interest.poll_flags())
            .build()
            .user_data(
                OpTag::new(OpKind::UdpRx, slot, generation)
                    .unwrap()
                    .encode(),
            );
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

#[cfg(test)]
mod tests {
    use super::*;
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
