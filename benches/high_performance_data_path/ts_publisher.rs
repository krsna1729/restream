use std::hint::black_box;

use criterion::{Criterion, Throughput};
use restream::media::engine::MediaEngine;
use restream::media::ts_publisher::{
    PUBLISH_CHUNK_BYTES, TsPublishTarget, TsPublisher, TsTimestamps,
};

/// The shared TS-to-ring publisher (file ingest, HLS push ingest) on the
/// canonical fixture in 64 KiB chunks: demux, continuity, input gate,
/// keyframe bookkeeping and ring publication. Same fixture and throughput
/// unit as `data_path/mpegts_demux_drain`, so the difference is the
/// publisher's own cost.
fn bench_ts_publisher(c: &mut Criterion) {
    let fixture_path =
        restream::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let fixture = std::fs::read(&fixture_path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture_path.display()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap_or_else(|e| panic!("runtime: {e}"));
    let engine = std::sync::Arc::new(MediaEngine::new());
    let target = runtime.block_on(async {
        let registration = engine
            .try_register_pipeline_input_attempt("bench", "bench-input", "bench-key", "hls", true)
            .await
            .unwrap_or_else(|| panic!("bench input registers"));
        let ring = engine.get_or_create_pipeline("bench").await;
        TsPublishTarget::open(engine.clone(), "bench".to_string(), registration, ring)
            .await
            .unwrap_or_else(|e| panic!("{e}"))
    });

    let mut group = c.benchmark_group("data_path/ts_publisher");
    group.sample_size(10);
    group.throughput(Throughput::Bytes(fixture.len() as u64));
    group.bench_function("publish_64k_chunks", |b| {
        b.iter(|| {
            runtime.block_on(async {
                let mut publisher = TsPublisher::new(TsTimestamps::default());
                for chunk in fixture.chunks(PUBLISH_CHUNK_BYTES) {
                    publisher.publish(&target, black_box(chunk)).await;
                }
                publisher.flush(&target);
            });
        });
    });
    group.finish();
}

pub(super) fn register(c: &mut Criterion) {
    bench_ts_publisher(c);
}
