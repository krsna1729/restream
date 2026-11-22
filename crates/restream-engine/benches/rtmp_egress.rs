//! RTMP egress through the production fabric (`retain_rtmp_fabric_runtime` /
//! `dispatch_rtmp_fabric_command`): one RTMP output publishes a source ring
//! to this engine's own RTMP listener over loopback. Each iteration pushes a
//! batch of H.264 inter frames into the source ring and waits until the
//! listener has published all of them to the sink pipeline's ring. Covers
//! the egress leaf's steady-state publish path (`MediaPublisher::advance`:
//! feed reads, encoding, vectored writes) plus the shared ingest cost.

use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_domain::domain::ingest_security::IngestSecurityConfig;
use restream_engine::egress::backends::rtmp::RtmpPublishStartup;
use restream_engine::egress::journal::{FeedEpoch, RingFeed};
use restream_engine::egress::{
    EgressCommand, FeedId, LeafPolicy, OutputId, OutputSpec, ProtocolSpec,
};
use restream_engine::engine::MediaEngine;
use restream_engine::ingest_auth::{
    AuthenticatedPipeline, PipelineAccessAuthenticator, PipelineAccessFuture, PipelineAccessMode,
};
use restream_engine::security::IngestSecurityService;
use restream_sync::RingBuffer;
use tokio_util::sync::CancellationToken;

const SINK_PIPELINE: &str = "bench-rtmp-egress-sink";
const FRAMES_PER_BATCH: usize = 256;
const FRAME_BYTES: usize = 4096;
const FRAME_INTERVAL_MS: i64 = 20;

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
                id: SINK_PIPELINE.to_string(),
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

/// FLV video units with increasing timestamps.
struct Frames {
    timestamp_ms: i64,
}

impl Frames {
    fn next(&mut self, payload: Bytes, keyframe: bool) -> MediaPacket {
        let packet = MediaPacket {
            media_type: MediaType::Video,
            format: PayloadFormat::Flv,
            is_keyframe: keyframe,
            track_index: 0,
            pts: self.timestamp_ms,
            dts: self.timestamp_ms,
            payload,
        };
        self.timestamp_ms += FRAME_INTERVAL_MS;
        packet
    }
}

async fn wait_for_ring(ring: &RingBuffer, target: usize) {
    while ring.get_write_idx() < target {
        tokio::task::yield_now().await;
    }
}

fn bench_rtmp_egress(c: &mut Criterion) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .unwrap();
    let engine = Arc::new(MediaEngine::new());
    let shutdown = CancellationToken::new();
    let source = Arc::new(RingBuffer::new(4096));
    let feed = RingFeed::new(source.clone(), Arc::new(FeedEpoch::new()));
    let feed_id = FeedId::new("rtmp:bench-egress");
    let output_id = OutputId::new("bench-egress");
    let mut frames = Frames { timestamp_ms: 0 };
    let (sink, server) = runtime.block_on(async {
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
        let sink = engine.get_or_create_pipeline(SINK_PIPELINE).await;

        // The leaf starts at the latest keyframe.
        source.push(frames.next(avc_frame(true), true));
        engine
            .retain_rtmp_fabric_runtime(feed_id.clone(), &feed)
            .await
            .unwrap();
        assert!(
            engine
                .set_rtmp_publish_startup(
                    &feed_id,
                    output_id.clone(),
                    RtmpPublishStartup {
                        startup_video_sequence_header: Some(Bytes::from_static(
                            &AVC_SEQUENCE_HEADER
                        )),
                        ..RtmpPublishStartup::default()
                    },
                )
                .await
        );
        engine
            .dispatch_rtmp_fabric_command(
                &feed_id,
                EgressCommand::Add(OutputSpec {
                    id: output_id.clone(),
                    generation: 1,
                    feed: feed_id.clone(),
                    protocol: ProtocolSpec::Rtmp {
                        url: format!("rtmp://127.0.0.1:{}/live/key", address.port()),
                        tls: false,
                    },
                    policy: LeafPolicy::default(),
                    progress: Default::default(),
                }),
            )
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), wait_for_ring(&sink, 2))
            .await
            .expect("the sequence header and keyframe reach the sink");
        (sink, server)
    });
    let frame = avc_frame(false);

    let mut group = c.benchmark_group("rtmp_egress");
    group.throughput(Throughput::Elements(FRAMES_PER_BATCH as u64));
    group.bench_function("source_batch_to_sink", |b| {
        b.iter_custom(|iterations| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iterations {
                let batch: Vec<_> = (0..FRAMES_PER_BATCH)
                    .map(|_| frames.next(frame.clone(), false))
                    .collect();
                let target = sink.get_write_idx() + FRAMES_PER_BATCH;
                elapsed += runtime.block_on(async {
                    let start = Instant::now();
                    source.push_batch(batch);
                    tokio::time::timeout(Duration::from_secs(10), wait_for_ring(&sink, target))
                        .await
                        .expect("every frame of the batch reaches the sink");
                    start.elapsed()
                });
            }
            elapsed
        });
    });
    group.finish();

    runtime.block_on(async {
        let _ = engine
            .dispatch_rtmp_fabric_command(&feed_id, EgressCommand::Remove(output_id))
            .await;
        engine.release_rtmp_fabric_runtime(&feed_id).await;
    });
    engine.shutdown_listeners();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("RTMP listener stops")
            .expect("RTMP listener does not panic");
    });
    for handle in engine.drain_os_thread_handles() {
        handle.join().expect("engine thread joins");
    }
}

criterion_group!(benches, bench_rtmp_egress);
criterion_main!(benches);
