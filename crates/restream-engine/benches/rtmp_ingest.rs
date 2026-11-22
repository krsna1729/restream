//! RTMP publish and play over loopback, through the production listener: one
//! publishing client stays connected while each iteration writes a batch of
//! H.264 inter frames.
//!
//! - `rtmp_ingest/publish_batch_to_ring` waits until the connection's owner
//!   has published the batch to the pipeline ring: socket read, session
//!   parse, parser budget and publish (`handle_rtmp_client`).
//! - `rtmp_play/publish_batch_to_player` waits until one RTMP player has
//!   received the batch: the same ingest, then the control session's
//!   `PlayNext` bursts (`run_rtmp_control_session`) and the play writes.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_engine::engine::MediaEngine;
use restream_engine::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessFuture, PipelineAccessMode,
};
use restream_engine::security::IngestSecurityService;
use restream_sync::RingBuffer;
use rml_rtmp::handshake::{Handshake, HandshakeProcessResult, PeerType};
use rml_rtmp::sessions::{
    ClientSession, ClientSessionConfig, ClientSessionEvent, ClientSessionResult, PublishRequestType,
};
use rml_rtmp::time::RtmpTimestamp;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_util::sync::CancellationToken;

const PIPELINE: &str = "bench-rtmp-ingest";
const FRAMES_PER_BATCH: usize = 256;
const FRAME_BYTES: usize = 4096;
const FRAME_INTERVAL_MS: u32 = 20;

/// libx264 AVCDecoderConfigurationRecord (1920x1080@50) as an FLV sequence
/// header tag body.
#[rustfmt::skip]
const AVC_SEQUENCE_HEADER: [u8; 44] = [
    0x17, 0x00, 0x00, 0x00, 0x00,
    0x01, 0x42, 0xc0, 0x2a, 0xff, 0xe1, 0x00, 0x18,
    0x67, 0x42, 0xc0, 0x2a, 0xda, 0x01, 0xe0, 0x08,
    0x9f, 0x97, 0x01, 0x10, 0x00, 0x00, 0x03, 0x00,
    0x10, 0x00, 0x00, 0x06, 0x48, 0xf1, 0x83, 0x2a,
    0x01, 0x00, 0x04, 0x68, 0xce, 0x0f, 0xc8,
];

struct AcceptAll;

impl PipelineAccessAuthenticator for AcceptAll {
    fn authenticate<'a>(
        &'a self,
        _mode: PipelineAccessMode,
        stream_key: &'a str,
        _client_ip: &'a str,
    ) -> PipelineAccessFuture<'a> {
        Box::pin(async move {
            Ok(AuthenticatedPipeline {
                id: PIPELINE.to_string(),
                input_id: stream_key.to_string(),
                selected: true,
            })
        })
    }
}

/// An FLV AVC NALU tag body holding one `FRAME_BYTES` NAL unit.
fn avc_frame(keyframe: bool) -> Bytes {
    let nal_type = if keyframe { 0x65 } else { 0x41 };
    let mut body = vec![if keyframe { 0x17 } else { 0x27 }, 0x01, 0, 0, 0];
    body.extend_from_slice(&u32::try_from(FRAME_BYTES).unwrap().to_be_bytes());
    body.push(nal_type);
    body.resize(body.len() + FRAME_BYTES - 1, 0x5a);
    Bytes::from(body)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Publish,
    Play,
}

