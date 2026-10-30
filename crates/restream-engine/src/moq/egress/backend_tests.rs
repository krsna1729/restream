use std::cell::{Cell, RefCell};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use bytes::Bytes;
use futures_util::future::LocalBoxFuture;

use super::*;
use crate::egress::command::FeedId;
use crate::egress::journal::FeedEpoch;
use crate::egress::leaf::EgressProgressSink;
use crate::egress::policy::LeafPolicy;
use crate::moq::egress::fake_session::FakeSession;
use crate::moq::egress::leaf::LinkState;
use crate::moq::egress::startup::{MoqPublishStartup, MoqVideoStartup};
use restream_core::codec::build_avcc_sequence_header;
use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_moq::media::VideoCodec;
use restream_moq::url::MoqUrl;
use restream_sync::RingBuffer;

const PARAMETER_SETS: &[u8] = &[
    0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2,
];

#[derive(Default)]
struct FakeConnector {
    sessions: RefCell<Vec<FakeSession>>,
    paths: RefCell<Vec<String>>,
    refuse: Cell<bool>,
}

impl MoqConnector for FakeConnector {
    type Session = FakeSession;

    fn connect(
        self: Rc<Self>,
        url: MoqUrl,
        _extra_ca_pem: Option<Vec<u8>>,
    ) -> LocalBoxFuture<'static, Result<FakeSession, String>> {
        Box::pin(async move {
            self.paths.borrow_mut().push(url.path);
            if self.refuse.get() {
                return Err("relay refused".to_string());
            }
            let session = FakeSession::default();
            self.sessions.borrow_mut().push(session.clone());
            Ok(session)
        })
    }
}

struct Harness {
    backend: MoqShardBackend<FakeConnector>,
    connector: Rc<FakeConnector>,
    startups: SharedMoqStartupSource,
    ring: Arc<RingBuffer>,
    commands: flume::Receiver<EgressCommand>,
    _sender: flume::Sender<EgressCommand>,
}

struct Output {
    terminated: Arc<AtomicBool>,
    bytes_sent: Arc<AtomicU64>,
}

impl Harness {
    fn new() -> Self {
        let connector = Rc::new(FakeConnector::default());
        let startups = SharedMoqStartupSource::default();
        let ring = Arc::new(RingBuffer::new(64));
        let feed = RingFeed::new(ring.clone(), Arc::new(FeedEpoch::new()));
        let backend = MoqShardBackend::new(
            Runtime::new().unwrap(),
            connector.clone(),
            feed,
            WorkBudgetConfig::new(64, 1 << 20, Duration::from_millis(50)),
            startups.clone(),
            Duration::from_millis(100),
            8,
        );
        let (sender, commands) = flume::unbounded();
        Self {
            backend,
            connector,
            startups,
            ring,
            commands,
            _sender: sender,
        }
    }

    fn add(&mut self, id: &str, url: &str, with_startup: bool) -> Output {
        if with_startup {
            self.startups.set(
                OutputId::new(id),
                MoqPublishStartup {
                    video: Some(MoqVideoStartup {
                        codec: VideoCodec::H264,
                        config: None,
                    }),
                    broadcast: "show".to_string(),
                    ..MoqPublishStartup::default()
                },
            );
        }
        let output = Output {
            terminated: Arc::new(AtomicBool::new(false)),
            bytes_sent: Arc::new(AtomicU64::new(0)),
        };
        self.backend.on_command(EgressCommand::Add(OutputSpec {
            id: OutputId::new(id),
            generation: 1,
            feed: FeedId::new("moq:test"),
            protocol: ProtocolSpec::Moq {
                url: url.to_string(),
            },
            policy: LeafPolicy::default(),
            progress: EgressProgressSink {
                terminated_unexpectedly: Some(output.terminated.clone()),
                bytes_sent: Some(output.bytes_sent.clone()),
                ..Default::default()
            },
        }));
        output
    }

    /// Park in the backend until a leaf reports a state change.
    fn until_activity(&mut self) {
        for _ in 0..100 {
            match self
                .backend
                .wait_idle(&self.commands, Duration::from_millis(20))
            {
                EgressShardIdleWake::BackendActivity => return,
                EgressShardIdleWake::Timeout => {}
                other => panic!("unexpected wake {other:?}"),
            }
        }
        panic!("no leaf activity");
    }

    /// Visit until no leaf is ready.
    fn drain_ready(&mut self) {
        for _ in 0..64 {
            if self.backend.on_ready() == EgressShardCommandEffect::Continue {
                return;
            }
        }
        panic!("the backend kept asking for visits");
    }

