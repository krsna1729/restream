use super::*;
use crate::media::egress::backends::rtmp_shard::EmptyRtmpPublishStartupSource;
use crate::media::egress::backends::tcp::TcpEgressPoller;
use crate::media::egress::command::{FeedId, OutputId};
use crate::media::egress::journal::FeedEpoch;
use crate::media::egress::leaf::EgressProgressSink;
use crate::media::egress::policy::LeafPolicy;
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

fn budget() -> WorkBudget {
    WorkBudget::new(8, 4096, Duration::from_millis(50))
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
fn add_command_spawns_a_resolve_worker_reaped_on_next_media_tick() {
    let mut backend = resolving_rtmp_shard_backend(
        TcpEgressPoller::new(4).unwrap(),
        feed(),
        budget(),
        4096,
        crate::media::rtmp::rustls_client_config(),
        EmptyRtmpPublishStartupSource,
    );

    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        "rtmp://127.0.0.1:1/live/key",
        1,
    )));
    assert_eq!(backend.worker_count(), 1);

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while backend.worker_count() > 0 {
        assert!(
            std::time::Instant::now() < deadline,
            "resolve worker never finished"
        );
        backend.on_media_tick();
        thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn invalid_url_spawns_no_resolve_worker() {
    let mut backend = resolving_rtmp_shard_backend(
        TcpEgressPoller::new(4).unwrap(),
        feed(),
        budget(),
        4096,
        crate::media::rtmp::rustls_client_config(),
        EmptyRtmpPublishStartupSource,
    );

    backend.on_command(EgressCommand::Add(output_spec("out-1", "not a url", 1)));
    assert_eq!(backend.worker_count(), 0);
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
/// negotiate → publish path with no manual `complete_pending_connect` call —
/// only `on_command`/`on_media_tick`/`on_ready`, the same three calls a real
/// shard event loop makes.
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
        TcpEgressPoller::new(4).unwrap(),
        feed(),
        budget(),
        4096,
        crate::media::rtmp::rustls_client_config(),
        EmptyRtmpPublishStartupSource,
    );

    backend.on_command(EgressCommand::Add(output_spec(
        "out-1",
        &format!("rtmp://{}/live/key", addr),
        1,
    )));

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        assert!(
            std::time::Instant::now() < deadline,
            "shard never reached publish acceptance via the resolving decorator"
        );
        if done_rx.try_recv().is_ok() {
            break;
        }
        backend.on_media_tick();
        backend.on_ready();
        thread::sleep(Duration::from_millis(1));
    }

    server.join().unwrap();
}
