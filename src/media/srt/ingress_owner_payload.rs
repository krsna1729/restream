use super::{
    AdmissionEvent, Bytes, CallerEvent, ConnectionEvent, Instant, Ordering, OwnerLoop,
    PENDING_EVENTS_MAX, SessionId, SrtIngressEvent,
};
use srt_proto::{PayloadChunks, PayloadRetirement};
use std::net::{Ipv4Addr, SocketAddr};

pub(super) const PAYLOAD_WORK_CAPACITY: usize = 64;
const MEDIA_CHUNKS_PER_VISIT: usize = 64;
const MEDIA_CHUNKS_PER_PEER: usize = 16;
const RECLAIM_ACTIONS_PER_VISIT: usize = 64;

/// One fixed-capacity Owner-local work list. A message, its chunk cursor and
/// its retirement occupy the same slot; cancellation never adds a second job.
pub(super) enum PayloadWork {
    Event(SessionEvent),
    Data {
        peer: SessionId,
        chunks: PayloadChunks,
    },
    Retired(PayloadRetirement),
}

/// A protocol event of one session: an admitted listener peer's or an
/// outbound call's, which run the same media path.
pub(super) struct SessionEvent {
    peer: SocketAddr,
    session: SessionId,
    event: ConnectionEvent,
}

impl From<AdmissionEvent> for SessionEvent {
    fn from(event: AdmissionEvent) -> Self {
        Self {
            peer: event.representative_peer,
            session: SessionId::Peer(event.logical_peer),
            event: event.event,
        }
    }
}

/// One visit's event-order discipline. Events apply in queue order, so the
/// first event a visit cannot apply blocks every later event of that visit:
/// a terminal event can never overtake a payload the visit already accepted
/// but has not yet consumed, even when the media budget is spent before that
/// payload's own event is reached. Cursors and retirements are independent of
/// the latch and keep running.
#[derive(Debug, Default)]
struct EventOrder {
    blocked: bool,
}

impl EventOrder {
    /// Whether this event may be applied now. `needs_defer` is the event's own
    /// reason to wait (full bridge, live cursor, exhausted media budget).
    fn step(&mut self, needs_defer: bool) -> bool {
        self.blocked |= needs_defer;
        !self.blocked
    }
}

