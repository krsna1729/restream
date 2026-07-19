use bytes::Bytes;
use criterion::{Criterion, criterion_group, criterion_main};
use restream::media::input_gate::{InputPacketBoundary, InputPacketGate};
use restream::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat};
use restream::media::standby_gop::StandbyGopCache;
use std::hint::black_box;

fn packet(dts: i64, keyframe: bool) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        format: PayloadFormat::Raw,
        is_keyframe: keyframe,
        track_index: 0,
        pts: dts,
        dts,
        payload: Bytes::from_static(&[0; 1_024]),
    }
}

fn bench_active_packet(c: &mut Criterion) {
    let gate = InputPacketGate::active();
    c.bench_function("input_gate/active_packet", |b| {
        b.iter(|| {
            let lease = gate
                .try_enter(black_box(InputPacketBoundary::Other))
                .expect("active gate accepts packet");
            black_box(lease.activated());
        });
    });
}

fn bench_standby_packet(c: &mut Criterion) {
    let gate = InputPacketGate::standby();
    c.bench_function("input_gate/standby_packet", |b| {
        b.iter(|| {
            black_box(gate.try_enter(black_box(InputPacketBoundary::Other)));
        });
    });
}

fn bench_promotion_keyframe(c: &mut Criterion) {
    c.bench_function("input_gate/promotion_keyframe", |b| {
        b.iter_batched(
            || {
                let gate = InputPacketGate::standby();
                gate.arm_for_promotion();
                gate
            },
            |gate| {
                let lease = gate
                    .try_enter(black_box(InputPacketBoundary::VideoKeyframe))
                    .expect("armed gate accepts keyframe");
                black_box(lease.activated());
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_standby_gop_packet(c: &mut Criterion) {
    c.bench_function("input_gate/standby_gop_packet", |b| {
        b.iter_batched(
            || {
                let mut cache = StandbyGopCache::default();
                cache.push(packet(0, true));
                cache
            },
            |mut cache| {
                cache.push(black_box(packet(1, false)));
                black_box(cache.packet_count());
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

fn bench_buffered_promotion(c: &mut Criterion) {
    c.bench_function("input_gate/buffered_promotion_32_packets", |b| {
        b.iter_batched(
            || {
                let gate = InputPacketGate::standby();
                gate.arm_for_promotion();
                let mut cache = StandbyGopCache::default();
                cache.push(packet(0, true));
                for dts in 1..32 {
                    cache.push(packet(dts, false));
                }
                (gate, cache)
            },
            |(gate, mut cache)| {
                let lease = gate
                    .try_enter(black_box(InputPacketBoundary::ReplayReady))
                    .expect("armed gate accepts replay");
                black_box(cache.take_replay());
                black_box(lease.activated());
            },
            criterion::BatchSize::SmallInput,
        );
    });
}

criterion_group!(
    benches,
    bench_active_packet,
    bench_standby_packet,
    bench_promotion_keyframe,
    bench_standby_gop_packet,
    bench_buffered_promotion,
);
criterion_main!(benches);
