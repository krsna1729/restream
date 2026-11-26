//! Egress scheduler shape gate: ready-queue cost follows ready work, never the
//! leaf population. Drives the production `ReadyQueue` exactly as the shards
//! do (`push` a leaf's key, then `pop`) and counts the entries `pop` inspects.
//!
//! The counter is test support on `ReadyQueue`, so the shape is checked without
//! wall-clock ratios: a pop that scanned the population would inspect every
//! leaf, while the indexed queue inspects exactly the queued entries.
#![allow(clippy::disallowed_methods)] // test code: raw std locks are fine

use restream_engine::egress::leaf_arena::{LeafArena, LeafKey};
use restream_engine::egress::scheduler::{Push, ReadyQueue, ScheduleState};

/// Push `ready` keys into a queue sized for `population`, drain it, and return
/// the entries `pop` inspected plus the keys it returned, in order.
fn drain(population: usize, ready: usize) -> (usize, Vec<LeafKey>) {
    let mut leaves = LeafArena::with_capacity(population);
    let keys: Vec<LeafKey> = (0..population)
        .map(|_| {
            leaves
                .insert_with(|_| ScheduleState::new())
                .expect("capacity")
        })
        .collect();
    let mut queue = ReadyQueue::with_capacity(population);
    for &key in keys.iter().take(ready) {
        assert_eq!(queue.push(key), Push::Queued);
    }
    queue.reset_pop_probes();
    let mut popped = Vec::with_capacity(ready);
    while let Some(key) = queue.pop() {
        assert!(leaves.get(key).is_some());
        popped.push(key);
    }
    (queue.pop_probes(), popped)
}

#[test]
fn one_ready_leaf_is_population_independent() {
    // A population scan would inspect all 16384 leaves; the queue inspects
    // exactly the one queued entry.
    for population in [1_024, 16_384] {
        let (probes, popped) = drain(population, 1);
        assert_eq!(popped.len(), 1, "population {population}");
        assert_eq!(
            probes, 1,
            "pop inspected {probes} entries for one ready leaf at population {population}"
        );
    }
}

#[test]
fn few_ready_leaves_cost_far_less_than_all_ready() {
    // Probes equal the ready count, never the population, and FIFO holds.
    for ready in [32, 4_096] {
        let (probes, popped) = drain(4_096, ready);
        assert_eq!(popped.len(), ready);
        assert_eq!(
            probes, ready,
            "pop inspected {probes} entries for {ready} ready leaves"
        );
    }
}
