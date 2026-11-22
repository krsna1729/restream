use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};

use bytes::Bytes;

use crate::egress::backend::{Interest, Readiness};
use restream_rtmp::RtmpSessionCore;
use restream_rtmp::RtmpSessionError;
use restream_rtmp::RtmpSessionEvent;

use super::SESSION_READ_BUFFER;
use crate::egress::tls::TlsTcpConnection;

pub(super) struct PendingWrite {
    pub(super) bytes: Bytes,
    pub(super) offset: usize,
}

impl PendingWrite {
    pub(super) fn new(bytes: Bytes) -> Option<Self> {
        if bytes.is_empty() {
            None
        } else {
            Some(Self { bytes, offset: 0 })
        }
    }

    pub(super) fn remaining(&self) -> &[u8] {
        &self.bytes[self.offset..]
    }

    pub(super) fn is_complete(&self) -> bool {
        self.offset >= self.bytes.len()
    }
}

pub(super) enum SessionAdvanceOutcome {
    Pending(Interest),
    PublishAccepted,
    Failed(String),
}

/// Drives connect/publish negotiation over an already-handshaken transport.
pub(super) struct SessionNegotiation {
    pub(super) core: RtmpSessionCore,
    outbound: VecDeque<Bytes>,
    pending_write: Option<PendingWrite>,
    unread: Vec<u8>,
    publish_accepted: bool,
}

impl SessionNegotiation {
    pub(super) fn new(
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

    pub(super) fn advance(
        &mut self,
        stream: &mut TlsTcpConnection,
        readiness: Readiness,
    ) -> SessionAdvanceOutcome {
        let wrote = match self.write_pending(stream, readiness.writable) {
            Ok(wrote) => wrote,
            Err(outcome) => return outcome,
        };
        if let Err(error) = self.process_unread() {
            return SessionAdvanceOutcome::Failed(error);
        }

        // A successful write_pending leaves the slot empty.
        self.pending_write =
            std::iter::from_fn(|| self.outbound.pop_front()).find_map(PendingWrite::new);

        // A visit writes at most once, even if parsing input queued another reply.
        let wrote_queued = match self.write_pending(stream, readiness.writable && !wrote) {
            Ok(wrote_queued) => wrote_queued,
            Err(outcome) => return outcome,
        };

        if self.publish_accepted && self.outbound.is_empty() && self.pending_write.is_none() {
            return SessionAdvanceOutcome::PublishAccepted;
        }
        if wrote || wrote_queued || !readiness.readable {
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
                SessionAdvanceOutcome::Pending(stream.interest_hint(Interest::READ))
            }
            Err(error) => SessionAdvanceOutcome::Failed(error.to_string()),
        }
    }

    fn write_pending(
        &mut self,
        stream: &mut TlsTcpConnection,
        writable: bool,
    ) -> Result<bool, SessionAdvanceOutcome> {
        let Some(pending) = &mut self.pending_write else {
            return Ok(false);
        };
        if !writable {
            return Err(SessionAdvanceOutcome::Pending(Interest::WRITE));
        }
        match stream.write(pending.remaining()) {
            Ok(0) => Err(SessionAdvanceOutcome::Failed(
                "peer closed during write".to_string(),
            )),
            Ok(n) => {
                pending.offset += n;
                if !pending.is_complete() {
                    return Err(SessionAdvanceOutcome::Pending(Interest::WRITE));
                }
                self.pending_write = None;
                Ok(true)
            }
            Err(error) if error.kind() == ErrorKind::WouldBlock => Err(
                SessionAdvanceOutcome::Pending(stream.interest_hint(Interest::WRITE)),
            ),
            Err(error) => Err(SessionAdvanceOutcome::Failed(error.to_string())),
        }
    }

    fn process_unread(&mut self) -> Result<(), String> {
        if self.unread.is_empty() {
            return Ok(());
        }
        let input = std::mem::take(&mut self.unread);
        let (packets, events) =
            self.core
                .handle_server_input(&input)
                .map_err(|error| match error {
                    RtmpSessionError::ConnectionRejected(description) => {
                        format!("connection rejected: {description}")
                    }
                    other => other.to_string(),
                })?;
        self.outbound.extend(packets);
        self.publish_accepted |= events
            .iter()
            .any(|event| matches!(event, RtmpSessionEvent::PublishRequestAccepted));
        Ok(())
    }
}
