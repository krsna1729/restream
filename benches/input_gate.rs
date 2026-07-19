use criterion::{Criterion, criterion_group, criterion_main};
use restream::media::input_gate::{InputPacketBoundary, InputPacketGate};
use std::hint::black_box;

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

criterion_group!(
    benches,
    bench_active_packet,
    bench_standby_packet,
    bench_promotion_keyframe
);
criterion_main!(benches);
