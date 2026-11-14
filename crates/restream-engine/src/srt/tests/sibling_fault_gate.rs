//! Prebuilt cfg(test) live gate: production SrtServer/Owner and wire callers.
//! Fault payloads do not exist in shipping binaries.
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::{Duration, Instant};

use bytes::Bytes;
use restream_domain::domain::srt_ingest::SrtPipelineIngestConfig;

use crate::engine::MediaEngine;
use crate::ingest_auth::AuthenticatedPipeline;

use super::super::SrtIngestPolicyEntry;
use super::super::ingress_test_support::*;

struct Peers {
    stop: Arc<AtomicBool>,
    tasks: Vec<tokio::task::JoinHandle<CallerReport>>,
}

impl Peers {
    fn new() -> Self {
        Self {
            stop: Arc::new(AtomicBool::new(false)),
            tasks: Vec::new(),
        }
    }

    fn publisher(&mut self, remote: std::net::SocketAddr, key: &str) {
        let attach = direct(remote, &format!("#!::r={key},m=publish"), None);
        let stop = self.stop.clone();
        self.tasks.push(tokio::task::spawn_blocking(move || {
            let chunks = ts_chunks(512);
            let mut next = 0;
            let mut send_at = Instant::now();
            run_caller(attach, Duration::from_secs(90), move |ctx| {
                if ctx.connected() && Instant::now() >= send_at {
                    for _ in 0..4 {
                        if !ctx.send(&chunks[next % chunks.len()]) {
                            break;
                        }
                        next += 1;
                    }
                    send_at = Instant::now() + Duration::from_millis(4);
                }
                stop.load(Ordering::Acquire) || ctx.report.disconnected
            })
        }));
    }

    fn reader(&mut self, remote: std::net::SocketAddr, key: &str) -> Arc<AtomicU64> {
        let attach = direct(remote, &format!("#!::r={key},m=request"), None);
        let stop = self.stop.clone();
        let received = Arc::new(AtomicU64::new(0));
        let counter = received.clone();
        self.tasks.push(tokio::task::spawn_blocking(move || {
            run_caller(attach, Duration::from_secs(90), move |ctx| {
                counter.store(ctx.report.received.len() as u64, Ordering::Release);
                stop.load(Ordering::Acquire) || ctx.report.disconnected
            })
        }));
        received
    }

    async fn finish(mut self, allow_disconnect: bool) {
        self.stop.store(true, Ordering::Release);
        for task in self.tasks.drain(..) {
            let report = task.await.expect("wire peer task");
            assert!(
                report.connected && (allow_disconnect || !report.disconnected),
                "wire peer: {report:?}"
            );
            if !report.received.is_empty() {
                assert_eq!(report.received[0], 0x47);
            }
        }
    }
}

impl Drop for Peers {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
    }
}

const INPUTS: [(&str, &str, bool); 4] = [
    ("selected", "a", true),
    ("standby", "a", false),
    ("doomed", "a", false),
    ("other", "b", true),
];

pub(super) fn auth() -> Arc<TestAuth> {
    Arc::new(TestAuth {
        pipelines: INPUTS
            .iter()
            .map(|(key, pipeline, selected)| {
                (
                    key.to_string(),
                    AuthenticatedPipeline {
                        id: pipeline.to_string(),
                        input_id: key.to_string(),
                        selected: *selected,
                    },
                )
            })
            .collect::<HashMap<_, _>>(),
    })
}

async fn server() -> TestServer {
    let entries = INPUTS
        .iter()
        .map(|(key, pipeline, _)| {
            SrtIngestPolicyEntry::new(*pipeline, *key, SrtPipelineIngestConfig::default())
        })
        .collect();
    TestServer::start_with_auth(auth(), entries, Arc::new(MediaEngine::new())).await
}

async fn healthy(server: &TestServer) -> (Peers, Vec<Arc<AtomicU64>>) {
    let mut peers = Peers::new();
    for key in ["selected", "standby", "other"] {
        peers.publisher(server.remote, key);
    }
    server
        .wait_until(
            "selected publisher is delivering",
            Duration::from_secs(10),
            || async { server.ring_progress("a").await > 0 && server.ring_progress("b").await > 0 },
        )
        .await;
    let readers = ["selected", "selected", "other"]
        .map(|key| peers.reader(server.remote, key))
        .to_vec();
    server
        .wait_until(
            "all real consumers are delivering",
            Duration::from_secs(10),
            || async {
                readers
                    .iter()
                    .all(|bytes| bytes.load(Ordering::Acquire) > 10_000)
            },
        )
        .await;
    (peers, readers)
}

