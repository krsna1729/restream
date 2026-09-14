use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use restream_dataplane::{FeedCursor, MediaArena, MediaRing, ReadyQueue, jain_fairness_milli};
use std::hint::black_box;
use std::time::{Duration, Instant};

fn ready_queue(c: &mut Criterion) {
    let mut group = c.benchmark_group("dataplane/ready_queue");
    for leaves in [1_usize, 4, 32, 256, 1024, 4096] {
        group.bench_with_input(
            BenchmarkId::from_parameter(leaves),
            &leaves,
            |b, &leaves| {
                let mut queue = ReadyQueue::new(leaves, leaves).unwrap();
                b.iter(|| {
                    for slot in 0..leaves as u32 {
                        assert!(queue.enqueue(slot));
                    }
                    let mut popped = 0;
                    while queue.pop().is_some() {
                        popped += 1;
                    }
                    black_box(popped);
                });
            },
        );
    }
    group.finish();
}

fn ready_queue_fixed_population(c: &mut Criterion) {
    const POPULATION: usize = 4_096;
    let mut group = c.benchmark_group("dataplane/ready_queue_fixed_population");
    for ready in [1_usize, 4, 32, 256, 1_024, POPULATION] {
        group.bench_with_input(BenchmarkId::from_parameter(ready), &ready, |b, &ready| {
            let mut queue = ReadyQueue::new(POPULATION, POPULATION).unwrap();
            b.iter(|| {
                for slot in 0..ready as u32 {
                    assert!(queue.enqueue(slot));
                }
                let mut popped = 0;
                while queue.pop().is_some() {
                    popped += 1;
                }
                black_box(popped);
            });
        });
    }
    group.finish();

    let mut group = c.benchmark_group("dataplane/ready_queue_one_ready_population");
    for population in [1_024_usize, 4_096, 16_384] {
        group.bench_with_input(
            BenchmarkId::from_parameter(population),
            &population,
            |b, &population| {
                let mut queue = ReadyQueue::new(population, population).unwrap();
                b.iter(|| {
                    assert!(queue.enqueue(0));
                    assert_eq!(queue.pop(), Some(0));
                });
            },
        );
    }
    group.finish();
}

fn media_fanout(c: &mut Criterion) {
    let mut group = c.benchmark_group("dataplane/media_fanout");
    for fanout in [1_usize, 10, 100, 500, 1_000, 2_000] {
        let mut ring = MediaRing::new(
            MediaArena::new(4_097, 188).unwrap(),
            4_096,
            4_096 * 188,
            Duration::from_secs(60),
        )
        .unwrap();
        let mut cursors = vec![FeedCursor::new(ring.epoch(), 0); fanout];
        let payload = [7_u8; 188];
        group.bench_with_input(BenchmarkId::from_parameter(fanout), &fanout, |b, _| {
            b.iter(|| {
                let reference = ring.arena_mut().acquire_copy(&payload).unwrap();
                let sequence = ring.push(reference, Instant::now(), true).unwrap();
                for cursor in &mut cursors {
                    cursor.sequence = sequence;
                    black_box(ring.read_cursor(*cursor).unwrap());
                }
            });
        });
    }
    group.finish();
}

fn service_fairness(c: &mut Criterion) {
    let mut group = c.benchmark_group("dataplane/service_fairness");
    for leaves in [1_usize, 4, 32, 256, 1_024, 4_096] {
        let visits = (0..leaves)
            .map(|index| (index % 7) as u64)
            .collect::<Vec<_>>();
        group.bench_with_input(BenchmarkId::from_parameter(leaves), &visits, |b, visits| {
            b.iter(|| black_box(jain_fairness_milli(visits)));
        });
    }
    group.finish();
}

criterion_group!(
    benches,
    ready_queue,
    ready_queue_fixed_population,
    media_fanout,
    service_fairness
);
criterion_main!(benches);
