#![allow(dead_code)]

//! RTMP fabric protocol engine: the shard-scheduled, readiness-driven
//! counterpart to the RTMP fabric's [`crate::media::egress::backends::tcp`]
//! poller and [`crate::media::egress::backends::tcp_connect`] dial.
//!
//! This slice covers connection startup through an accepted publish
//! request: the RTMP handshake (via [`rtmp_handshake::NonBlockingRtmpHandshake`])
//! followed by connect/publish session negotiation, reusing
//! [`crate::media::rtmp::RtmpSessionCore`] — the same pure,
//! socket-independent `ClientSession` driver the existing Tokio-adapted
//! egress path uses (`src/media/rtmp/egress_connection.rs`), here driven
//! from non-blocking readiness instead of `.await`. Media publishing
//! (draining `RingFeed` into `publish_video_data`/`publish_audio_data`) is
//! deliberately deferred to the next slice — see
//! `docs/egress-implementation.md` Phase 5 status.

use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};
use std::net::TcpStream;

use bytes::Bytes;

use crate::media::egress::backend::{
    CloseReason, EngineProgress, Interest, ProtocolEngine, ProtocolFailure, Readiness,
    RecoveryCapability,
};
use crate::media::egress::feed::FeedCursor;
use crate::media::egress::journal::RingFeed;
use crate::media::egress::policy::WorkBudget;
use crate::media::rtmp::{RtmpSessionCore, RtmpSessionError, RtmpSessionEvent, RtmpUrlParts};

use super::rtmp_handshake::{HandshakeOutcome, NonBlockingRtmpHandshake};

const SESSION_READ_BUFFER: usize = 4096;

struct PendingWrite {
    bytes: Bytes,
    offset: usize,
}

impl PendingWrite {
    fn new(bytes: Bytes) -> Option<Self> {
        if bytes.is_empty() {
            None
        } else {
            Some(Self { bytes, offset: 0 })
        }
    }

    fn remaining(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }

    fn is_complete(&self) -> bool {
        self.offset >= self.bytes.len()
    }
}

enum SessionAdvanceOutcome {
    Pending(Interest),
    PublishAccepted,
    Failed(String),
}

/// Drives connect/publish request negotiation over an already-handshaken
/// transport, reusing [`RtmpSessionCore`]'s pure protocol calls. Bounded to
/// at most one read or one write syscall per [`Self::advance`] call, matching
/// [`NonBlockingRtmpHandshake`]'s per-visit work discipline.
struct SessionNegotiation {
    core: RtmpSessionCore,
    outbound: VecDeque<Bytes>,
    pending_write: Option<PendingWrite>,
    unread: Vec<u8>,
    publish_accepted: bool,
}

impl SessionNegotiation {
    fn new(
        mut core: RtmpSessionCore,
        carried_over: Vec<u8>,
        enhanced: bool,
    ) -> Result<Self, String> {
        let mut outbound: VecDeque<Bytes> = core.take_initial_packets().into();
        outbound.push_back(core.request_connection(enhanced)?);
        Ok(Self {
            core,
            outbound,
            pending_write: None,
            unread: carried_over,
            publish_accepted: false,
        })
    }

    fn advance(&mut self, stream: &mut TcpStream, readiness: Readiness) -> SessionAdvanceOutcome {
        if let Some(pending) = &mut self.pending_write {
            if !readiness.writable {
                return SessionAdvanceOutcome::Pending(Interest::WRITE);
            }
            match stream.write(pending.remaining()) {
                Ok(0) => {
                    return SessionAdvanceOutcome::Failed("peer closed during write".to_string());
                }
                Ok(n) => {
                    pending.offset += n;
                    if !pending.is_complete() {
                        return SessionAdvanceOutcome::Pending(Interest::WRITE);
                    }
                    self.pending_write = None;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return SessionAdvanceOutcome::Pending(Interest::WRITE);
                }
                Err(error) => return SessionAdvanceOutcome::Failed(error.to_string()),
            }
        }

        if self.pending_write.is_none() {
            while self.pending_write.is_none() {
                match self.outbound.pop_front() {
                    Some(next) => self.pending_write = PendingWrite::new(next),
                    None => break,
                }
            }
            if self.pending_write.is_some() {
                return SessionAdvanceOutcome::Pending(Interest::WRITE);
            }
        }

        if !self.unread.is_empty() {
            let input = std::mem::take(&mut self.unread);
            return match self.core.handle_server_input(&input) {
                Ok((packets, events)) => {
                    self.outbound.extend(packets);
                    if events
                        .iter()
                        .any(|event| matches!(event, RtmpSessionEvent::PublishRequestAccepted))
                    {
                        self.publish_accepted = true;
                    }
                    let interest = if self.outbound.is_empty() {
                        Interest::READ
                    } else {
                        Interest::WRITE
                    };
                    SessionAdvanceOutcome::Pending(interest)
                }
                Err(RtmpSessionError::ConnectionRejected(description)) => {
                    SessionAdvanceOutcome::Failed(format!("connection rejected: {description}"))
                }
                Err(other) => SessionAdvanceOutcome::Failed(other.to_string()),
            };
        }

        if self.publish_accepted && self.outbound.is_empty() && self.pending_write.is_none() {
            return SessionAdvanceOutcome::PublishAccepted;
        }

        if !readiness.readable {
            return SessionAdvanceOutcome::Pending(Interest::READ);
        }

        let mut buffer = [0u8; SESSION_READ_BUFFER];
        match stream.read(&mut buffer) {
            Ok(0) => {
                SessionAdvanceOutcome::Failed("peer closed during session negotiation".to_string())
            }
            Ok(n) => {
                self.unread = buffer[..n].to_vec();
                SessionAdvanceOutcome::Pending(Interest::READ_WRITE)
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                SessionAdvanceOutcome::Pending(Interest::READ)
            }
            Err(error) => SessionAdvanceOutcome::Failed(error.to_string()),
        }
    }
}

