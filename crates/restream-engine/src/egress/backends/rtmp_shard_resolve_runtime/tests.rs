use super::*;
use crate::egress::backends::compio_tcp::CompioTcpPoller;
use crate::egress::backends::rtmp_shard::EmptyRtmpPublishStartupSource;
use crate::egress::command::ShardId;
use crate::egress::command::{FeedId, OutputId};
use crate::egress::journal::FeedEpoch;
use crate::egress::leaf::EgressProgressSink;
use crate::egress::metrics::ShardMetrics;
use crate::egress::policy::{LeafPolicy, WorkBudgetConfig};
use crate::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
use crate::resolver::fake::FakeDns;
use rml_rtmp::handshake::{
    Handshake as PeerHandshake, HandshakeProcessResult as PeerResult, PeerType,
};
use rml_rtmp::sessions::{
    ServerSession, ServerSessionConfig, ServerSessionEvent, ServerSessionResult,
};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream as StdTcpStream};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn budget() -> WorkBudgetConfig {
    WorkBudgetConfig::new(8, 4096, Duration::from_millis(50))
}

fn feed() -> RingFeed {
    RingFeed::new(
        Arc::new(restream_sync::RingBuffer::new(4)),
        Arc::new(FeedEpoch::new()),
    )
}

fn output_spec(id: &str, url: &str, generation: u64) -> OutputSpec {
    OutputSpec {
        id: OutputId::new(id),
        generation,
        feed: FeedId::new("feed"),
        protocol: ProtocolSpec::Rtmp {
            url: url.to_string(),
            tls: false,
        },
        policy: LeafPolicy::default(),
        progress: EgressProgressSink::default(),
    }
}

fn backend_on(
    resolver: Arc<Resolver>,
) -> ResolvingRtmpShardBackendWithPoller<CompioTcpPoller, EmptyRtmpPublishStartupSource> {
    resolving_rtmp_shard_backend(
        CompioTcpPoller::new(4).unwrap(),
        feed(),
        budget(),
        4096,
        crate::egress::tls::rustls_client_config(),
        EmptyRtmpPublishStartupSource,
        Duration::from_secs(3),
        8,
        resolver,
    )
}

/// An output whose `terminated_unexpectedly` flag the test reads, resolving
/// within `resolve_timeout`.
fn flagged_spec(
    id: &str,
    url: &str,
    generation: u64,
    resolve_timeout: Duration,
) -> (OutputSpec, Arc<std::sync::atomic::AtomicBool>) {
    let mut spec = output_spec(id, url, generation);
    spec.policy.resolve_timeout = resolve_timeout;
    let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    spec.progress.terminated_unexpectedly = Some(Arc::clone(&terminated));
    (spec, terminated)
}

fn tick_until(
    backend: &mut ResolvingRtmpShardBackendWithPoller<
        CompioTcpPoller,
        EmptyRtmpPublishStartupSource,
    >,
    within: Duration,
    mut done: impl FnMut(&RtmpShardBackend<CompioTcpPoller, EmptyRtmpPublishStartupSource>) -> bool,
) -> bool {
    let deadline = std::time::Instant::now() + within;
    while std::time::Instant::now() < deadline {
        backend.on_media_tick();
        if done(&backend.backend) {
            return true;
        }
        thread::sleep(Duration::from_millis(2));
    }
    false
}

