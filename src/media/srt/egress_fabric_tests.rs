use bytes::Bytes;
use tokio_util::sync::CancellationToken;

use super::srt_egress_engine::*;
use super::srt_egress_poller::*;
use super::sys::SRTSOCKET;
use crate::media::egress::backend::{EngineProgress, ProtocolEngine, Readiness};
use crate::media::egress::feed::FeedCursor;
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::egress::policy::WorkBudget;
use crate::media::egress::scheduler::LeafKey;
use crate::media::ts_chunk_ring::TsChunkRing;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::os::raw::c_int;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

#[derive(Clone)]
struct FakeSendOps {
    sends: Rc<RefCell<Vec<(SRTSOCKET, Bytes)>>>,
}

impl Default for FakeSendOps {
    fn default() -> Self {
        Self {
            sends: Rc::new(RefCell::new(Vec::new())),
        }
    }
}

impl SrtSendOps for FakeSendOps {
    fn send(&self, socket: SRTSOCKET, message: &Bytes) -> c_int {
        self.sends.borrow_mut().push((socket, message.clone()));
        message.len() as c_int
    }

    fn close(&self, _socket: SRTSOCKET) -> c_int {
        0
    }

    fn error(&self) -> (c_int, String) {
        (-1, "unused fake error".to_string())
    }
}

impl Default for SrtNativeMessageSender<FakeSendOps> {
    fn default() -> Self {
        Self::with_ops(0, FakeSendOps::default())
    }
}

#[derive(Clone)]
struct FakePollOps {
    waits: Rc<RefCell<VecDeque<FakeWaitResult>>>,
}

impl Default for FakePollOps {
    fn default() -> Self {
        Self {
            waits: Rc::new(RefCell::new(VecDeque::new())),
        }
    }
}

impl FakePollOps {
    fn push_writable(&self, socket: SRTSOCKET) {
        self.waits.borrow_mut().push_back(FakeWaitResult {
            read: Vec::new(),
            write: vec![socket],
        });
    }
}

struct FakeWaitResult {
    read: Vec<SRTSOCKET>,
    write: Vec<SRTSOCKET>,
}

impl SrtPollOps for FakePollOps {
    fn create(&self) -> c_int {
        7
    }

    fn add_usock(&self, _eid: c_int, _socket: SRTSOCKET, _events: c_int) -> c_int {
        0
    }

    fn update_usock(&self, _eid: c_int, _socket: SRTSOCKET, _events: c_int) -> c_int {
        0
    }

    fn remove_usock(&self, _eid: c_int, _socket: SRTSOCKET) -> c_int {
        0
    }

    fn wait(
        &self,
        _eid: c_int,
        readfds: &mut [SRTSOCKET],
        read_count: &mut c_int,
        writefds: &mut [SRTSOCKET],
        write_count: &mut c_int,
        _timeout_ms: i64,
    ) -> c_int {
        let result = self
            .waits
            .borrow_mut()
            .pop_front()
            .unwrap_or(FakeWaitResult {
                read: Vec::new(),
                write: Vec::new(),
            });
        let read_len = result.read.len().min(readfds.len());
        let write_len = result.write.len().min(writefds.len());
        readfds[..read_len].copy_from_slice(&result.read[..read_len]);
        writefds[..write_len].copy_from_slice(&result.write[..write_len]);
        *read_count = read_len as c_int;
        *write_count = write_len as c_int;
        (read_len + write_len) as c_int
    }

    fn release(&self, _eid: c_int) -> c_int {
        0
    }

    fn error(&self, operation: &'static str) -> SrtEgressPollError {
        SrtEgressPollError {
            operation,
            code: 1234,
            message: "fake poll error".to_string(),
        }
    }
}

struct SrtFabricHarness {
    socket: SRTSOCKET,
    generation: u64,
    poller: SrtEgressPoller<FakePollOps>,
    feed: TsFeed,
    cursor: FeedCursor,
    engine: SrtEgressEngine<SrtNativeMessageSender<FakeSendOps>>,
    sender: SrtNativeMessageSender<FakeSendOps>,
    send_ops: FakeSendOps,
}

impl SrtFabricHarness {
    fn new(socket: SRTSOCKET, generation: u64, chunks: impl IntoIterator<Item = Bytes>) -> Self {
        let poll_ops = FakePollOps::default();
        let mut poller = SrtEgressPoller::with_ops(8, poll_ops.clone()).unwrap();
        poller
            .register_leaf(socket, LeafKey(0), generation, SrtEgressInterest::WRITE)
            .unwrap();
        poll_ops.push_writable(socket);

        let ring = TsChunkRing::new(8, CancellationToken::new());
        for chunk in chunks {
            ring.push(chunk, true);
        }

        let send_ops = FakeSendOps::default();
        Self {
            socket,
            generation,
            poller,
            feed: TsFeed::new(&ring, Arc::new(FeedEpoch::new())),
            cursor: FeedCursor::new(0, 0),
            engine: SrtEgressEngine::default(),
            sender: SrtNativeMessageSender::with_ops(socket, send_ops.clone()),
            send_ops,
        }
    }

    fn drive_ready_once(&mut self) -> Option<EngineProgress> {
        let mut ready = Vec::new();
        self.poller.poll_leaves(0, &mut ready).unwrap();
        let event = ready
            .into_iter()
            .find(|event| event.socket == self.socket && event.generation == self.generation)?;
        Some(self.engine.advance(
            &mut self.sender,
            Readiness {
                readable: false,
                writable: event.writable,
            },
            &self.feed,
            &mut self.cursor,
            WorkBudget::new(8, 1024, Duration::from_millis(1)),
        ))
    }
}

#[test]
fn srt_fabric_ready_leaf_sends_shared_ts_message_through_native_sender() {
    let mut harness = SrtFabricHarness::new(42, 7, [Bytes::from_static(b"abc")]);

    let progress = harness.drive_ready_once();

    assert!(matches!(
        progress,
        Some(EngineProgress::Progress {
            bytes: 3,
            units: 1,
            ..
        })
    ));
    assert_eq!(harness.cursor, FeedCursor::new(0, 1));
    assert_eq!(
        harness.send_ops.sends.borrow().as_slice(),
        &[(42, Bytes::from_static(b"abc"))]
    );
}

#[test]
fn srt_fabric_ignores_stale_generation_readiness_before_send() {
    let mut harness = SrtFabricHarness::new(42, 8, [Bytes::from_static(b"abc")]);
    harness.generation = 9;

    let progress = harness.drive_ready_once();

    assert!(progress.is_none());
    assert_eq!(harness.cursor, FeedCursor::new(0, 0));
    assert!(harness.send_ops.sends.borrow().is_empty());
}
