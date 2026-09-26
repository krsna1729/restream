//! Steady-state allocation qualification for the Restream-side SRT egress
//! path, after warm-up. Control-plane connect/disconnect may allocate;
//! per-fragment and per-batch work driven by Restream must not.

use super::super::*;
use super::support::*;
use crate::media::srt::SrtSendResult;
use bytes::Bytes;
use std::time::Duration;

fn allocations_in(work: impl FnOnce()) -> usize {
    crate::test_alloc::begin();
    work();
    crate::test_alloc::end()
}

/// Restream's own per-batch scratch is reused: draining empty Owner event
/// queues and collecting Owner metrics allocate nothing.
#[test]
fn event_drain_and_metric_collection_do_not_allocate_after_warm_up() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    harness.add_resolved(srt_spec("out", 1, &url_for(sink.addr)), vec![sink.addr]);
    assert!(harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 20));
    // Let queues empty and capacities settle.
    for _ in 0..100 {
        harness.turn(Duration::from_millis(1));
    }

    let mut scratch = Vec::with_capacity(256);
    let mut metrics = ShardMetrics::default();
    // Warm the exact calls once, then measure.
    harness.backend.owners.drain_events(&mut scratch);
    harness.backend.owners.observe(&mut metrics);
    let allocations = allocations_in(|| {
        for _ in 0..200 {
            harness.backend.owners.drain_events(&mut scratch);
            harness.backend.owners.observe(&mut metrics);
        }
    });
    assert_eq!(
        allocations, 0,
        "{allocations} allocations in 200 drain+observe passes"
    );
}

/// The fragment path Restream drives (`owners.send`): the payload is a shared
/// `Bytes` and the lookup is a map probe, so nothing is allocated per
/// fragment by Restream. What remains is upstream's sender-buffer growth,
/// which is amortized (a few allocations per tens of fragments); this
/// measures the whole call and asserts it stays amortized.
#[test]
fn send_shared_marginal_allocations_are_reported() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    harness.add_resolved(srt_spec("out", 1, &url_for(sink.addr)), vec![sink.addr]);
    assert!(harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 20));
    let key = harness.backend.output_sockets[&OutputId::new("out")];
    let caller = harness.backend.leaves[key.0].as_ref().unwrap().caller();

    let now = harness.backend.owners.timestamp();
    // Warm: grow whatever the sender buffer grows.
    for _ in 0..64 {
        let _ = harness.backend.owners.send(&caller, &unit, now);
        harness.turn(Duration::from_millis(1));
    }
    let mut accepted = 0usize;
    let allocations = allocations_in(|| {
        for _ in 0..50 {
            if matches!(
                harness.backend.owners.send(&caller, &unit, now),
                SrtSendResult::Accepted { .. }
            ) {
                accepted += 1;
            }
        }
    });
    eprintln!("send_shared: {allocations} allocations for {accepted} accepted fragments");
    assert!(accepted > 0);
    assert!(
        allocations * 8 <= accepted,
        "per-fragment allocation is not amortized: {allocations} for {accepted}"
    );
}
