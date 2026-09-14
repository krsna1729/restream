//! Runtime io_uring capability discovery.

use std::io;

use io_uring::{IoUring, Probe, opcode};

use crate::build_ring;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UringCapabilities {
    pub poll_add: bool,
    pub accept: bool,
    pub accept_multishot: bool,
    pub connect: bool,
    pub recv: bool,
    pub recv_msg: bool,
    pub recv_msg_multi: bool,
    pub recv_multishot: bool,
    pub recv_bundle: bool,
    pub recv_zc: bool,
    pub send: bool,
    pub send_msg: bool,
    pub send_vectored: bool,
    pub send_zc: bool,
    pub send_bundle: bool,
    /// NAPI busy-poll registration is a Linux kernel facility rather than an
    /// io_uring opcode. It is exposed separately so deployment profiles can
    /// gate the optional optimization without making it a correctness path.
    pub napi: bool,
}

impl UringCapabilities {
    pub fn probe(entries: u32) -> io::Result<Self> {
        let ring = build_ring(entries)?;
        Self::from_ring(&ring)
    }

    pub fn from_ring(ring: &IoUring) -> io::Result<Self> {
        let mut probe = Probe::new();
        ring.submitter().register_probe(&mut probe)?;
        Ok(Self {
            poll_add: probe.is_supported(opcode::PollAdd::CODE),
            accept: probe.is_supported(opcode::Accept::CODE),
            accept_multishot: probe.is_supported(opcode::AcceptMulti::CODE),
            connect: probe.is_supported(opcode::Connect::CODE),
            recv: probe.is_supported(opcode::Recv::CODE),
            recv_msg: probe.is_supported(opcode::RecvMsg::CODE),
            recv_msg_multi: probe.is_supported(opcode::RecvMsgMulti::CODE),
            recv_multishot: probe.is_supported(opcode::RecvMulti::CODE),
            recv_bundle: probe.is_supported(opcode::RecvBundle::CODE),
            recv_zc: probe.is_supported(opcode::RecvZc::CODE),
            send: probe.is_supported(opcode::Send::CODE),
            send_msg: probe.is_supported(opcode::SendMsg::CODE),
            send_vectored: probe.is_supported(opcode::SendMsg::CODE),
            send_zc: probe.is_supported(opcode::SendZc::CODE),
            send_bundle: probe.is_supported(opcode::SendBundle::CODE),
            napi: cfg!(target_os = "linux"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_is_safe_when_io_uring_is_unavailable() {
        match UringCapabilities::probe(8) {
            Ok(_) => {}
            Err(error) => assert!(matches!(
                error.kind(),
                io::ErrorKind::PermissionDenied
                    | io::ErrorKind::Unsupported
                    | io::ErrorKind::InvalidInput
            )),
        }
    }
}
