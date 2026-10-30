//! Runtime io_uring capability discovery for operator telemetry.
//!
//! Probes one short-lived ring; it does not select or configure a transport
//! (production transports require io_uring through Compio and fail
//! explicitly without it).

use std::io;

use io_uring::{IoUring, Probe, opcode};

fn build_ring(entries: u32) -> io::Result<IoUring> {
    let mut builder = IoUring::builder();
    builder
        .setup_single_issuer()
        .setup_defer_taskrun()
        .setup_coop_taskrun()
        .setup_taskrun_flag()
        .setup_cqsize(entries.saturating_mul(2));
    match builder.build(entries) {
        Ok(ring) => Ok(ring),
        Err(_) => IoUring::new(entries),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UringCapabilities {
    pub poll_add: bool,
    pub accept: bool,
    pub accept_multishot: bool,
    pub connect: bool,
    pub recv: bool,
    pub recv_msg: bool,
    pub recv_msg_multi: bool,
    pub recv_multishot: bool,
    pub provide_buffers: bool,
    pub recv_bundle: bool,
    pub recv_zc: bool,
    pub send: bool,
    pub send_msg: bool,
    /// Ordinary scatter/gather through `sendmsg` with an iovec array. This is
    /// NOT the newer `IORING_SEND_VECTORIZED` facility; it is named for what
    /// the opcode probe actually proves.
    pub sendmsg_iovec: bool,
    pub send_zc: bool,
    pub send_bundle: bool,
    /// NAPI busy-poll registration is a Linux kernel facility rather than an
    /// io_uring opcode. It is exposed separately so deployment profiles can
    /// gate the optional optimization without making it a correctness path.
    pub napi: bool,
}

/// Ordered deployment profiles. Tier 0 is the correctness baseline; higher
/// tiers are optional performance challengers and must not be required by the
/// dataplane's protocol implementations.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UringCapabilityTier {
    Unsupported,
    Tier0,
    Tier1,
    Tier2,
    Tier3,
    Tier4,
}

impl UringCapabilityTier {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Unsupported => "unsupported",
            Self::Tier0 => "tier0",
            Self::Tier1 => "tier1",
            Self::Tier2 => "tier2",
            Self::Tier3 => "tier3",
            Self::Tier4 => "tier4",
        }
    }
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
            provide_buffers: probe.is_supported(opcode::ProvideBuffers::CODE),
            recv_bundle: probe.is_supported(opcode::RecvBundle::CODE),
            recv_zc: probe.is_supported(opcode::RecvZc::CODE),
            send: probe.is_supported(opcode::Send::CODE),
            send_msg: probe.is_supported(opcode::SendMsg::CODE),
            sendmsg_iovec: probe.is_supported(opcode::SendMsg::CODE),
            send_zc: probe.is_supported(opcode::SendZc::CODE),
            send_bundle: probe.is_supported(opcode::SendBundle::CODE),
            // No NAPI busy-poll registration is performed by restream yet;
            // the host OS alone is not evidence that the feature is usable.
            napi: false,
        })
    }

    /// Select the highest profile whose required primitives were actually
    /// probed. Fixed-file registration is validated by `FixedFileTable` when
    /// a native poller is constructed; it is therefore part of the Tier 0
    /// contract without being guessed from the opcode probe.
    pub const fn deployment_tier(self) -> UringCapabilityTier {
        if !self.poll_add || !self.send_msg {
            return UringCapabilityTier::Unsupported;
        }
        if self.recv_zc && self.napi {
            return UringCapabilityTier::Tier4;
        }
        if self.send_zc {
            return UringCapabilityTier::Tier3;
        }
        if self.recv_bundle && self.send_bundle {
            return UringCapabilityTier::Tier2;
        }
        if self.accept_multishot && self.recv_multishot && self.provide_buffers {
            return UringCapabilityTier::Tier1;
        }
        UringCapabilityTier::Tier0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deployment_tier_requires_the_correctness_baseline() {
        let optional = UringCapabilities {
            send_zc: true,
            recv_zc: true,
            napi: true,
            ..Default::default()
        };
        assert_eq!(optional.deployment_tier(), UringCapabilityTier::Unsupported);
    }

    #[test]
    fn deployment_tier_picks_the_highest_proven_profile() {
        let tier1 = UringCapabilities {
            poll_add: true,
            send_msg: true,
            accept_multishot: true,
            recv_multishot: true,
            provide_buffers: true,
            ..Default::default()
        };
        assert_eq!(tier1.deployment_tier(), UringCapabilityTier::Tier1);

        let tier3 = UringCapabilities {
            send_zc: true,
            ..tier1
        };
        assert_eq!(tier3.deployment_tier(), UringCapabilityTier::Tier3);
    }

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
