#![allow(dead_code)]

//! Non-blocking RTMP client handshake for the fabric TCP leaf.
//!
//! Drives `rml_rtmp::handshake::Handshake` — the same pure, socket-
//! independent state machine the existing Tokio adapter uses
//! (`src/media/rtmp/handshake.rs`) — over a non-blocking `TcpStream`
//! instead of an async one, one bounded step per call so it fits the
//! fabric's readiness-driven visit model (`TcpEgressPoller` +
//! `tcp_connect`) rather than an `.await`ed read/write loop.

use std::io::{ErrorKind, Read, Write};

use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};

use crate::media::egress::backend::{Interest, Readiness};

use super::rtmp_connection::RtmpConnection;

const HANDSHAKE_READ_BUFFER: usize = 4096;

#[derive(Debug)]
pub(crate) enum HandshakeOutcome {
    /// Not done yet; register for this interest and call `advance` again
    /// once the transport reports it.
    Pending(Interest),
    /// Handshake complete. `remaining` is any RTMP chunk-stream bytes the
    /// peer sent immediately after the handshake, already read and not yet
    /// consumed by the session layer.
    Complete {
        remaining: Vec<u8>,
    },
    Failed(String),
}

#[derive(Debug)]
struct PendingWrite {
    bytes: Vec<u8>,
    offset: usize,
}

impl PendingWrite {
    fn new(bytes: Vec<u8>) -> Option<Self> {
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

pub(crate) struct NonBlockingRtmpHandshake {
    handshake: Handshake,
    pending_write: Option<PendingWrite>,
    completed_remaining: Option<Vec<u8>>,
}

impl NonBlockingRtmpHandshake {
    /// Start a client handshake: generates C0/C1 immediately, queued as the
    /// first pending write.
    pub(crate) fn new_client() -> Result<Self, String> {
        let mut handshake = Handshake::new(PeerType::Client);
        let c0_c1 = handshake
            .generate_outbound_p0_and_p1()
            .map_err(|error| format!("{error:?}"))?;
        Ok(Self {
            handshake,
            pending_write: PendingWrite::new(c0_c1),
            completed_remaining: None,
        })
    }

    /// Advance the handshake by at most one non-blocking read or write,
    /// matching the fabric's bounded-work-per-visit contract.
    pub(crate) fn advance(
        &mut self,
        stream: &mut RtmpConnection,
        readiness: Readiness,
    ) -> HandshakeOutcome {
        // A pending write (including the final C2 response that accompanies
        // a `Completed` result) must always be fully flushed before the
        // handshake can be reported done — checking `completed_remaining`
        // ahead of this would abandon that last write mid-flight and leave
        // the peer blocked waiting for bytes that were never sent.
        if let Some(pending) = &mut self.pending_write {
            if !readiness.writable {
                return HandshakeOutcome::Pending(Interest::WRITE);
            }
            match stream.write(pending.remaining()) {
                Ok(0) => return HandshakeOutcome::Failed("peer closed during write".to_string()),
                Ok(n) => {
                    pending.offset += n;
                    if !pending.is_complete() {
                        return HandshakeOutcome::Pending(Interest::WRITE);
                    }
                    self.pending_write = None;
                    // Fall through: the flush may have been the last
                    // handshake response, in which case `completed_remaining`
                    // is now ready to report.
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return HandshakeOutcome::Pending(stream.interest_hint(Interest::WRITE));
                }
                Err(error) => return HandshakeOutcome::Failed(error.to_string()),
            }
        }

        if let Some(remaining) = self.completed_remaining.take() {
            return HandshakeOutcome::Complete { remaining };
        }

        if !readiness.readable {
            return HandshakeOutcome::Pending(Interest::READ);
        }

        let mut buffer = [0u8; HANDSHAKE_READ_BUFFER];
        match stream.read(&mut buffer) {
            Ok(0) => HandshakeOutcome::Failed("peer closed during handshake".to_string()),
            Ok(n) => self.process_bytes(&buffer[..n]),
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                HandshakeOutcome::Pending(stream.interest_hint(Interest::READ))
            }
            Err(error) => HandshakeOutcome::Failed(error.to_string()),
        }
    }

    fn process_bytes(&mut self, data: &[u8]) -> HandshakeOutcome {
        match self.handshake.process_bytes(data) {
            Ok(HandshakeProcessResult::InProgress { response_bytes }) => {
                self.pending_write = PendingWrite::new(response_bytes);
                if self.pending_write.is_some() {
                    HandshakeOutcome::Pending(Interest::WRITE)
                } else {
                    HandshakeOutcome::Pending(Interest::READ)
                }
            }
            Ok(HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            }) => {
                self.pending_write = PendingWrite::new(response_bytes);
                if self.pending_write.is_some() {
                    // Flush the final response before reporting completion.
                    self.completed_remaining = Some(remaining_bytes);
                    HandshakeOutcome::Pending(Interest::WRITE)
                } else {
                    HandshakeOutcome::Complete {
                        remaining: remaining_bytes,
                    }
                }
            }
            Err(error) => HandshakeOutcome::Failed(format!("{error:?}")),
        }
    }
}

#[cfg(test)]
#[path = "rtmp_handshake_tests.rs"]
mod tests;
