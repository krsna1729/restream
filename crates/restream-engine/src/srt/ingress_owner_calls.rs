//! Outbound calls (`srt_call` inputs) on this Owner's caller pool. A call
//! the pool admits becomes a session like an admitted peer; its protocol
//! events arrive through the payload work list (`ingress_owner_payload`).

use std::net::SocketAddr;

use srt_proto::Timestamp;
use srt_transport::advanced::caller::{LogicalCallerId, PoolEvent, PoolOutcome};

use super::{CallToken, OwnerLoop, SessionId, SrtIngressEvent};

impl OwnerLoop {
    /// Dial out for an `srt_call` input on this Owner's shared caller socket.
    /// A call the pool admits is a live session at once (its handshake is
    /// under way); `Connected` follows from the protocol.
    pub(super) fn call(
        &mut self,
        token: CallToken,
        remote: SocketAddr,
        request: restream_srt::egress_connect::SrtConnectRequest,
        now: Timestamp,
    ) {
        use restream_srt::egress_connect::SrtConnectKind;
        let owner = &mut self.owner;
        let outcome = self.runtime.block_on(async {
            match &request.kind {
                SrtConnectKind::Direct(config) => owner.connect(config, now),
                SrtConnectKind::Bonded(config) => owner.connect_bonded(config, now),
            }
        });
        match outcome {
            Ok(PoolOutcome::Admitted(caller)) => self.call_admitted(token, remote, caller),
            Ok(PoolOutcome::Queued(request)) => {
                self.queued_calls.insert(request, (token, remote));
            }
            Ok(PoolOutcome::Full) => self.pending_events.push_back(SrtIngressEvent::CallFailed {
                token,
                reason: "too many SRT calls in progress".to_string(),
            }),
            Err(error) => self.pending_events.push_back(SrtIngressEvent::CallFailed {
                token,
                reason: error.to_string(),
            }),
        }
    }

    fn call_admitted(&mut self, token: CallToken, remote: SocketAddr, caller: LogicalCallerId) {
        let session = SessionId::Caller(caller);
        self.addrs.insert(session, remote);
        self.pending_events.push_back(SrtIngressEvent::CallStarted {
            token,
            logical_peer: self.session(session),
        });
    }

    /// The pool's verdicts on queued calls, and attempt-deadline expiries.
    pub(super) fn drain_call_pool(&mut self) {
        self.owner.poll_caller_pool_events(&mut self.pool_scratch);
        let events = std::mem::take(&mut self.pool_scratch);
        for event in &events {
            match *event {
                PoolEvent::Admitted {
                    request_id,
                    caller_id,
                } => {
                    if let Some((token, remote)) = self.queued_calls.remove(&request_id) {
                        self.call_admitted(token, remote, caller_id);
                    }
                }
                PoolEvent::Failed {
                    request_id,
                    ref reason,
                } => {
                    if let Some((token, _)) = self.queued_calls.remove(&request_id) {
                        self.pending_events.push_back(SrtIngressEvent::CallFailed {
                            token,
                            reason: reason.clone(),
                        });
                    }
                }
                PoolEvent::Cancelled { request_id } => {
                    if let Some((token, _)) = self.queued_calls.remove(&request_id) {
                        self.pending_events.push_back(SrtIngressEvent::CallFailed {
                            token,
                            reason: "the call was cancelled".to_string(),
                        });
                    }
                }
                // The handshake ran out of time: the call ends like any session.
                PoolEvent::Expired { caller_id, .. } => {
                    let session = SessionId::Caller(caller_id);
                    if let Some(peer) = self.addrs.get(&session).copied() {
                        self.pending_events
                            .push_back(SrtIngressEvent::Disconnected {
                                peer,
                                logical_peer: self.session(session),
                                reason: "the SRT listener did not answer in time".to_string(),
                            });
                    }
                    self.retire(session);
                }
                PoolEvent::Queued { .. } => {}
            }
        }
        self.pool_scratch = events;
        self.pool_scratch.clear();
    }
}
