//! RTMP publish ingest over loopback, through the production listener: one
//! publishing client stays connected while each iteration writes a batch of
//! H.264 inter frames and waits until the connection's owner has published
//! all of them to the pipeline ring. Covers the per-read path of
//! `handle_rtmp_client`: socket read, session parse, parser budget and
//! publish to the ring.

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

struct Publisher {
    socket: TcpStream,
    session: ClientSession,
    timestamp: u32,
}

impl Publisher {
    async fn connect(address: std::net::SocketAddr) -> Self {
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
                assert!(count > 0, "server closed before accepting the publish");
                pending.extend_from_slice(&buffer[..count]);
            }
            let results = session.handle_input(&std::mem::take(&mut pending)).unwrap();
            let mut outbound = Vec::new();
            let mut published = false;
            for result in results {
                match result {
                    ClientSessionResult::RaisedEvent(
                        ClientSessionEvent::ConnectionRequestAccepted,
                    ) => outbound.push(
                        session
                            .request_publishing("key".to_string(), PublishRequestType::Live)
                            .unwrap(),
                    ),
                    ClientSessionResult::RaisedEvent(
                        ClientSessionEvent::PublishRequestAccepted,
                    ) => {
                        published = true;
                    }
                    other => outbound.push(other),
                }
            }
            write_results(&mut socket, outbound).await;
            if published {
                break;
            }
        }
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
    let (mut publisher, ring, server) = runtime.block_on(async {
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
        (publisher, ring, server)
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
