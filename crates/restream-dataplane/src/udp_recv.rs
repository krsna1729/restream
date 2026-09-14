use std::collections::VecDeque;
use std::io;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr};
use std::os::fd::RawFd;
use std::sync::Arc;
use std::time::Duration;

use io_uring::{IoUring, opcode, types};

use crate::{OpKind, OpTag, build_ring};

pub(crate) const RECV_GENERATION: u32 = 1;
pub(crate) const PROVIDED_GENERATION: u32 = 0;
pub(crate) const BUFFER_GROUP: u16 = 1;

/// Fixed storage shared by the native UDP owner and short-lived datagram
/// leases. The storage never moves while io_uring may reference it.
pub struct UdpRecvBuffers {
    pub(crate) bytes: Box<[u8]>,
    pub(crate) buffer_size: usize,
    pub(crate) count: u16,
}

impl UdpRecvBuffers {
    #[doc(hidden)]
    pub fn for_test(count: u16, buffer_size: usize, fill: u8) -> Self {
        Self {
            bytes: vec![fill; usize::from(count) * buffer_size].into_boxed_slice(),
            buffer_size,
            count,
        }
    }

    pub fn new_buffers(count: u16, buffer_size: usize) -> Self {
        Self {
            bytes: vec![0; usize::from(count) * buffer_size].into_boxed_slice(),
            buffer_size,
            count,
        }
    }