/// One black-holed destination on a shard leaves a sibling output on that
/// shard connecting at once. The hung output fails at its resolve deadline
/// (its egress task retries it), and its retry joins the lookup still
/// running instead of stacking another.
#[test]
fn a_black_holed_destination_never_delays_a_sibling_on_the_shard() {
    let dns = FakeDns::new();
    let mut backend = backend_on(dns.resolver.clone());
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    let (accepted_tx, accepted) = std::sync::mpsc::channel();
    let server = thread::spawn(move || {
        let accepted_stream = listener.accept().map(|(stream, _)| stream);
        let _ = accepted_tx.send(());
        accepted_stream
    });

    let timeout = Duration::from_millis(300);
    let (hung, hung_failed) = flagged_spec("hung", "rtmp://hung.test:1935/live/key", 1, timeout);
    let hung_id = hung.id.clone();
    let hung_added = std::time::Instant::now();
    backend.on_command(EgressCommand::Add(hung));
    dns.wait_for_calls(1);

    let started = std::time::Instant::now();
    let (sibling, _) = flagged_spec(
        "sibling",
        &format!("rtmp://sibling.test:{port}/live/key"),
        1,
        Duration::from_secs(5),
    );
    let sibling_id = sibling.id.clone();
    backend.on_command(EgressCommand::Add(sibling));
    assert!(
        tick_until(&mut backend, Duration::from_secs(1), |inner| {
            !inner.has_pending_connect(&sibling_id)
        }),
        "the sibling resolved while the hung lookup runs"
    );
    accepted
        .recv_timeout(Duration::from_secs(1))
        .expect("the sibling connected");
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "{:?}",
        started.elapsed()
    );
    assert!(backend.backend.has_pending_connect(&hung_id));

    assert!(
        tick_until(&mut backend, Duration::from_secs(2), |inner| {
            !inner.has_pending_connect(&hung_id)
        }),
        "the hung output fails at its deadline"
    );
    assert!(hung_failed.load(std::sync::atomic::Ordering::Relaxed));
    assert!(hung_added.elapsed() >= timeout);

    let (retry, retry_failed) = flagged_spec("hung", "rtmp://hung.test:1935/live/key", 2, timeout);
    backend.on_command(EgressCommand::Add(retry));
    assert!(tick_until(&mut backend, Duration::from_secs(2), |inner| {
        !inner.has_pending_connect(&hung_id)
    }));
    assert!(retry_failed.load(std::sync::atomic::Ordering::Relaxed));
    assert_eq!(dns.calls(), 2, "the retry joined the hung lookup");
    drop(server);
}

/// A full resolver queue answers at once: the output fails alone (and is
/// retried by its egress task), never left pending.
#[test]
fn a_full_resolver_queue_fails_the_output_at_once() {
    let dns = FakeDns::with_pool(1, 1);
    let mut backend = backend_on(dns.resolver.clone());
    backend.on_command(EgressCommand::Add(output_spec(
        "hung",
        "rtmp://hung.test/live/key",
        1,
    )));
    dns.wait_for_calls(1);
    backend.on_command(EgressCommand::Add(output_spec(
        "queued",
        "rtmp://queued.test/live/key",
        1,
    )));
    let (spec, terminated) = flagged_spec(
        "refused",
        "rtmp://refused.test/live/key",
        1,
        Duration::from_secs(5),
    );
    let output_id = spec.id.clone();
    backend.on_command(EgressCommand::Add(spec));
    backend.on_media_tick();

    assert!(terminated.load(std::sync::atomic::Ordering::Relaxed));
    assert!(!backend.backend.has_pending_connect(&output_id));
    assert_eq!(dns.resolver.stats().overflows, 1);
}

/// The shard owns no resolver thread: tearing it down with a lookup hung
/// does not wait on DNS.
#[test]
fn shard_teardown_never_waits_on_a_hung_lookup() {
    let dns = FakeDns::new();
    let resolver = dns.resolver.clone();
    let config =
        crate::egress::shard::EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(5))
            .unwrap();
    let handle =
        crate::egress::shard::EgressShardHandle::spawn_with(ShardId::new(0), config, move || {
            backend_on(resolver)
        });
    handle
        .try_send(EgressCommand::Add(output_spec(
            "hung",
            "rtmp://hung.test/live/key",
            1,
        )))
        .unwrap();
    dns.wait_for_calls(1);

    let started = std::time::Instant::now();
    assert!(!handle.shutdown_and_join().panicked);
    assert!(
        started.elapsed() < Duration::from_secs(1),
        "teardown waited {:?} on DNS",
        started.elapsed()
    );
}

