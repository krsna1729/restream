//! Scheduler shape gate: cost follows ready/due work, never population.
//!
//! Criterion measures; this test gates. Hosted VMs are noisy, so the bounds
//! are generous shape invariants, not nanosecond pins:
//! - one ready leaf costs ~the same at N=1024 and N=16384 (no population scan);
//! - draining 32 ready leaves costs far less than draining all 4096.

use std::time::Instant;

use restream_dataplane::ReadyQueue;

fn drain_nanos(population: usize, ready: usize, iters: u64) -> u128 {
    let mut queue = ReadyQueue::new(population, population).unwrap();
    let start = Instant::now();
    for _ in 0..iters {
        for slot in 0..ready as u32 {
            assert!(queue.enqueue(slot));
        }
        while queue.pop().is_some() {}
    }
    start.elapsed().as_nanos() / u128::from(iters.max(1))
}

#[test]
fn one_ready_leaf_is_population_independent() {
    // Warm up, then compare. Generous 4x bound: a population scan would be
    // 16x (16384/1024); O(1) ready work stays flat within noise.
    let _ = drain_nanos(1_024, 1, 100);
    let small = drain_nanos(1_024, 1, 2_000);
    let large = drain_nanos(16_384, 1, 2_000);
    assert!(
        large <= small.saturating_mul(4).max(1_000),
        "T(N=16384,R=1)={large}ns vs T(N=1024,R=1)={small}ns: ready cost scales with population"
    );
}

#[test]
fn few_ready_leaves_cost_far_less_than_all_ready() {
    let _ = drain_nanos(4_096, 32, 50);
    let few = drain_nanos(4_096, 32, 500);
    let all = drain_nanos(4_096, 4_096, 50);
    // 32 vs 4096 leaves is 128x the work; even at 4x measurement slop the
    // gap must be obvious. A flat O(N) tick would show ~1x.
    assert!(
        few.saturating_mul(8) < all,
        "T(N=4096,R=32)={few}ns vs T(N=4096,R=4096)={all}ns: ready cost does not follow ready work"
    );
}