    /// View one kernel-filled payload without copying it.
    pub fn payload(&self, buffer_id: u16, offset: usize, len: usize) -> Option<&[u8]> {
        if buffer_id >= self.count {
            return None;
        }
        let start = usize::from(buffer_id)
            .checked_mul(self.buffer_size)?
            .checked_add(offset)?;
        self.bytes.get(start..start.checked_add(len)?)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdpRecvDatagram {
    pub buffer_id: u16,
    pub offset: usize,
    pub len: usize,
    pub peer: SocketAddr,
}

/// Multishot UDP receive owner using a bounded provided-buffer group.
///
/// The caller owns each returned buffer until it calls [`Self::recycle`].
/// No receive completion allocates or copies packet payload bytes.
pub struct UringUdpReceiver {
    ring: IoUring,
    fd: RawFd,
    buffers: Arc<UdpRecvBuffers>,
    message: Box<libc::msghdr>,
    recv_armed: bool,
    provided: usize,
    recycle: VecDeque<u16>,
    timeout_armed: bool,
}

impl UringUdpReceiver {
    pub fn new(fd: RawFd, buffer_count: u16, buffer_size: usize) -> io::Result<Self> {
        if buffer_count == 0 || buffer_size < 256 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid UDP receive buffer geometry",
            ));
        }
        let ring = build_ring(u32::from(buffer_count.next_power_of_two().max(8)))?;
        let buffers = Arc::new(UdpRecvBuffers::new_buffers(buffer_count, buffer_size));
        let message = Box::new(libc::msghdr {
            msg_name: std::ptr::null_mut(),
            msg_namelen: std::mem::size_of::<libc::sockaddr_storage>() as libc::socklen_t,
            msg_iov: std::ptr::null_mut(),
            msg_iovlen: 0,
            msg_control: std::ptr::null_mut(),
            msg_controllen: 0,
            msg_flags: 0,
        });
        let mut receiver = Self {
            ring,
            fd,
            buffers,
            message,
            recv_armed: false,
            provided: 0,
            recycle: VecDeque::with_capacity(usize::from(buffer_count)),
            timeout_armed: false,
        };
        receiver.provide_all()?;
        receiver.ring.submit_and_wait(1)?;
        receiver.drain_provided(true)?;
        receiver.arm_recv()?;
        receiver.ring.submit()?;
        Ok(receiver)
    }

    pub fn buffers(&self) -> Arc<UdpRecvBuffers> {
        self.buffers.clone()
    }

    pub fn available_buffers(&self) -> usize {
        self.provided
    }

    pub fn recycle(&mut self, buffer_id: u16) -> io::Result<()> {
        if buffer_id >= self.buffers.count {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "UDP receive buffer id out of range",
            ));
        }
        self.recycle.push_back(buffer_id);
        self.flush_recycle()
    }

    pub fn poll(
        &mut self,
        timeout: Duration,
        datagrams: &mut [UdpRecvDatagram],
    ) -> io::Result<usize> {
        self.flush_recycle()?;
        if !self.recv_armed && self.provided != 0 {
            self.arm_recv()?;
        }
        if !timeout.is_zero() && !self.timeout_armed {
            let timespec = types::Timespec::from(timeout);
            let entry = opcode::Timeout::new(&timespec)
                .build()
                .user_data(OpTag::new(OpKind::Timeout, 0, 0).unwrap().encode());
            unsafe { self.push(&entry)? };
            self.timeout_armed = true;
        }
        self.ring.submit_and_wait(usize::from(!timeout.is_zero()))?;

        let mut count = 0;
        let mut recv_more = false;
        {
            let cq = self.ring.completion();
            for completion in cq {
                let Some(tag) = OpTag::decode(completion.user_data()) else {
                    continue;
                };
                match tag.kind {
                    OpKind::Timeout => self.timeout_armed = false,
                    OpKind::PollCancel => {}
                    OpKind::UdpRx if tag.generation == PROVIDED_GENERATION => {
                        if completion.result() < 0 {
                            return Err(io::Error::from_raw_os_error(-completion.result()));
                        }
                        self.provided = self.provided.saturating_add(1);
                    }
                    OpKind::UdpRx if tag.generation == RECV_GENERATION => {
                        recv_more |= io_uring::cqueue::more(completion.flags());
                        if completion.result() < 0 {
                            let error = io::Error::from_raw_os_error(-completion.result());
                            if error.raw_os_error() != Some(libc::ENOBUFS) {
                                return Err(error);
                            }
                            continue;
                        }
                        self.provided = self.provided.saturating_sub(1);
                        let Some(buffer_id) = io_uring::cqueue::buffer_select(completion.flags())
                        else {
                            return Err(io::Error::other(
                                "UDP multishot completion did not select a buffer",
                            ));
                        };
                        if count == datagrams.len() {
                            self.recycle.push_back(buffer_id);
                            continue;
                        }
                        let result_len = usize::try_from(completion.result()).map_err(|_| {
                            io::Error::other("UDP receive completion length overflow")
                        })?;
                        let parsed = types::RecvMsgOut::parse(
                            &self.buffers.bytes[usize::from(buffer_id) * self.buffers.buffer_size
                                ..usize::from(buffer_id) * self.buffers.buffer_size + result_len],
                            &self.message,
                        )
                        .map_err(|_| {
                            io::Error::new(io::ErrorKind::InvalidData, "invalid UDP recvmsg output")
                        })?;
                        let Some(peer) = socket_addr(parsed.name_data()) else {
                            self.recycle.push_back(buffer_id);
                            continue;
                        };
                        let base = self
                            .buffers
                            .bytes
                            .as_ptr()
                            .wrapping_add(usize::from(buffer_id) * self.buffers.buffer_size)
                            as usize;
                        let payload = parsed.payload_data();
                        let offset = (payload.as_ptr() as usize).saturating_sub(base);
                        datagrams[count] = UdpRecvDatagram {
                            buffer_id,
                            offset,
                            len: payload.len(),
                            peer,
                        };
                        count += 1;
                    }
                    _ => {}
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
        self.flush_recycle()?;
        if !recv_more {
            self.recv_armed = false;
        }
        if !self.recv_armed && self.provided != 0 {
            self.arm_recv()?;
            self.ring.submit()?;
        }
        Ok(count)
    }

    fn provide_all(&mut self) -> io::Result<()> {
        let entry = opcode::ProvideBuffers::new(
            self.buffers.bytes.as_ptr() as *mut u8,
            self.buffers.buffer_size as i32,
            self.buffers.count,
            BUFFER_GROUP,
            0,
        )
        .build()
        .user_data(
            OpTag::new(OpKind::UdpRx, 0, PROVIDED_GENERATION)
                .unwrap()
                .encode(),
        );
        unsafe { self.push(&entry) }
    }

    fn flush_recycle(&mut self) -> io::Result<()> {
        while let Some(buffer_id) = self.recycle.front().copied() {
            let address = unsafe {
                self.buffers
                    .bytes
                    .as_ptr()
                    .add(usize::from(buffer_id) * self.buffers.buffer_size)
                    as *mut u8
            };
            let entry = opcode::ProvideBuffers::new(
                address,
                self.buffers.buffer_size as i32,
                1,
                BUFFER_GROUP,
                buffer_id,
            )
            .build()
            .user_data(
                OpTag::new(OpKind::UdpRx, 0, PROVIDED_GENERATION)
                    .unwrap()
                    .encode(),
            );
            if unsafe { self.push(&entry) }.is_err() {
                break;
            }
            self.recycle.pop_front();
        }
        if !self.recycle.is_empty() {
            self.ring.submit()?;
        }
        Ok(())
    }

    fn drain_provided(&mut self, require_one: bool) -> io::Result<()> {
        let mut count = 0;
        let cq = self.ring.completion();
        for completion in cq {
            let Some(tag) = OpTag::decode(completion.user_data()) else {
                continue;
            };
            if tag.kind == OpKind::UdpRx && tag.generation == PROVIDED_GENERATION {
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
        self.provided = if count == 0 {
            0
        } else {
            usize::from(self.buffers.count)
        };
        Ok(())
    }

    fn arm_recv(&mut self) -> io::Result<()> {
        if self.recv_armed || self.provided == 0 {
            return Ok(());
        }
        let entry =
            opcode::RecvMsgMulti::new(types::Fd(self.fd), self.message.as_ref(), BUFFER_GROUP)
                .build()
                .user_data(
                    OpTag::new(OpKind::UdpRx, 0, RECV_GENERATION)
                        .unwrap()
                        .encode(),
                );
        unsafe { self.push(&entry)? };
        self.recv_armed = true;
        Ok(())
    }

    unsafe fn push(&mut self, entry: &io_uring::squeue::Entry) -> io::Result<()> {
        unsafe { self.ring.submission().push(entry) }
            .map_err(|_| io::Error::new(io::ErrorKind::WouldBlock, "io_uring SQ full"))
    }
}

pub(crate) fn parse_socket_addr(bytes: &[u8]) -> Option<SocketAddr> {
    socket_addr(bytes)
}

fn socket_addr(bytes: &[u8]) -> Option<SocketAddr> {
    if bytes.len() < 2 {
        return None;
    }
    let family = u16::from_ne_bytes([bytes[0], bytes[1]]) as i32;
    match family {
        libc::AF_INET if bytes.len() >= 8 => Some(SocketAddr::new(
            IpAddr::V4(Ipv4Addr::new(bytes[4], bytes[5], bytes[6], bytes[7])),
            u16::from_be_bytes([bytes[2], bytes[3]]),
        )),
        libc::AF_INET6 if bytes.len() >= 28 => {
            let mut address = [0_u8; 16];
            address.copy_from_slice(&bytes[8..24]);
            Some(SocketAddr::new(
                IpAddr::V6(Ipv6Addr::from(address)),
                u16::from_be_bytes([bytes[2], bytes[3]]),
            ))
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::UdpSocket;
    use std::os::fd::AsRawFd;

    #[test]
    fn multishot_receive_returns_peer_and_recyclable_payload() {
        let receiver = UdpSocket::bind("127.0.0.1:0").unwrap();
        let sender = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut uring = match UringUdpReceiver::new(receiver.as_raw_fd(), 8, 2_048) {
            Ok(uring) => uring,
            Err(error)
                if matches!(
                    error.kind(),
                    io::ErrorKind::PermissionDenied | io::ErrorKind::Unsupported
                ) =>
            {
                return;
            }
            Err(error) => panic!("io_uring receiver unavailable: {error}"),
        };
        sender
            .send_to(b"srt", receiver.local_addr().unwrap())
            .unwrap();
        let mut datagrams = [UdpRecvDatagram {
            buffer_id: 0,
            offset: 0,
            len: 0,
            peer: "0.0.0.0:0".parse().unwrap(),
        }; 2];
        let count = uring.poll(Duration::from_secs(1), &mut datagrams).unwrap();
        assert_eq!(count, 1);
        let datagram = datagrams[0];
        assert_eq!(datagram.peer, sender.local_addr().unwrap());
        assert_eq!(
            uring
                .buffers()
                .payload(datagram.buffer_id, datagram.offset, datagram.len),
            Some(&b"srt"[..])
        );
        uring.recycle(datagram.buffer_id).unwrap();
    }
}
