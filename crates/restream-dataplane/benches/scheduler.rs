use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use restream_dataplane::{FeedCursor, MediaArena, MediaRing, ReadyQueue};
use std::hint::black_box;
use std::time::{Duration, Instant};

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

criterion_group!(benches, ready_queue, media_fanout);
criterion_main!(benches);
