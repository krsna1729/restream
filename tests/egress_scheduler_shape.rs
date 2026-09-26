//! Egress scheduler shape gate: ready-queue cost follows ready work, never the
//! leaf population. Drives the production `ReadyQueue` exactly as the sink and
//! pipeline shards do (`try_enqueue` on a leaf's `ScheduleState`, then
//! `dequeue_next` and clearing `enqueued`).
//!
//! Hosted VMs are noisy, so the bounds are generous shape invariants, not
//! nanosecond pins: a population scan would show the full population ratio.
//! Each figure is the fastest of several trials, so a preemption or a sibling
//! test binary competing for the CPU inflates one trial, not the result.

use std::time::Instant;

use restream::media::egress::scheduler::{LeafKey, ReadyQueue, ScheduleState, try_enqueue};

fn drain_nanos(population: usize, ready: usize, iters: u64) -> u128 {
    let mut leaves: Vec<ScheduleState> = (0..population).map(|_| ScheduleState::new()).collect();
    let mut queue = ReadyQueue::with_capacity(population);
    let start = Instant::now();
    for _ in 0..iters {
        for (key, leaf) in leaves.iter_mut().enumerate().take(ready) {
            assert!(try_enqueue(leaf, &mut queue, LeafKey(key)));
        }
        while let Some(key) = queue.dequeue_next() {
            leaves[key.0].enqueued = false;
        }
    }
    start.elapsed().as_nanos() / u128::from(iters.max(1))
}

fn fastest(population: usize, ready: usize, iters: u64) -> u128 {
    (0..5)
        .map(|_| drain_nanos(population, ready, iters))
        .min()
        .expect("at least one trial")
}

#[test]
fn one_ready_leaf_is_population_independent() {
    // 16x population; a scan would be ~16x, O(ready) stays flat within noise.
    let _ = drain_nanos(1_024, 1, 100);
    let small = fastest(1_024, 1, 2_000);
    let large = fastest(16_384, 1, 2_000);
    assert!(
        large <= small.saturating_mul(4).max(1_000),
        "T(N=16384,R=1)={large}ns vs T(N=1024,R=1)={small}ns: ready cost scales with population"
    );
}

#[test]
fn few_ready_leaves_cost_far_less_than_all_ready() {
    // 32 vs 4096 ready leaves is 128x the work; even 4x slop leaves a gap.
    let _ = drain_nanos(4_096, 32, 50);
    let few = fastest(4_096, 32, 500);
    let all = fastest(4_096, 4_096, 50);
    assert!(
        few.saturating_mul(8) < all,
        "T(N=4096,R=32)={few}ns vs T(N=4096,R=4096)={all}ns: ready cost does not follow ready work"
    );
}