async fn watch(
    engine: &MediaEngine,
    listener: &tokio::task::JoinHandle<()>,
    readers: &[Arc<AtomicU64>],
    check_srt_owner: bool,
    fault: impl Future<Output = ()>,
) {
    let mut previous: Vec<_> = readers
        .iter()
        .map(|bytes| bytes.load(Ordering::Acquire))
        .collect();
    let mut last = vec![Instant::now(); readers.len()];
    let mut input_previous = [0; 3];
    let mut input_last = [Instant::now(); 3];
    let started = Instant::now();
    let mut finished = None;
    let mut ticks = tokio::time::interval(Duration::from_millis(100));
    tokio::pin!(fault);
    loop {
        tokio::select! {
            () = &mut fault, if finished.is_none() => finished = Some(Instant::now()),
            _ = ticks.tick() => {
                for (i, bytes) in readers.iter().enumerate() {
                    assert!(last[i].elapsed() < Duration::from_millis(1500), "consumer {i} observation gap exceeded bound");
                    let now = bytes.load(Ordering::Acquire);
                    if now > previous[i] { last[i] = Instant::now(); }
                    previous[i] = now;
                    assert!(last[i].elapsed() < Duration::from_millis(1500), "consumer {i} stalled through fault");
                }
                for (i, (key, state)) in [("selected", "active"), ("standby", "standby"), ("other", "active")].into_iter().enumerate() {
                    assert!(input_last[i].elapsed() < Duration::from_millis(1500), "input {key} observation gap exceeded bound");
                    let input = engine.ingests.sessions.read().await.get(key).cloned().expect("healthy sibling input remains registered");
                    assert_eq!(input.gate.state().as_str(), state);
                    let bytes = input.bytes_received.load(Ordering::Relaxed);
                    if bytes > input_previous[i] { input_last[i] = Instant::now(); }
                    input_previous[i] = bytes;
                    assert!(input_last[i].elapsed() < Duration::from_millis(1500), "input {key} stopped receiving through fault");
                }
                assert_eq!(engine.ingests.selected_inputs.read().await.get("a").map(String::as_str), Some("selected"));
                if check_srt_owner {
                    assert!(!engine.listener_stats_handle().ingress_owner.snapshot().faulted);
                }
                assert!(!listener.is_finished(), "independently owned victim listener stopped");
                if finished.is_some_and(|at: Instant| at.elapsed() >= Duration::from_secs(2)) { break; }
                assert!(started.elapsed() < Duration::from_secs(30), "fault never completed");
            }
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn live_sibling_fault_gate_peer_panic() {
    let server = server().await;
    let (peers, readers) = healthy(&server).await;
    let remote = server.remote;
    let before = crate::panic_boundary::contained_panics();
    watch(&server.engine, &server.task, &readers, true, async {
        let doomed = tokio::task::spawn_blocking(move || {
            let mut poison = vec![0; CHUNK];
            poison[..super::super::ingress_media::INJECTED_PANIC_PAYLOAD.len()]
                .copy_from_slice(super::super::ingress_media::INJECTED_PANIC_PAYLOAD);
            let mut publish = publish_step(vec![Bytes::from(poison); 32]);
            run_caller(
                direct(remote, "#!::r=doomed,m=publish", None),
                Duration::from_secs(6),
                move |ctx| ctx.report.disconnected || publish(ctx),
            )
        })
        .await
        .expect("fault peer");
        assert!(
            doomed.connected && doomed.disconnected,
            "panic entity retired: {doomed:?}"
        );
        assert!(crate::panic_boundary::contained_panics() > before);
        assert!(
            !server
                .engine
                .listener_stats_handle()
                .ingress_owner
                .snapshot()
                .faulted
        );
    })
    .await;
    assert!(
        !server
            .engine
            .ingests
            .sessions
            .read()
            .await
            .contains_key("doomed"),
        "the panicked peer releases its input registration, not a zombie attempt"
    );
    peers.finish(false).await;
    server.stop().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 3)]
async fn live_sibling_fault_gate_owner_fault() {
    // RTMP and SRT use independent Owners but share the production engine's
    // registries and release queue. A fatal SRT Owner cannot interrupt RTMP.
    let victims = super::sibling_rtmp::RtmpVictims::start().await;
    let mut pipelines = auth().pipelines.clone();
    pipelines.insert(
        "recovered".into(),
        AuthenticatedPipeline {
            id: "pipeline-recovered".into(),
            input_id: "input-recovered".into(),
            selected: true,
        },
    );
    let affected = TestServer::start_with_auth(
        Arc::new(TestAuth { pipelines }),
        vec![
            plain("recovered"),
            SrtIngestPolicyEntry::new("a", "doomed", SrtPipelineIngestConfig::default()),
        ],
        victims.engine.clone(),
    )
    .await;
    let remote = affected.remote;
    let mut affected_publisher = Peers::new();
    affected_publisher.publisher(remote, "recovered");
    // This SRT standby shares pipeline A with the healthy RTMP selected and
    // standby inputs/readers: Owner teardown must not cancel its siblings.
    affected_publisher.publisher(remote, "doomed");
    affected
        .wait_until(
            "affected pipeline delivers before fault",
            Duration::from_secs(10),
            || async {
                affected.ring_progress("pipeline-recovered").await > 0
                    && affected
                        .engine
                        .ingests
                        .sessions
                        .read()
                        .await
                        .get("doomed")
                        .is_some_and(|input| input.bytes_received.load(Ordering::Relaxed) > 0)
            },
        )
        .await;
    let mut recovered_listener = None;
    watch(
        &victims.engine,
        &victims.listener,
        &victims.received,
        false,
        async {
            let before = crate::panic_boundary::contained_panics();
            tokio::task::spawn_blocking(move || {
                run_caller(
                    direct(
                        remote,
                        super::super::ingress_admission::INJECTED_RESOLVER_PANIC_STREAM_ID,
                        None,
                    ),
                    Duration::from_millis(1500),
                    |_| false,
                )
            })
            .await
            .expect("Owner fault peer");
            affected
                .wait_until(
                    "affected Owner reports fault",
                    Duration::from_secs(5),
                    || async { affected.task.is_finished() },
                )
                .await;
            assert!(crate::panic_boundary::contained_panics() > before);
            assert!(
                affected
                    .engine
                    .listener_stats_handle()
                    .ingress_owner
                    .snapshot()
                    .faulted
            );
            affected_publisher.finish(true).await;
            assert!(
                !affected
                    .engine
                    .ingests
                    .sessions
                    .read()
                    .await
                    .contains_key("input-recovered"),
                "the faulted Owner's old registration is released on the shared engine"
            );
            assert!(
                !affected
                    .engine
                    .ingests
                    .sessions
                    .read()
                    .await
                    .contains_key("doomed"),
                "the affected same-pipeline standby releases without ending its RTMP siblings"
            );
            let before_restart = affected.ring_progress("pipeline-recovered").await;
            let task = tokio::spawn(affected.server.clone().run(remote.port()));
            let restarted = TestServer {
                server: affected.server.clone(),
                engine: affected.engine.clone(),
                remote,
                task,
            };
            restarted
                .wait_until(
                    "affected listener restarts",
                    Duration::from_secs(5),
                    || async {
                        restarted
                            .engine
                            .listener_stats_handle()
                            .bonding_available
                            .load(Ordering::Relaxed)
                    },
                )
                .await;
            let mut replacements = Peers::new();
            replacements.publisher(remote, "recovered");
            replacements.publisher(remote, "doomed");
            restarted
                .wait_until(
                    "replacement publishers deliver new media before reader admission",
                    Duration::from_secs(10),
                    || async {
                        restarted.ring_progress("pipeline-recovered").await > before_restart
                            && restarted
                                .engine
                                .ingests
                                .sessions
                                .read()
                                .await
                                .get("doomed")
                                .is_some_and(|input| {
                                    input.bytes_received.load(Ordering::Relaxed) > 0
                                })
                    },
                )
                .await;
            let received = replacements.reader(remote, "recovered");
            restarted
                .wait_until(
                    "a real SRT reader receives the recovered publisher",
                    Duration::from_secs(10),
                    || async { received.load(Ordering::Acquire) > 10_000 },
                )
                .await;
            replacements.finish(false).await;
            recovered_listener = Some(restarted);
        },
    )
    .await;
    victims.finish().await;
    recovered_listener
        .expect("affected listener recovered")
        .stop()
        .await;
}