impl OwnerLoop {
    /// Protocol service continues every turn. Large application messages are
    /// consumed in packet chunks here, without flattening or a Tokio handoff.
    pub(super) fn drain_owner_events(&mut self) {
        let started = Instant::now();
        let bridge_has_room = self.pending_events.len() < PENDING_EVENTS_MAX;
        if bridge_has_room {
            let available = PAYLOAD_WORK_CAPACITY - self.payload_work.len();
            self.owner
                .poll_listener_events_bounded(available, &mut self.scratch);
            self.payload_work.extend(
                self.scratch
                    .drain(..)
                    .map(|event| PayloadWork::Event(event.into())),
            );
            let available = PAYLOAD_WORK_CAPACITY.saturating_sub(self.payload_work.len());
            self.owner
                .poll_caller_events_bounded(available, &mut self.caller_scratch);
            for CallerEvent { id, event } in self.caller_scratch.drain(..) {
                let session = SessionId::Caller(id);
                let peer = self
                    .addrs
                    .get(&session)
                    .copied()
                    .unwrap_or(SocketAddr::from((Ipv4Addr::UNSPECIFIED, 0)));
                self.payload_work
                    .push_back(PayloadWork::Event(SessionEvent {
                        peer,
                        session,
                        event,
                    }));
            }
        } else {
            self.stats
                .ingress_owner
                .event_bridge_full_visits
                .fetch_add(1, Ordering::Relaxed);
        }
        let visits = self.payload_work.len();
        let mut media_left = MEDIA_CHUNKS_PER_VISIT;
        let mut reclaim_left = RECLAIM_ACTIONS_PER_VISIT;
        let mut order = EventOrder::default();
        for _ in 0..visits {
            let Some(work) = self.payload_work.pop_front() else {
                break;
            };
            match work {
                PayloadWork::Retired(mut retired) => {
                    reclaim_left -= retired.reclaim(reclaim_left);
                    if !retired.is_complete() {
                        self.payload_work.push_back(PayloadWork::Retired(retired));
                    }
                }
                PayloadWork::Data { peer, chunks } => {
                    if self.pending_events.len() < PENDING_EVENTS_MAX {
                        self.drive_payload_chunks(peer, chunks, &mut media_left);
                    } else {
                        self.payload_work
                            .push_back(PayloadWork::Data { peer, chunks });
                    }
                }
                PayloadWork::Event(event) => {
                    let needs_defer = self.pending_events.len() >= PENDING_EVENTS_MAX
                        || (!self.active_payloads.is_empty()
                            && self.active_payloads.contains(&event.session))
                        || (media_left == 0
                            && matches!(event.event, ConnectionEvent::DataReceived { .. }));
                    if order.step(needs_defer) {
                        self.apply_received_event(event, &mut media_left);
                    } else {
                        self.payload_work.push_back(PayloadWork::Event(event));
                    }
                }
            }
        }
        let elapsed_us = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
        let stats = &self.stats.ingress_owner;
        stats.media_work_us.fetch_add(elapsed_us, Ordering::Relaxed);
        stats
            .media_pass_max_us
            .fetch_max(elapsed_us, Ordering::Relaxed);
        if elapsed_us > 5_000 {
            stats.media_slow_passes_5ms.fetch_add(1, Ordering::Relaxed);
            if elapsed_us > 20_000 {
                stats.media_slow_passes_20ms.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    fn apply_received_event(&mut self, event: SessionEvent, media_left: &mut usize) {
        let SessionEvent {
            peer,
            session: logical_peer,
            event,
        } = event;
        match event {
            ConnectionEvent::Connected => {
                // A call's identity is its input, which the Tokio side holds.
                let mut stream_id = String::new();
                if let SessionId::Peer(id) = logical_peer {
                    stream_id = self
                        .owner
                        .listener_peer_mut(id)
                        .and_then(|entry| entry.stream_id().map(str::to_owned))
                        .unwrap_or_default();
                    self.peers += 1;
                }
                self.live.insert(logical_peer);
                self.addrs.insert(logical_peer, peer);
                self.pending_events.push_back(SrtIngressEvent::Connected {
                    peer,
                    logical_peer: self.session(logical_peer),
                    stream_id,
                });
            }
            ConnectionEvent::DataReceived { payload, .. } => {
                let mut chunks = payload.into_chunks();
                if chunks.remaining_chunks() == 1 {
                    let chunk = chunks.next().expect("one received packet chunk");
                    self.queue_payload_retirement(chunks.retire());
                    *media_left -= 1;
                    self.accept_payload_chunk(logical_peer, chunk);
                } else {
                    self.active_payloads.insert(logical_peer);
                    self.drive_payload_chunks(logical_peer, chunks, media_left);
                }
            }
            ConnectionEvent::Disconnected { reason } => {
                self.pending_events
                    .push_back(SrtIngressEvent::Disconnected {
                        peer,
                        logical_peer: self.session(logical_peer),
                        reason: reason.to_string(),
                    });
                self.retire(logical_peer);
            }
            ConnectionEvent::StateChanged(_)
            | ConnectionEvent::Error(_)
            | ConnectionEvent::KeyRefreshNeeded { .. } => {}
        }
    }

    fn drive_payload_chunks(&mut self, peer: SessionId, chunks: PayloadChunks, left: &mut usize) {
        // Restore the cursor before calling media containment: a panic-triggered
        // retirement must find and cancel its unconsumed tail in this work list.
        self.payload_work
            .push_back(PayloadWork::Data { peer, chunks });
        for _ in 0..(*left).min(MEDIA_CHUNKS_PER_PEER) {
            if self.pending_events.len() >= PENDING_EVENTS_MAX {
                break;
            }
            let (chunk, finished) = match self.payload_work.back_mut() {
                Some(PayloadWork::Data {
                    peer: current,
                    chunks,
                }) if *current == peer => {
                    let chunk = chunks.next();
                    (chunk, chunks.remaining_chunks() == 0)
                }
                _ => break,
            };
            if finished {
                let Some(PayloadWork::Data { chunks, .. }) = self.payload_work.pop_back() else {
                    unreachable!("current payload cursor retained");
                };
                self.active_payloads.remove(&peer);
                self.queue_payload_retirement(chunks.retire());
            }
            if let Some(chunk) = chunk {
                *left -= 1;
                self.accept_payload_chunk(peer, chunk);
            }
            if finished {
                break;
            }
        }
    }

    fn accept_payload_chunk(&mut self, peer: SessionId, chunk: Bytes) {
        match self.contain_peer_media(peer, |ingress, stats| {
            ingress.on_payload(peer, chunk, stats)
        }) {
            Some(Some(probe)) => {
                if let Some(identity) = self.media.probe_identity(peer) {
                    self.pending_events.push_back(SrtIngressEvent::Probe {
                        logical_peer: self.session(peer),
                        identity,
                        probe: Box::new(probe),
                    });
                }
            }
            Some(None) => {}
            None => self.cancel_payload_work(peer),
        }
    }

    fn queue_payload_retirement(&mut self, retired: PayloadRetirement) {
        if !retired.is_complete() {
            self.payload_work.push_back(PayloadWork::Retired(retired));
        }
    }

    pub(super) fn payload_work_runnable(&self) -> bool {
        !self.payload_work.is_empty()
            && (self.pending_events.len() < PENDING_EVENTS_MAX
                || self
                    .payload_work
                    .iter()
                    .any(|work| matches!(work, PayloadWork::Retired(_))))
    }

    pub(super) fn cancel_payload_work(&mut self, peer: SessionId) {
        for work in &mut self.payload_work {
            let owner = match work {
                PayloadWork::Event(event) => Some(event.session),
                PayloadWork::Data { peer, .. } => Some(*peer),
                PayloadWork::Retired(_) => None,
            };
            if owner != Some(peer) {
                continue;
            }
            let old = std::mem::replace(work, PayloadWork::Retired(PayloadRetirement::default()));
            let retired = match old {
                PayloadWork::Data { chunks, .. } => chunks.retire(),
                PayloadWork::Event(SessionEvent {
                    event: ConnectionEvent::DataReceived { payload, .. },
                    ..
                }) => payload.retire(),
                _ => PayloadRetirement::default(),
            };
            *work = PayloadWork::Retired(retired);
        }
        self.active_payloads.remove(&peer);
    }
}

#[cfg(test)]
mod tests {
    use super::EventOrder;

    /// A payload the visit cannot afford is requeued, and its peer's terminal
    /// event follows it in the same visit. Applying that event immediately
    /// retires the accepted payload and drops accepted media.
    #[test]
    fn a_deferred_event_defers_every_later_event_of_its_visit() {
        let mut order = EventOrder::default();
        assert!(!order.step(true), "the unaffordable payload waits");
        assert!(
            !order.step(false),
            "a terminal event must not overtake the payload queued ahead of it"
        );
    }

    #[test]
    fn an_unblocked_visit_applies_every_event() {
        let mut order = EventOrder::default();
        assert!(order.step(false), "nothing queued requires a wait");
        assert!(order.step(false), "the latch never engages on its own");
    }
}