fn run_accepting_server_peer(mut stream: StdTcpStream, done_tx: std::sync::mpsc::Sender<()>) {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();

    let mut handshake = PeerHandshake::new(PeerType::Server);
    let mut buf = [0u8; 4096];
    let remaining;
    loop {
        let n = stream.read(&mut buf).expect("server handshake read");
        assert_ne!(n, 0);
        match handshake.process_bytes(&buf[..n]).unwrap() {
            PeerResult::InProgress { response_bytes } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes).unwrap();
                }
            }
            PeerResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                if !response_bytes.is_empty() {
                    stream.write_all(&response_bytes).unwrap();
                }
                remaining = remaining_bytes;
                break;
            }
        }
    }

    let config = ServerSessionConfig::new();
    let (mut session, initial_results) = ServerSession::new(config).unwrap();
    for result in initial_results {
        if let ServerSessionResult::OutboundResponse(packet) = result {
            stream.write_all(&packet.bytes).unwrap();
        }
    }

    let mut pending_input = remaining;
    loop {
        if !pending_input.is_empty() {
            let input = std::mem::take(&mut pending_input);
            let results = session.handle_input(&input).unwrap();
            for result in results {
                match result {
                    ServerSessionResult::OutboundResponse(packet) => {
                        stream.write_all(&packet.bytes).unwrap();
                    }
                    ServerSessionResult::RaisedEvent(ServerSessionEvent::ConnectionRequested {
                        request_id,
                        ..
                    }) => {
                        for response in session.accept_request(request_id).unwrap() {
                            if let ServerSessionResult::OutboundResponse(packet) = response {
                                stream.write_all(&packet.bytes).unwrap();
                            }
                        }
                    }
                    ServerSessionResult::RaisedEvent(
                        ServerSessionEvent::PublishStreamRequested { request_id, .. },
                    ) => {
                        for response in session.accept_request(request_id).unwrap() {
                            if let ServerSessionResult::OutboundResponse(packet) = response {
                                stream.write_all(&packet.bytes).unwrap();
                            }
                        }
                        let _ = done_tx.send(());
                        return;
                    }
                    _ => {}
                }
            }
        }

        let n = stream.read(&mut buf).expect("server session read");
        assert_ne!(n, 0);
        pending_input = buf[..n].to_vec();
    }
}

/// End-to-end proof of the full `Add` → resolve → connect → handshake →
/// negotiate → publish path through the production Compio idle-wait path.
#[test]
fn add_command_resolves_connects_and_reaches_publish_accepted_against_a_real_peer() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let (done_tx, done_rx) = std::sync::mpsc::channel::<()>();
    let server = thread::spawn(move || {
        let (stream, _) = listener.accept().unwrap();
        run_accepting_server_peer(stream, done_tx);
    });

    let mut backend = backend_on(crate::resolver::shared());
    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        &format!("rtmp://{}/live/key", addr),
        1,
    )));
    let (_command_tx, commands) = flume::unbounded();

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "shard never reached publish acceptance via Compio idle readiness"
        );
        if done_rx.try_recv().is_ok() {
            break;
        }
        backend.on_media_tick();
        backend.on_ready();
        match backend.wait_idle(&commands, Duration::from_millis(100)) {
            crate::egress::shard::EgressShardIdleWake::BackendActivity
            | crate::egress::shard::EgressShardIdleWake::Timeout => {
                backend.on_ready();
            }
            crate::egress::shard::EgressShardIdleWake::Command(command) => {
                backend.on_command(command);
            }
            crate::egress::shard::EgressShardIdleWake::Disconnected => {
                panic!("test command channel disconnected")
            }
        }
    }

    server.join().unwrap();
}

struct ForwardingProbe;

impl EgressShardBackend for ForwardingProbe {
    fn on_command(&mut self, _command: EgressCommand) -> EgressShardCommandEffect {
        EgressShardCommandEffect::Continue
    }

    fn resync_count(&self) -> u64 {
        7
    }

    fn budget_exhaustion_count(&self) -> u64 {
        11
    }

    fn observe_metrics(&self, metrics: &mut ShardMetrics) {
        metrics.stale_completions = 13;
    }
}

#[test]
fn resolving_rtmp_backend_forwards_metrics() {
    let (completion_sender, _completion_queue) = rtmp_resolve_completion_queue();
    let backend = ResolvingRtmpShardBackend::new(
        ForwardingProbe,
        crate::resolver::shared(),
        completion_sender,
    );
    let mut metrics = ShardMetrics::new(ShardId::new(0));

    assert_eq!(backend.resync_count(), 7);
    assert_eq!(backend.budget_exhaustion_count(), 11);
    backend.observe_metrics(&mut metrics);
    assert_eq!(metrics.stale_completions, 13);
}
