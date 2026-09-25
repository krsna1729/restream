use super::*;
use crate::media::egress::backends::rtmp_shard::EmptyRtmpPublishStartupSource;
use crate::media::egress::backends::tcp::TcpEgressPoller;
use crate::media::egress::command::ShardId;
use crate::media::egress::command::{FeedId, OutputId};
use crate::media::egress::journal::FeedEpoch;
use crate::media::egress::leaf::EgressProgressSink;
use crate::media::egress::metrics::ShardMetrics;
use crate::media::egress::policy::{LeafPolicy, WorkBudgetConfig};
use crate::media::egress::shard::{EgressShardBackend, EgressShardCommandEffect};
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
        Arc::new(crate::media::ring_buffer::RingBuffer::new(4)),
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

#[test]
fn rejected_resolver_request_does_not_leave_output_pending() {
    let mut backend = resolving_rtmp_shard_backend(
        TcpEgressPoller::new(4).unwrap(),
        feed(),
        budget(),
        4096,
        crate::media::rtmp::rustls_client_config(),
        EmptyRtmpPublishStartupSource,
        Duration::from_secs(3),
        8,
    );
    drop(backend.resolve_workers.request_sender.take());
    backend
        .resolve_workers
        .worker
        .take()
        .expect("resolver worker")
        .join()
        .expect("resolver worker should exit after its request sender closes");

    let terminated = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mut spec = output_spec("out-1", "rtmp://127.0.0.1:1/live/key", 1);
    let output_id = spec.id.clone();
    spec.progress.terminated_unexpectedly = Some(Arc::clone(&terminated));
    backend.on_command(EgressCommand::Add(spec));

    assert!(terminated.load(std::sync::atomic::Ordering::Relaxed));
    assert!(!backend.backend.has_pending_connect(&output_id));
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

    let mut backend = resolving_rtmp_shard_backend(
        super::super::compio_tcp::CompioTcpPoller::new(4).unwrap(),
        feed(),
        budget(),
        4096,
        crate::media::rtmp::rustls_client_config(),
        EmptyRtmpPublishStartupSource,
        Duration::from_secs(3),
        8,
    );
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
            crate::media::egress::shard::EgressShardIdleWake::BackendActivity
            | crate::media::egress::shard::EgressShardIdleWake::Timeout => {
                backend.on_ready();
            }
            crate::media::egress::shard::EgressShardIdleWake::Command(command) => {
                backend.on_command(command);
            }
            crate::media::egress::shard::EgressShardIdleWake::Disconnected => {
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
        metrics.cq_overflows = 13;
    }
}

#[test]
fn resolving_rtmp_backend_forwards_metrics() {
    let (completion_sender, _completion_queue) = rtmp_resolve_completion_queue(1);
    let backend = ResolvingRtmpShardBackend::new(
        ForwardingProbe,
        RtmpResolveWorkerSet::new(completion_sender),
    );
    let mut metrics = ShardMetrics::new(ShardId::new(0));

    assert_eq!(backend.resync_count(), 7);
    assert_eq!(backend.budget_exhaustion_count(), 11);
    backend.observe_metrics(&mut metrics);
    assert_eq!(metrics.cq_overflows, 13);
}
