//! Fixtures for the SRT egress backend tests. Tests drive the REAL backend,
//! Compio runtime and Owners against a REAL peer: a Compio `Owner` listener
//! on its own thread. There are no transport fakes.

use super::super::*;
use crate::media::egress::command::{FeedId, OutputId, OutputSpec, ProtocolSpec};
use crate::media::egress::journal::{FeedEpoch, TsFeed};
use crate::media::egress::policy::{LeafPolicy, WorkBudget};
use crate::media::ts_chunk_ring::TsChunkRing;
use bytes::Bytes;
use srt_transport::compio::{Owner, OwnerServiceBudget};
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tokio_util::sync::CancellationToken;

pub(super) fn budget() -> WorkBudget {
    WorkBudget::new(8, 64 * 1024, Duration::from_millis(5))
}

/// A ring plus a feed reading it, so a test can keep publishing.
pub(super) struct TestFeed {
    pub(super) ring: TsChunkRing,
    pub(super) feed: TsFeed,
}

impl TestFeed {
    pub(super) fn new() -> Self {
        let ring = TsChunkRing::new(256, CancellationToken::new());
        let feed = TsFeed::new(&ring, Arc::new(FeedEpoch::new()));
        Self { ring, feed }
    }

    pub(super) fn reader(&self) -> TsFeed {
        self.feed.clone_reader()
    }

    pub(super) fn publish(&self, payload: Bytes) {
        self.ring.push(payload, true);
    }
}

pub(super) fn settings() -> SrtOwnerSettings {
    SrtOwnerSettings::new(16, Duration::from_secs(10))
}

/// A backend plus the resolver-completion sender the tests drive by hand.
pub(super) struct Harness {
    pub(super) backend: SrtShardBackend,
    pub(super) resolved: SyncSender<SrtResolvedConnect>,
    pub(super) feed: TestFeed,
    /// The shard command channel the backend parks on in `wait_idle`.
    _commands: flume::Sender<EgressCommand>,
    commands_rx: flume::Receiver<EgressCommand>,
}

impl Harness {
    pub(super) fn new() -> Self {
        Self::with_settings(settings())
    }

    pub(super) fn with_settings(settings: SrtOwnerSettings) -> Self {
        let feed = TestFeed::new();
        let (resolved, queue) = srt_resolve_completion_queue(256);
        let backend = SrtShardBackend::with_runtime_components(
            feed.reader(),
            budget(),
            queue,
            SrtOwners::new(settings).expect("shard runtime builds on this host"),
        )
        .with_leaf_capacity(64)
        .with_drain_timeout(Duration::from_millis(400));
        let (commands, commands_rx) = flume::bounded(64);
        Self {
            backend,
            resolved,
            feed,
            _commands: commands,
            commands_rx,
        }
    }

    /// Add an output and complete its (already numeric) DNS resolution.
    pub(super) fn add_resolved(&mut self, spec: OutputSpec, peers: Vec<SocketAddr>) {
        let (output_id, generation) = (spec.id.clone(), spec.generation);
        self.backend.on_command(EgressCommand::Add(spec));
        self.resolved
            .send(SrtResolvedConnect {
                output_id,
                generation,
                peer_addrs: peers,
            })
            .expect("completion queue open");
        self.backend.on_media_tick();
    }

    /// Publish a unit and deliver the coalesced feed wake the shard would get.
    pub(super) fn publish(&mut self, payload: Bytes) {
        self.feed.publish(payload);
        self.backend.on_command(EgressCommand::FeedWake);
    }

    /// Publish `unit` at a modest fixed rate (about 1 per millisecond) while
    /// turning the loop, until `done` or the timeout. A test that floods the
    /// feed as fast as the CPU spins measures its own starvation, not the
    /// transport.
    pub(super) fn feed_until(
        &mut self,
        unit: &Bytes,
        timeout: Duration,
        mut done: impl FnMut(&mut SrtShardBackend) -> bool,
    ) -> bool {
        let start = Instant::now();
        let mut last_publish = Instant::now() - Duration::from_secs(1);
        while start.elapsed() < timeout {
            if last_publish.elapsed() >= Duration::from_millis(1) {
                self.publish(unit.clone());
                last_publish = Instant::now();
            }
            self.turn(Duration::from_millis(1));
            if done(&mut self.backend) {
                return true;
            }
        }
        false
    }

    /// One turn of the shard loop's shape: media tick, ready work, then park
    /// in `wait_idle` (which is what turns the Compio runtime so TX lanes and
    /// receives make progress). A command that wakes the park is delivered
    /// like the shard would.
    pub(super) fn turn(&mut self, park: Duration) -> EgressShardIdleWake {
        self.backend.on_media_tick();
        self.backend.on_ready();
        let wake = self.backend.wait_idle(&self.commands_rx, park);
        match wake {
            EgressShardIdleWake::Command(command) => {
                self.backend.on_command(command);
                EgressShardIdleWake::BackendActivity
            }
            other => other,
        }
    }

    /// Run turns until `done` or the timeout (test-side polling only; the
    /// backend itself never sleeps or spins).
    pub(super) fn pump(
        &mut self,
        timeout: Duration,
        mut done: impl FnMut(&mut SrtShardBackend) -> bool,
    ) -> bool {
        let start = Instant::now();
        while start.elapsed() < timeout {
            self.turn(Duration::from_millis(2));
            if done(&mut self.backend) {
                return true;
            }
        }
        false
    }
}

