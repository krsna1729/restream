//! The MoQ egress publish path per broadcast: one 2 s GOP of H.264 (60
//! frames, 60 kB keyframe, 12 kB deltas) and AAC (94 frames) read from the
//! ring and published through `RingPublisher` (conversion, framing,
//! catalog, moq-net groups), for FLV units (RTMP ingest) and raw Annex-B /
//! ADTS units (SRT/TS ingest). Ring pushes are setup, not measured.

use criterion::{Criterion, Throughput, criterion_group, criterion_main};
use restream_engine::moq_bench_entry::{PublishBench, RingUnits};
use std::hint::black_box;
use std::time::{Duration, Instant};

fn bench_publish(c: &mut Criterion) {
    let mut group = c.benchmark_group("moq_publish");
    for (name, units) in [("flv", RingUnits::Flv), ("annexb", RingUnits::AnnexB)] {
        bench_units(&mut group, name, units);
    }
    group.finish();
}

fn bench_units(
    group: &mut criterion::BenchmarkGroup<'_, criterion::measurement::WallTime>,
    name: &str,
    units: RingUnits,
) {
    let mut bench = PublishBench::new(units);
    bench.push_gop();
    let bytes = bench.pump();
    assert!(bytes > 0, "{name}: nothing published");
    group.throughput(Throughput::Bytes(bytes as u64));
    group.bench_function(format!("gop_2s_h264_aac/{name}"), |b| {
        b.iter_custom(|iters| {
            let mut elapsed = Duration::ZERO;
            for _ in 0..iters {
                bench.push_gop();
                let start = Instant::now();
                black_box(bench.pump());
                elapsed += start.elapsed();
            }
            elapsed
        })
    });
}

criterion_group!(benches, bench_publish);
criterion_main!(benches);
