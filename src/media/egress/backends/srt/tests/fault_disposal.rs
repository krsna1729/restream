//! A faulted family Owner is disposed once: its retained protocol storage is
//! released, it stops reporting runnable work, and the family stays closed.

use super::super::*;
use super::support::*;
use bytes::Bytes;
use std::time::Duration;

/// A faulted Owner never runs protocol maintenance again, so anything retired
/// into it would hold its windows and keep reporting runnable work for the
/// life of the shard. The batch that observes the fault fails the family and
/// disposes its Owner; the family stays closed for admission.
#[test]
fn a_faulted_family_is_disposed_once_and_stays_closed() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 16 * 1316]);
    harness.add_resolved(srt_spec("out", 1, &url_for(sink.addr)), vec![sink.addr]);
    assert!(
        harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 5),
        "the output delivers before the fault"
    );

    // The canonical teardown marks the engine faulted once it is quiescent:
    // exactly the state `service` reports as `newly_faulted`.
    assert!(
        harness.backend.owners.shutdown(Duration::from_secs(5)),
        "an idle family Owner tears down inside its bound"
    );
    assert!(
        harness.backend.owners.fault(AddressFamily::V4).is_some(),
        "the family Owner latched its fault"
    );

    let _ = harness.backend.begin_batch();

    assert!(
        harness.backend.owners.owner(AddressFamily::V4).is_none(),
        "the faulted Owner is dropped, not serviced for the life of the shard"
    );
    assert!(
        harness.backend.callers.is_empty(),
        "the family has no leaves"
    );
    assert!(
        !harness.backend.owners.service().work_remaining,
        "a disposed family reports no runnable work"
    );

    let request = SrtFabricEgressConnectSpec::from_url(&url_for(sink.addr), Duration::from_secs(5))
        .connect_request(&[sink.addr])
        .expect("a single peer builds a direct request");
    let refused = harness
        .backend
        .owners
        .connect(request)
        .expect_err("a disposed family admits nothing");
    assert!(refused.contains("faulted"), "{refused}");

    let mut metrics = ShardMetrics::default();
    harness.backend.owners.observe(&mut metrics);
    let row = metrics.srt_owners[AddressFamily::V4.index()];
    assert!(
        row.present && row.faulted,
        "the disposed family keeps reporting its fault: {row:?}"
    );
    assert!(
        !harness.backend.owners.has_owner(),
        "a quiescent disposal leaves nothing for the shard to drive"
    );
}

/// A family whose disposal is still pending must keep the shard driving its
/// runtime — that is what discharges the cleanup — and must be collected by a
/// due service visit, with admission closed the whole time.
#[test]
fn a_pending_disposal_keeps_the_shard_driving_and_is_collected() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 16 * 1316]);
    harness.add_resolved(srt_spec("out", 1, &url_for(sink.addr)), vec![sink.addr]);
    assert!(
        harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 5),
        "the output delivers before the fault"
    );

    // A real teardown miss needs a hung RX stop or kernel-owned sends, which a
    // harness cannot produce, so the pending state is injected here: the park
    // bound, the wake, the retry and the release below are all production.
    harness
        .backend
        .owners
        .force_disposal_pending(AddressFamily::V4);
    assert!(
        harness.backend.owners.has_owner(),
        "a pending disposal keeps the shard's runtime driven"
    );
    assert!(
        harness.backend.owners.park_bound(Duration::from_secs(10)) <= Duration::from_millis(250),
        "a pending disposal bounds the shard's park by its retry deadline"
    );

    let request = SrtFabricEgressConnectSpec::from_url(&url_for(sink.addr), Duration::from_secs(5))
        .connect_request(&[sink.addr])
        .expect("a single peer builds a direct request");
    let refused = harness
        .backend
        .owners
        .connect(request)
        .expect_err("a closed family admits nothing while its Owner is torn down");
    assert!(refused.contains("faulted"), "{refused}");

    assert!(
        harness.pump(Duration::from_secs(10), |backend| !backend
            .owners
            .disposal_pending(AddressFamily::V4)),
        "the due retry collects the Owner"
    );
    assert!(
        harness.backend.owners.owner(AddressFamily::V4).is_none(),
        "the collected Owner is released"
    );
    assert!(
        !harness.backend.owners.has_owner(),
        "a collected disposal leaves nothing for the shard to drive"
    );

    let request = SrtFabricEgressConnectSpec::from_url(&url_for(sink.addr), Duration::from_secs(5))
        .connect_request(&[sink.addr])
        .expect("a single peer builds a direct request");
    let refused = harness
        .backend
        .owners
        .connect(request)
        .expect_err("the family stays closed after collection");
    assert!(refused.contains("faulted"), "{refused}");
}
