use std::collections::VecDeque;
use std::io::{ErrorKind, Read, Write};

use bytes::Bytes;

use crate::media::egress::backend::{Interest, Readiness};
use crate::media::rtmp::{RtmpSessionCore, RtmpSessionError, RtmpSessionEvent};

use super::RtmpConnection;
use super::SESSION_READ_BUFFER;

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
        stream: &mut RtmpConnection,
        readiness: Readiness,
    ) -> SessionAdvanceOutcome {
        let mut wrote = false;
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
                    wrote = true;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return SessionAdvanceOutcome::Pending(stream.interest_hint(Interest::WRITE));
                }
                Err(error) => return SessionAdvanceOutcome::Failed(error.to_string()),
            }
        }

        if !self.unread.is_empty() {
            let input = std::mem::take(&mut self.unread);
            match self.core.handle_server_input(&input) {
                Ok((packets, events)) => {
                    self.outbound.extend(packets);
                    if events
                        .iter()
                        .any(|event| matches!(event, RtmpSessionEvent::PublishRequestAccepted))
                    {
                        self.publish_accepted = true;
                    }
                }
                Err(RtmpSessionError::ConnectionRejected(description)) => {
                    return SessionAdvanceOutcome::Failed(format!(
                        "connection rejected: {description}"
                    ));
                }
                Err(other) => return SessionAdvanceOutcome::Failed(other.to_string()),
            }
        }

        if self.pending_write.is_none() {
            while let Some(next) = self.outbound.pop_front() {
                if let Some(pending) = PendingWrite::new(next) {
                    self.pending_write = Some(pending);
                    break;
                }
            }
        }

        if let Some(pending) = &mut self.pending_write {
            if wrote || !readiness.writable {
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
                    wrote = true;
                }
                Err(error) if error.kind() == ErrorKind::WouldBlock => {
                    return SessionAdvanceOutcome::Pending(stream.interest_hint(Interest::WRITE));
                }
                Err(error) => return SessionAdvanceOutcome::Failed(error.to_string()),
            }
        }

        if self.publish_accepted && self.outbound.is_empty() && self.pending_write.is_none() {
            return SessionAdvanceOutcome::PublishAccepted;
        }
        if wrote {
            return SessionAdvanceOutcome::Pending(Interest::READ);
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
                SessionAdvanceOutcome::Pending(stream.interest_hint(Interest::READ))
            }
            Err(error) => SessionAdvanceOutcome::Failed(error.to_string()),
        }
    }
}