enum RtmpFabricState {
    Handshaking(Box<NonBlockingRtmpHandshake>),
    Negotiating(Box<SessionNegotiation>),
    /// Connect and publish requests both accepted. Media publishing (draining
    /// `RingFeed` into `publish_video_data`/`publish_audio_data`) is the next
    /// slice; until then the leaf parks here rather than claiming false
    /// progress.
    PublishAccepted {
        #[allow(dead_code)]
        core: Box<RtmpSessionCore>,
    },
}

pub(crate) struct RtmpFabricEngine {
    /// `None` only transiently, inside `advance`, while a state transition
    /// takes ownership of the previous state to build the next one — never
    /// observed outside this file.
    state: Option<RtmpFabricState>,
    /// Taken once, when the handshake completes and the session core is
    /// constructed; `None` afterward.
    parts: Option<RtmpUrlParts>,
    chunk_size: u32,
    enhanced: bool,
}

impl RtmpFabricEngine {
    pub(crate) fn new_client(
        parts: RtmpUrlParts,
        chunk_size: u32,
        enhanced: bool,
    ) -> Result<Self, String> {
        Ok(Self {
            state: Some(RtmpFabricState::Handshaking(Box::new(
                NonBlockingRtmpHandshake::new_client()?,
            ))),
            parts: Some(parts),
            chunk_size,
            enhanced,
        })
    }

    #[cfg(test)]
    pub(crate) fn is_handshake_done(&self) -> bool {
        !matches!(self.state, Some(RtmpFabricState::Handshaking(_)))
    }

    #[cfg(test)]
    pub(crate) fn is_publish_accepted(&self) -> bool {
        matches!(self.state, Some(RtmpFabricState::PublishAccepted { .. }))
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
        match self.state.take().expect("state is only None transiently") {
            RtmpFabricState::Handshaking(mut handshake) => {
                let outcome = handshake.advance(transport, readiness);
                match outcome {
                    HandshakeOutcome::Pending(interest) => {
                        self.state = Some(RtmpFabricState::Handshaking(handshake));
                        EngineProgress::Needs(interest)
                    }
                    HandshakeOutcome::Complete { remaining } => {
                        let parts = self
                            .parts
                            .take()
                            .expect("parts are only taken once, on this transition");
                        let core = match RtmpSessionCore::new(parts, self.chunk_size) {
                            Ok(core) => core,
                            Err(detail) => {
                                return EngineProgress::Failed(ProtocolFailure {
                                    reason: "rtmp_session_init",
                                    detail,
                                    retryable: true,
                                });
                            }
                        };
                        match SessionNegotiation::new(core, remaining, self.enhanced) {
                            Ok(negotiation) => {
                                self.state =
                                    Some(RtmpFabricState::Negotiating(Box::new(negotiation)));
                                EngineProgress::HandshakeComplete
                            }
                            Err(detail) => EngineProgress::Failed(ProtocolFailure {
                                reason: "rtmp_connect_request",
                                detail,
                                retryable: true,
                            }),
                        }
                    }
                    HandshakeOutcome::Failed(detail) => EngineProgress::Failed(ProtocolFailure {
                        reason: "rtmp_handshake",
                        detail,
                        retryable: true,
                    }),
                }
            }
            RtmpFabricState::Negotiating(mut negotiation) => {
                let outcome = negotiation.advance(transport, readiness);
                match outcome {
                    SessionAdvanceOutcome::Pending(interest) => {
                        self.state = Some(RtmpFabricState::Negotiating(negotiation));
                        EngineProgress::Needs(interest)
                    }
                    SessionAdvanceOutcome::PublishAccepted => {
                        self.state = Some(RtmpFabricState::PublishAccepted {
                            core: Box::new(negotiation.core),
                        });
                        EngineProgress::HandshakeComplete
                    }
                    SessionAdvanceOutcome::Failed(detail) => {
                        EngineProgress::Failed(ProtocolFailure {
                            reason: "rtmp_session_negotiation",
                            detail,
                            retryable: true,
                        })
                    }
                }
            }
            state @ RtmpFabricState::PublishAccepted { .. } => {
                self.state = Some(state);
                EngineProgress::Needs(Interest::NONE)
            }
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