/// Handshake, connect, then publish or play `key`; returns once the server
/// accepts.
async fn open_client(address: std::net::SocketAddr, role: Role) -> (TcpStream, ClientSession) {
    let mut socket = TcpStream::connect(address).await.unwrap();
    let remaining = client_handshake(&mut socket).await;
    let mut config = ClientSessionConfig::new();
    config.tc_url = Some("rtmp://127.0.0.1/live".to_string());
    let (mut session, initial) = ClientSession::new(config).unwrap();
    write_results(&mut socket, initial).await;
    let connect = session.request_connection("live".to_string()).unwrap();
    write_results(&mut socket, vec![connect]).await;

    let mut pending = remaining;
    let mut buffer = vec![0; 4096];
    loop {
        if pending.is_empty() {
            let count = socket.read(&mut buffer).await.unwrap();
            assert!(count > 0, "server closed before accepting the request");
            pending.extend_from_slice(&buffer[..count]);
        }
        let results = session.handle_input(&std::mem::take(&mut pending)).unwrap();
        let mut outbound = Vec::new();
        let mut accepted = false;
        for result in results {
            match result {
                ClientSessionResult::RaisedEvent(ClientSessionEvent::ConnectionRequestAccepted) => {
                    outbound.push(match role {
                        Role::Publish => session
                            .request_publishing("key".to_string(), PublishRequestType::Live)
                            .unwrap(),
                        Role::Play => session.request_playback("key".to_string()).unwrap(),
                    });
                }
                ClientSessionResult::RaisedEvent(
                    ClientSessionEvent::PublishRequestAccepted
                    | ClientSessionEvent::PlaybackRequestAccepted,
                ) => accepted = true,
                other => outbound.push(other),
            }
        }
        write_results(&mut socket, outbound).await;
        if accepted {
            return (socket, session);
        }
    }
}

/// An RTMP player of the pipeline.
struct Player {
    socket: TcpStream,
    session: ClientSession,
    buffer: Vec<u8>,
}

impl Player {
    async fn connect(address: std::net::SocketAddr) -> Self {
        let (socket, session) = open_client(address, Role::Play).await;
        Self {
            socket,
            session,
            buffer: vec![0; 64 * 1024],
        }
    }

    /// Reads until `count` video messages have arrived.
    async fn receive_video(&mut self, count: usize) {
        let mut received = 0;
        while received < count {
            received += self.read_video().await;
        }
    }

    /// Reads until nothing arrives for `quiet`.
    async fn drain(&mut self, quiet: Duration) {
        while tokio::time::timeout(quiet, self.read_video()).await.is_ok() {}
    }

    /// One socket read; returns the video messages it completed.
    async fn read_video(&mut self) -> usize {
        let count = self.socket.read(&mut self.buffer).await.unwrap();
        assert!(count > 0, "server closed the playback");
        let results = self.session.handle_input(&self.buffer[..count]).unwrap();
        let mut video = 0;
        let mut outbound = Vec::new();
        for result in results {
            match result {
                ClientSessionResult::RaisedEvent(ClientSessionEvent::VideoDataReceived {
                    ..
                }) => video += 1,
                other => outbound.push(other),
            }
        }
        write_results(&mut self.socket, outbound).await;
        video
    }
}

struct Publisher {
    socket: TcpStream,
    session: ClientSession,
    timestamp: u32,
}

impl Publisher {
    async fn connect(address: std::net::SocketAddr) -> Self {
        let (socket, session) = open_client(address, Role::Publish).await;
        let mut publisher = Self {
            socket,
            session,
            timestamp: 0,
        };
        let header = publisher.frame(Bytes::from_static(&AVC_SEQUENCE_HEADER));
        let keyframe = publisher.frame(avc_frame(true));
        publisher.socket.write_all(&header).await.unwrap();
        publisher.socket.write_all(&keyframe).await.unwrap();
        publisher
    }

    fn frame(&mut self, body: Bytes) -> Vec<u8> {
        let result = self
            .session
            .publish_video_data(body, RtmpTimestamp::new(self.timestamp), false)
            .unwrap();
        self.timestamp = self.timestamp.wrapping_add(FRAME_INTERVAL_MS);
        match result {
            ClientSessionResult::OutboundResponse(packet) => packet.bytes,
            other => panic!("expected a media packet, got {other:?}"),
        }
    }

    fn batch(&mut self, frame: &Bytes) -> Vec<u8> {
        let mut wire = Vec::with_capacity(FRAMES_PER_BATCH * (FRAME_BYTES + 64));
        for _ in 0..FRAMES_PER_BATCH {
            wire.extend_from_slice(&self.frame(frame.clone()));
        }
        wire
    }
}

