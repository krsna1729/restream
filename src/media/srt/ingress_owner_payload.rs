use super::{
    AdmissionEvent, Bytes, ConnectionEvent, Instant, LogicalPeerId, Ordering, OwnerLoop,
    PENDING_EVENTS_MAX, SrtIngressEvent,
};
use srt_proto::{PayloadChunks, PayloadRetirement};

pub(super) const PAYLOAD_WORK_CAPACITY: usize = 64;
const MEDIA_CHUNKS_PER_VISIT: usize = 64;
const MEDIA_CHUNKS_PER_PEER: usize = 16;
const RECLAIM_ACTIONS_PER_VISIT: usize = 64;

/// One fixed-capacity Owner-local work list. A message, its chunk cursor and
/// its retirement occupy the same slot; cancellation never adds a second job.
pub(super) enum PayloadWork {
    Event(AdmissionEvent),
    Data {
        peer: LogicalPeerId,
        chunks: PayloadChunks,
    },
    Retired(PayloadRetirement),
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
            self.payload_work
                .extend(self.scratch.drain(..).map(PayloadWork::Event));
        } else {
            self.stats
                .ingress_owner
                .event_bridge_full_visits
                .fetch_add(1, Ordering::Relaxed);
        }
        let visits = self.payload_work.len();
        let mut media_left = MEDIA_CHUNKS_PER_VISIT;
        let mut reclaim_left = RECLAIM_ACTIONS_PER_VISIT;
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
                    if self.pending_events.len() >= PENDING_EVENTS_MAX
                        || (!self.active_payloads.is_empty()
                            && self.active_payloads.contains(&event.logical_peer))
                        || (media_left == 0
                            && matches!(event.event, ConnectionEvent::DataReceived { .. }))
                    {
                        self.payload_work.push_back(PayloadWork::Event(event));
                    } else {
                        self.apply_received_event(event, &mut media_left);
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

    fn apply_received_event(&mut self, event: AdmissionEvent, media_left: &mut usize) {
        let AdmissionEvent {
            representative_peer: peer,
            logical_peer,
            event,
        } = event;
        match event {
            ConnectionEvent::Connected => {
                let stream_id = self
                    .owner
                    .listener_peer_mut(logical_peer)
                    .and_then(|entry| entry.stream_id().map(str::to_owned))
                    .unwrap_or_default();
                self.peers += 1;
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

    fn drive_payload_chunks(
        &mut self,
        peer: LogicalPeerId,
        chunks: PayloadChunks,
        left: &mut usize,
    ) {
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

    fn accept_payload_chunk(&mut self, peer: LogicalPeerId, chunk: Bytes) {
        match self.contain_peer_media(peer, |ingress, stats| {
            ingress.on_payload(peer, chunk, stats)
        }) {
            Some(Some(probe)) => self.pending_events.push_back(SrtIngressEvent::Probe {
                logical_peer: self.session(peer),
                probe,
            }),
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

    pub(super) fn cancel_payload_work(&mut self, peer: LogicalPeerId) {
        for work in &mut self.payload_work {
            let owner = match work {
                PayloadWork::Event(event) => Some(event.logical_peer),
                PayloadWork::Data { peer, .. } => Some(*peer),
                PayloadWork::Retired(_) => None,
            };
            if owner != Some(peer) {
                continue;
            }
            let old = std::mem::replace(work, PayloadWork::Retired(PayloadRetirement::default()));
            let retired = match old {
                PayloadWork::Data { chunks, .. } => chunks.retire(),
                PayloadWork::Event(AdmissionEvent {
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
