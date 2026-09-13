use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use restream_dataplane::ReadyQueue;

fn ready_queue(c: &mut Criterion) {
    let mut group = c.benchmark_group("dataplane/ready_queue");
    for leaves in [1_usize, 4, 32, 256, 1024] {
        group.bench_with_input(
            BenchmarkId::from_parameter(leaves),
            &leaves,
            |b, &leaves| {
                b.iter(|| {
                    let mut queue = ReadyQueue::new(leaves, leaves).unwrap();
                    for slot in 0..leaves as u32 {
                        assert!(queue.enqueue(slot));
                    }
                    while queue.pop().is_some() {}
                });
            },
        );
    }
    group.finish();
}

criterion_group!(benches, ready_queue);
criterion_main!(benches);