async fn client_handshake(socket: &mut TcpStream) -> Vec<u8> {
    let mut handshake = Handshake::new(PeerType::Client);
    let p0_and_p1 = handshake.generate_outbound_p0_and_p1().unwrap();
    socket.write_all(&p0_and_p1).await.unwrap();
    let mut buffer = vec![0; 4096];
    loop {
        let count = socket.read(&mut buffer).await.unwrap();
        assert!(count > 0, "server closed during the handshake");
        match handshake.process_bytes(&buffer[..count]).unwrap() {
            HandshakeProcessResult::InProgress { response_bytes } => {
                socket.write_all(&response_bytes).await.unwrap();
            }
            HandshakeProcessResult::Completed {
                response_bytes,
                remaining_bytes,
            } => {
                socket.write_all(&response_bytes).await.unwrap();
                return remaining_bytes;
            }
        }
    }
}

async fn write_results(socket: &mut TcpStream, results: Vec<ClientSessionResult>) {
    for result in results {
        if let ClientSessionResult::OutboundResponse(packet) = result {
            socket.write_all(&packet.bytes).await.unwrap();
        }
    }
}

async fn wait_for_ring(ring: &RingBuffer, target: usize) {
    while ring.get_write_idx() < target {
        tokio::task::yield_now().await;
    }
}

fn bench_rtmp_ingest(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let engine = Arc::new(MediaEngine::new());
    let shutdown = CancellationToken::new();
    let (mut publisher, ring, server, address) = runtime.block_on(async {
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(restream_engine::rtmp::start_rtmp_server_on_with_shutdown(
            Arc::new(AcceptAll),
            Arc::new(IngestSecurityService::new(IngestSecurityConfig::default())),
            engine.clone(),
            0,
            shutdown.clone(),
            Some(started_tx),
        ));
        let address = started_rx.await.unwrap().unwrap();
        let publisher = Publisher::connect(address).await;
        let ring = engine.get_or_create_pipeline(PIPELINE).await;
        tokio::time::timeout(Duration::from_secs(5), wait_for_ring(&ring, 2))
            .await
            .expect("the sequence header and keyframe reach the ring");
        (publisher, ring, server, address)
    });
    let frame = avc_frame(false);

    let mut group = c.benchmark_group("rtmp_ingest");
    group.throughput(Throughput::Elements(FRAMES_PER_BATCH as u64));
    group.bench_function("publish_batch_to_ring", |b| {
        b.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                let wire = publisher.batch(&frame);
                let target = ring.get_write_idx() + FRAMES_PER_BATCH;
                elapsed += runtime.block_on(async {
                    let start = Instant::now();
                    publisher.socket.write_all(&wire).await.unwrap();
                    tokio::time::timeout(Duration::from_secs(10), wait_for_ring(&ring, target))
                        .await
                        .expect("every frame of the batch reaches the ring");
                    start.elapsed()
                });
            }
            elapsed
        });
    });
    group.finish();

    // One player of the same pipeline: each iteration's batch goes publisher
    // -> ring -> control session `PlayNext` bursts -> player socket.
    let mut player = runtime.block_on(async {
        let mut player = Player::connect(address).await;
        let wire = publisher.batch(&frame);
        publisher.socket.write_all(&wire).await.unwrap();
        player.drain(Duration::from_millis(300)).await;
        player
    });
    let mut group = c.benchmark_group("rtmp_play");
    group.throughput(Throughput::Elements(FRAMES_PER_BATCH as u64));
    group.bench_function("publish_batch_to_player", |b| {
        b.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                let wire = publisher.batch(&frame);
                elapsed += runtime.block_on(async {
                    let start = Instant::now();
                    let (written, ()) = tokio::join!(publisher.socket.write_all(&wire), async {
                        tokio::time::timeout(
                            Duration::from_secs(10),
                            player.receive_video(FRAMES_PER_BATCH),
                        )
                        .await
                        .expect("every frame of the batch reaches the player");
                    });
                    written.unwrap();
                    start.elapsed()
                });
            }
            elapsed
        });
    });
    group.finish();
    drop(player);

    drop(publisher);
    engine.shutdown_listeners();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("RTMP listener stops")
            .expect("RTMP listener does not panic");
    });
    for handle in engine.drain_os_thread_handles() {
        handle.join().expect("RTMP owner thread joins");
    }
}

criterion_group!(benches, bench_rtmp_ingest);
criterion_main!(benches);
