use super::super::*;
use crate::media::egress::backend::CloseReason;
use crate::media::egress::command::{FeedId, OutputId};
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::{LeafLimits, WorkBudget};
use crate::media::egress::scheduler::LeafKey;
use crate::media::srt::{
    SRTSOCKET, SrtEgressInterest, SrtEgressPollError, SrtMessageSender, SrtReadyLeaf, SrtSendResult,
};
use crate::media::ts_chunk_ring::TsChunkRing;
use bytes::Bytes;
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(super) struct FakeSender {
    sends: Vec<Bytes>,
    closed: u32,
}

impl SrtMessageSender for FakeSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        self.sends.push(message.clone());
        SrtSendResult::Accepted {
            bytes: message.len(),
        }
    }

    fn close(&mut self, _reason: CloseReason) {
        self.closed = self.closed.saturating_add(1);
    }
}

pub(super) struct SharedFakeSender {
    sends: Arc<Mutex<Vec<Bytes>>>,
    closed: Arc<Mutex<u32>>,
    events: Option<Arc<Mutex<Vec<&'static str>>>>,
}

impl SrtMessageSender for SharedFakeSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        self.sends.lock().unwrap().push(message.clone());
        SrtSendResult::Accepted {
            bytes: message.len(),
        }
    }

    fn close(&mut self, _reason: CloseReason) {
        if let Some(events) = &self.events {
            events.lock().unwrap().push("close");
        }
        let mut closed = self.closed.lock().unwrap();
        *closed = closed.saturating_add(1);
    }
}

pub(super) struct SharedSenderProbe {
    pub(super) sender: SharedFakeSender,
    pub(super) sends: Arc<Mutex<Vec<Bytes>>>,
    pub(super) closed: Arc<Mutex<u32>>,
}

#[derive(Clone, Default)]
pub(super) struct FakeReadinessPoller {
    inner: Arc<Mutex<FakeReadinessState>>,
}

#[derive(Default)]
struct FakeReadinessState {
    registered: Vec<(SRTSOCKET, LeafKey, u64, SrtEgressInterest)>,
    removed: Vec<SRTSOCKET>,
    ready: VecDeque<SrtReadyLeaf>,
    events: Option<Arc<Mutex<Vec<&'static str>>>>,
}

impl FakeReadinessPoller {
    pub(super) fn with_events(events: Arc<Mutex<Vec<&'static str>>>) -> Self {
        Self {
            inner: Arc::new(Mutex::new(FakeReadinessState {
                events: Some(events),
                ..FakeReadinessState::default()
            })),
        }
    }

    pub(super) fn push_ready(&self, event: SrtReadyLeaf) {
        self.inner.lock().unwrap().ready.push_back(event);
    }

    pub(super) fn registered(&self) -> Vec<(SRTSOCKET, LeafKey, u64, SrtEgressInterest)> {
        self.inner.lock().unwrap().registered.clone()
    }

    pub(super) fn removed(&self) -> Vec<SRTSOCKET> {
        self.inner.lock().unwrap().removed.clone()
    }
}

impl SrtReadinessPoller for FakeReadinessPoller {
    fn register_leaf(
        &mut self,
        socket: SRTSOCKET,
        key: LeafKey,
        generation: u64,
        interest: SrtEgressInterest,
    ) -> Result<(), SrtEgressPollError> {
        self.inner
            .lock()
            .unwrap()
            .registered
            .push((socket, key, generation, interest));
        Ok(())
    }

    fn remove(&mut self, socket: SRTSOCKET) -> Result<(), SrtEgressPollError> {
        let mut state = self.inner.lock().unwrap();
        state.removed.push(socket);
        if let Some(events) = &state.events {
            events.lock().unwrap().push("remove");
        }
        Ok(())
    }

    fn poll_leaves(
        &mut self,
        _timeout_ms: i64,
        ready: &mut Vec<SrtReadyLeaf>,
    ) -> Result<usize, SrtEgressPollError> {
        ready.clear();
        let mut state = self.inner.lock().unwrap();
        while let Some(event) = state.ready.pop_front() {
            ready.push(event);
        }
        Ok(ready.len())
    }
}

pub(super) fn leaf(generation: u64) -> SrtFabricLeaf<FakeSender> {
    SrtFabricLeaf::new(common(generation), FakeSender::default())
}

pub(super) fn feed(chunks: impl IntoIterator<Item = Bytes>) -> TsFeed {
    let ring = TsChunkRing::new(8, CancellationToken::new());
    for chunk in chunks {
        ring.push(chunk, true);
    }
    TsFeed::new(&ring, Arc::new(FeedEpoch::new()))
}

pub(super) fn budget() -> WorkBudget {
    WorkBudget::new(8, 1024, Duration::from_millis(1))
}

pub(super) fn shared_sender() -> SharedSenderProbe {
    shared_sender_with_events(None)
}

pub(super) fn shared_sender_recording(events: Arc<Mutex<Vec<&'static str>>>) -> SharedSenderProbe {
    shared_sender_with_events(Some(events))
}

fn shared_sender_with_events(events: Option<Arc<Mutex<Vec<&'static str>>>>) -> SharedSenderProbe {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(Mutex::new(0));
    SharedSenderProbe {
        sender: SharedFakeSender {
            sends: Arc::clone(&sends),
            closed: Arc::clone(&closed),
            events,
        },
        sends,
        closed,
    }
}

pub(super) fn common(generation: u64) -> LeafCommon {
    LeafCommon::new(
        OutputId::new("out-srt"),
        generation,
        FeedId::new("feed-srt"),
        LeafLimits::default(),
    )
}
