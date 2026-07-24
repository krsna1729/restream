#![allow(dead_code)]

//! RTMP fabric protocol engine: the shard-scheduled, readiness-driven
//! counterpart to the RTMP fabric's [`crate::media::egress::backends::tcp`]
//! poller and [`crate::media::egress::backends::tcp_connect`] dial.
//!
//! This slice covers connection startup through a completed RTMP
//! handshake, reusing [`rtmp_handshake::NonBlockingRtmpHandshake`]. RTMP
//! session negotiation (connect/publish requests) and media publishing are
//! deliberately deferred to the next slice rather than rushed here — see
//! `docs/egress-implementation.md` Phase 5 status for the reuse path
//! (`RtmpSessionCore` in `src/media/rtmp/egress_connection.rs` is already a
//! pure, socket-independent state machine suitable for driving the same
//! way this file drives the handshake).

use std::net::TcpStream;

use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, ProtocolFailure, Readiness,
    RecoveryCapability,
};
use crate::media::egress::feed::FeedCursor;
use crate::media::egress::journal::RingFeed;
use crate::media::egress::policy::WorkBudget;

use super::rtmp_handshake::{HandshakeOutcome, NonBlockingRtmpHandshake};

enum RtmpFabricState {
    Handshaking(Box<NonBlockingRtmpHandshake>),
    /// Handshake complete. `carried_over` is any chunk-stream bytes the
    /// peer sent immediately after the handshake, not yet consumed — the
    /// next slice's session-negotiation step must process these first,
    /// before reading anything further from the transport.
    HandshakeDone {
        carried_over: Vec<u8>,
    },
}

pub(crate) struct RtmpFabricEngine {
    state: RtmpFabricState,
}

impl RtmpFabricEngine {
    pub(crate) fn new_client() -> Result<Self, String> {
        Ok(Self {
            state: RtmpFabricState::Handshaking(Box::new(NonBlockingRtmpHandshake::new_client()?)),
        })
    }

    #[cfg(test)]
    pub(crate) fn is_handshake_done(&self) -> bool {
        matches!(self.state, RtmpFabricState::HandshakeDone { .. })
    }
}

impl ProtocolEngine for RtmpFabricEngine {
    type Feed = RingFeed;
    type Transport = TcpStream;

    fn advance(
        &mut self,
        transport: &mut Self::Transport,
        readiness: Readiness,
        _feed: &Self::Feed,
        _cursor: &mut FeedCursor,
        _budget: WorkBudget,
    ) -> EngineProgress {
        match &mut self.state {
            RtmpFabricState::Handshaking(handshake) => {
                match handshake.advance(transport, readiness) {
                    HandshakeOutcome::Pending(interest) => EngineProgress::Needs(interest),
                    HandshakeOutcome::Complete { remaining } => {
                        self.state = RtmpFabricState::HandshakeDone {
                            carried_over: remaining,
                        };
                        EngineProgress::HandshakeComplete
                    }
                    HandshakeOutcome::Failed(detail) => EngineProgress::Failed(ProtocolFailure {
                        reason: "rtmp_handshake",
                        detail,
                        retryable: true,
                    }),
                }
            }
            // Session negotiation (connect/publish requests) and media
            // publishing are the next slice; until then the leaf parks here
            // rather than claiming false progress.
            RtmpFabricState::HandshakeDone { .. } => EngineProgress::Needs(Interest::NONE),
        }
    }

    fn close(&mut self, transport: &mut Self::Transport, _reason: CloseReason) {
        let _ = transport.shutdown(std::net::Shutdown::Both);
    }

    fn recovery_capability(&self) -> RecoveryCapability {
        RecoveryCapability::ReconnectOnly
    }
}

#[cfg(test)]
#[path = "rtmp_tests.rs"]
mod tests;
