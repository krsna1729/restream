use super::super::*;
use crate::media::egress::backend::CloseReason;
use crate::media::egress::command::{FeedId, OutputId};
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::egress::leaf::LeafCommon;
use crate::media::egress::policy::{LeafLimits, WorkBudget};
use crate::media::snapshots::PublisherQuality;
use crate::media::srt::{NativeSendBacklog, SrtMessageSender, SrtSendResult};
use crate::media::ts_chunk_ring::TsChunkRing;
use bytes::Bytes;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio_util::sync::CancellationToken;

#[derive(Default)]
pub(super) struct FakeSender {
    sends: Vec<Bytes>,
    closed: u32,
    pub(super) native_backlog: Option<NativeSendBacklog>,
    pub(super) quality: Option<PublisherQuality>,
}

impl FakeSender {
    pub(super) fn sends(&self) -> &[Bytes] {
        &self.sends
    }
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

    fn native_send_backlog(&mut self) -> Option<NativeSendBacklog> {
        self.native_backlog
    }

    fn sender_quality(&self) -> Option<PublisherQuality> {
        self.quality.clone()
    }
}

pub(super) struct SharedFakeSender {
    sends: Arc<Mutex<Vec<Bytes>>>,
    closed: Arc<Mutex<u32>>,
}

impl SrtMessageSender for SharedFakeSender {
    fn send_message(&mut self, message: &Bytes) -> SrtSendResult {
        self.sends.lock().unwrap().push(message.clone());
        SrtSendResult::Accepted {
            bytes: message.len(),
        }
    }

    fn close(&mut self, _reason: CloseReason) {
        let mut closed = self.closed.lock().unwrap();
        *closed = closed.saturating_add(1);
    }
}

pub(super) struct SharedSenderProbe {
    pub(super) sender: SharedFakeSender,
    pub(super) sends: Arc<Mutex<Vec<Bytes>>>,
    pub(super) closed: Arc<Mutex<u32>>,
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

/// A `TsFeed` whose ring has already wrapped, like every feed an output is
/// added to on an established pipeline: `total_chunks` pushed through a
/// capacity-8 ring, so only the last 8 sequences are still retained. Chunk
/// payloads are `chunk-<sequence>` so a test can assert exactly where a leaf
/// started reading; every `keyframe_every`-th chunk is a keyframe (pass a
/// value larger than `total_chunks` for a feed whose only keyframe has
/// already aged out of the retention window).
pub(super) fn wrapped_feed(total_chunks: u64, keyframe_every: u64) -> TsFeed {
    let ring = TsChunkRing::new(8, CancellationToken::new());
    for sequence in 0..total_chunks {
        ring.push(
            Bytes::from(format!("chunk-{sequence}")),
            sequence % keyframe_every == 0,
        );
    }
    TsFeed::new(&ring, Arc::new(FeedEpoch::new()))
}

pub(super) fn budget() -> WorkBudget {
    WorkBudget::new(8, 1024, Duration::from_millis(1))
}

pub(super) fn shared_sender() -> SharedSenderProbe {
    let sends = Arc::new(Mutex::new(Vec::new()));
    let closed = Arc::new(Mutex::new(0));
    SharedSenderProbe {
        sender: SharedFakeSender {
            sends: Arc::clone(&sends),
            closed: Arc::clone(&closed),
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