    /// Let the runtime run until `done` holds.
    fn until(&mut self, mut done: impl FnMut(&Self) -> bool) {
        for _ in 0..100 {
            if done(self) {
                return;
            }
            let _ = self
                .backend
                .wait_idle(&self.commands, Duration::from_millis(20));
        }
        panic!("condition never held");
    }

    fn state(&self, id: &str) -> Option<LinkState> {
        self.backend.link_state(&OutputId::new(id))
    }
}

fn flv(keyframe: bool, payload: Vec<u8>) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: 40,
        dts: 40,
        is_keyframe: keyframe,
        format: PayloadFormat::Flv,
        payload: Bytes::from(payload),
    }
}

const URL: &str = "moqt://127.0.0.1:4443/anon?jwt=x&broadcast=ignored-here";

#[test]
fn a_leaf_connects_publishes_and_terminates_unexpectedly_when_the_relay_closes() {
    let mut harness = Harness::new();
    let output = harness.add("out-1", URL, true);
    assert_eq!(harness.state("out-1"), Some(LinkState::Connecting));

    harness.until_activity();
    harness.drain_ready();
    assert_eq!(harness.state("out-1"), Some(LinkState::Active));
    // Restream's own keys never reach the relay.
    assert_eq!(*harness.connector.paths.borrow(), ["/anon?jwt=x"]);

    harness.ring.push(flv(
        false,
        build_avcc_sequence_header(PARAMETER_SETS).unwrap().to_vec(),
    ));
    harness
        .ring
        .push(flv(true, vec![0x17, 1, 0, 0, 0, 0, 0, 0, 2, 0x65, 0x88]));
    assert_eq!(
        harness.backend.on_command(EgressCommand::FeedWake),
        EgressShardCommandEffect::ScheduleReady { count: 1 }
    );
    harness.drain_ready();
    assert!(
        output.bytes_sent.load(Ordering::Relaxed) > 0,
        "the keyframe was published"
    );
    assert!(!output.terminated.load(Ordering::Acquire));

    harness.connector.sessions.borrow()[0].close_from_peer(7);
    harness.until_activity();
    harness.drain_ready();
    assert!(
        output.terminated.load(Ordering::Acquire),
        "a relay close is unexpected"
    );
    assert_eq!(harness.state("out-1"), None, "the leaf is gone");
}

#[test]
fn an_explicit_remove_closes_the_session_with_code_zero_and_is_not_a_failure() {
    let mut harness = Harness::new();
    let output = harness.add("out-1", URL, true);
    harness.until_activity();
    harness.drain_ready();
    assert_eq!(harness.state("out-1"), Some(LinkState::Active));

    harness
        .backend
        .on_command(EgressCommand::Remove(OutputId::new("out-1")));
    assert_eq!(harness.state("out-1"), None);
    // The detached task drains (bounded) and closes.
    harness.until(|harness| harness.connector.sessions.borrow()[0].closed().is_some());
    assert_eq!(
        harness.connector.sessions.borrow()[0].closed(),
        Some((0, false))
    );
    assert!(!output.terminated.load(Ordering::Acquire));
}

#[test]
fn a_refused_connection_terminates_unexpectedly() {
    let mut harness = Harness::new();
    harness.connector.refuse.set(true);
    let output = harness.add("out-1", URL, true);
    harness.until_activity();
    harness.drain_ready();
    assert!(output.terminated.load(Ordering::Acquire));
    assert_eq!(harness.state("out-1"), None);
}

#[test]
fn an_add_without_startup_or_with_an_unusable_url_is_refused_at_once() {
    let mut harness = Harness::new();
    let missing = harness.add("out-1", URL, false);
    assert!(missing.terminated.load(Ordering::Acquire));
    let invalid = harness.add("out-2", "moqt://relay.example/live?insecure=maybe", true);
    assert!(invalid.terminated.load(Ordering::Acquire));
    assert_eq!(harness.state("out-1"), None);
    assert_eq!(harness.state("out-2"), None);
    assert!(harness.connector.paths.borrow().is_empty());
}

#[test]
fn shutdown_closes_every_leaf() {
    let mut harness = Harness::new();
    let first = harness.add("out-1", URL, true);
    let second = harness.add("out-2", URL, true);
    harness.until_activity();
    harness.drain_ready();
    harness.backend.on_command(EgressCommand::Shutdown);
    assert_eq!(harness.state("out-1"), None);
    assert_eq!(harness.state("out-2"), None);
    harness.until(|harness| {
        harness
            .connector
            .sessions
            .borrow()
            .iter()
            .all(|session| session.closed().is_some())
    });
    assert!(!first.terminated.load(Ordering::Acquire));
    assert!(!second.terminated.load(Ordering::Acquire));
}