pub(super) fn srt_spec(id: &str, generation: u64, url: &str) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation,
        feed: FeedId::new("feed-srt"),
        protocol: ProtocolSpec::Srt {
            url: url.to_string(),
        },
        policy: LeafPolicy::default(),
        progress: Default::default(),
    }
}

/// `OutputSpec` whose progress sink exposes the unexpected-termination flag.
pub(super) fn srt_spec_with_flag(
    id: &str,
    generation: u64,
    url: &str,
) -> (OutputSpec, Arc<AtomicBool>) {
    let flag = Arc::new(AtomicBool::new(false));
    let mut spec = srt_spec(id, generation, url);
    spec.progress.terminated_unexpectedly = Some(Arc::clone(&flag));
    (spec, flag)
}

#[derive(Default)]
pub(super) struct SinkStats {
    pub(super) payloads: AtomicU64,
    pub(super) payload_bytes: AtomicU64,
    /// Distinct remote (Restream-side) socket addresses the sink has seen:
    /// one per Restream caller socket.
    pub(super) sources: Mutex<HashSet<SocketAddr>>,
}

/// A real SRT listener: a Compio `Owner` on its own thread accepting direct
/// and bonded callers and counting delivered payload. `stall()` stops it
/// servicing its socket, which is what a slow or wedged peer looks like.
pub(super) struct SinkPeer {
    pub(super) addr: SocketAddr,
    pub(super) stats: Arc<SinkStats>,
    stall: Arc<AtomicBool>,
    stop: Arc<AtomicBool>,
    join: Option<JoinHandle<()>>,
}

impl SinkPeer {
    pub(super) fn start(bind: &str) -> Option<Self> {
        let stats = Arc::new(SinkStats::default());
        let stall = Arc::new(AtomicBool::new(false));
        let stop = Arc::new(AtomicBool::new(false));
        let (addr_tx, addr_rx) = std::sync::mpsc::channel();
        let (thread_stats, thread_stall, thread_stop) =
            (Arc::clone(&stats), Arc::clone(&stall), Arc::clone(&stop));
        let bind: SocketAddr = bind.parse().expect("bind address");
        let join = std::thread::Builder::new()
            .name("srt-test-sink".into())
            .spawn(move || {
                let runtime = compio::runtime::Runtime::new().expect("sink runtime");
                runtime.block_on(async {
                    let config = srt_transport::ListenerConfig::builder(bind)
                        .topology(srt_transport::ListenerTopology::PerPort)
                        .bonded_inputs(
                            srt_transport::advanced::admission::BondedInputPolicy::Accept,
                        )
                        .configure_transport(|transport| {
                            transport.promotion = srt_transport::PromotionPolicy::Never;
                        })
                        .build()
                        .expect("listener config");
                    let mut owner = Owner::new(64);
                    if owner.listen(&config).is_err() {
                        let _ = addr_tx.send(None);
                        return;
                    }
                    let _ = addr_tx.send(owner.listener_local_addr());
                    let epoch = Instant::now();
                    let mut events = Vec::new();
                    while !thread_stop.load(Ordering::Relaxed) {
                        if !thread_stall.load(Ordering::Relaxed) {
                            let now = srt_proto::Timestamp::from_micros(
                                epoch.elapsed().as_micros() as u64,
                            );
                            let _ = owner.service(now, OwnerServiceBudget::default()).await;
                            owner.poll_listener_events(&mut events);
                            for event in events.drain(..) {
                                if let srt_proto::ConnectionEvent::DataReceived {
                                    payload, ..
                                } = &event.event
                                {
                                    thread_stats.payloads.fetch_add(1, Ordering::Relaxed);
                                    thread_stats
                                        .payload_bytes
                                        .fetch_add(payload.len() as u64, Ordering::Relaxed);
                                }
                                thread_stats
                                    .sources
                                    .lock()
                                    .unwrap()
                                    .insert(event.representative_peer);
                            }
                        }
                        owner.wait_for_activity(Duration::from_millis(2)).await;
                    }
                    let _ = owner.shutdown_and_drain(Duration::from_millis(200)).await;
                });
            })
            .expect("spawn sink");
        let addr = addr_rx.recv_timeout(Duration::from_secs(5)).ok()??;
        Some(Self {
            addr,
            stats,
            stall,
            stop,
            join: Some(join),
        })
    }

    pub(super) fn v4() -> Self {
        Self::start("127.0.0.1:0").expect("IPv4 sink listens")
    }

    /// `None` on hosts without IPv6 loopback.
    pub(super) fn v6() -> Option<Self> {
        Self::start("[::1]:0")
    }

    pub(super) fn set_stalled(&self, stalled: bool) {
        self.stall.store(stalled, Ordering::Relaxed);
    }

    pub(super) fn payloads(&self) -> u64 {
        self.stats.payloads.load(Ordering::Relaxed)
    }
}

impl Drop for SinkPeer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(join) = self.join.take() {
            let _ = join.join();
        }
    }
}

pub(super) fn url_for(peer: SocketAddr) -> String {
    format!("srt://{peer}?streamid=publish%3Akey")
}
