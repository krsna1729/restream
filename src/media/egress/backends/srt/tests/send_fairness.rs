//! Send path and scheduling: a slow peer parks and never spins or blocks
//! siblings; Owner service is once per ready batch under finite budgets; a
//! network wake does bounded work, not a population scan.

use super::super::*;
use super::support::*;
use bytes::Bytes;
use srt_transport::compio::OwnerServiceBudget;
use std::time::Duration;

fn v4_metrics(harness: &Harness) -> crate::media::egress::metrics::OwnerFamilyMetrics {
    let mut metrics = ShardMetrics::default();
    harness.backend.owners.observe(&mut metrics);
    metrics.srt_owners[AddressFamily::V4.index()]
}

/// Q. One wedged peer cannot stop another leaf from progressing. (Live SRT
/// drops late packets rather than closing the window for a wedged peer, so
/// the sibling's progress -- not parking -- is the property here.)
#[test]
fn a_slow_peer_parks_without_stopping_sibling_leaves() {
    let slow = SinkPeer::v4();
    let fast = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    harness.add_resolved(srt_spec("slow", 1, &url_for(slow.addr)), vec![slow.addr]);
    harness.add_resolved(srt_spec("fast", 1, &url_for(fast.addr)), vec![fast.addr]);
    assert!(harness.feed_until(&unit, Duration::from_secs(10), |_| {
        slow.payloads() >= 3 && fast.payloads() >= 3
    }));

    slow.set_stalled(true);
    let before = fast.payloads();
    assert!(
        harness.feed_until(&unit, Duration::from_secs(20), |_| fast.payloads()
            >= before + 200),
        "the healthy sibling kept progressing while the peer was wedged"
    );
}

fn silent_peer() -> (std::net::UdpSocket, std::net::SocketAddr) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let addr = socket.local_addr().expect("addr");
    (socket, addr)
}

/// P/T. A caller that cannot send yet (still connecting) parks, and with only
/// parked leaves and no new work `on_ready` asks for no follow-up visit:
/// nothing spins.
#[test]
fn parked_leaves_cause_no_follow_up_visits() {
    let (_hold, silent) = silent_peer();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    harness.add_resolved(srt_spec("waiting", 1, &url_for(silent)), vec![silent]);
    let key = harness.backend.output_sockets[&OutputId::new("waiting")];
    assert!(
        harness.feed_until(&unit, Duration::from_secs(5), |backend| backend
            .blocked
            .contains(&key)),
        "a connecting caller parks"
    );

    let mut follow_ups = 0;
    for _ in 0..30 {
        if harness.backend.on_ready() != EgressShardCommandEffect::Continue {
            follow_ups += 1;
        }
    }
    assert!(
        follow_ups <= 1,
        "{follow_ups} of 30 idle passes asked for another visit"
    );
}

/// S. Several leaves and a multi-fragment unit in ONE ready batch cause ONE
/// Owner service pass, not one per leaf or per fragment.
#[test]
fn one_ready_batch_is_one_owner_service_pass() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    for index in 0..4 {
        harness.add_resolved(
            srt_spec(&format!("out-{index}"), 1, &url_for(sink.addr)),
            vec![sink.addr],
        );
    }
    // Let every leaf connect and go idle on the feed.
    let unit = Bytes::from(vec![0x47u8; 1316]);
    assert!(harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 8));
    for _ in 0..30 {
        harness.turn(Duration::from_millis(1));
    }

    // A 20 KB unit is 16 fragments per leaf; publish it and deliver ONE feed
    // wake, then run exactly the ready passes that wake schedules.
    harness.feed.publish(Bytes::from(vec![0x47u8; 20 * 1316]));
    harness.backend.on_command(EgressCommand::FeedWake);
    let service_before = v4_metrics(&harness).service_visits;
    let visits_before = harness.backend.leaf_visits();
    let mut passes = 0;
    loop {
        let effect = harness.backend.on_ready();
        passes += 1;
        if effect == EgressShardCommandEffect::Continue || passes > 64 {
            break;
        }
    }
    let visits = harness.backend.leaf_visits() - visits_before;
    let service = v4_metrics(&harness).service_visits - service_before;
    assert!(visits >= 4, "every leaf was visited ({visits})");
    assert!(
        service <= 2,
        "{service} Owner service passes for {visits} leaf visits of 16 fragments each"
    );
}

/// R. Owner service stops at its finite budget and reports the exhaustion;
/// the backend then schedules an ordinary ready visit instead of looping.
#[test]
fn owner_service_is_bounded_by_its_budget() {
    let sink = SinkPeer::v4();
    let mut settings = settings();
    settings.service_budget = OwnerServiceBudget {
        max_tx_packets: 1,
        max_actions: 64,
        ..OwnerServiceBudget::default()
    };
    let mut harness = Harness::with_settings(settings);
    let unit = Bytes::from(vec![0x47u8; 1316]);
    for index in 0..6 {
        harness.add_resolved(
            srt_spec(&format!("out-{index}"), 1, &url_for(sink.addr)),
            vec![sink.addr],
        );
    }
    harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 10);
    let metrics = v4_metrics(&harness);
    assert!(
        metrics.service_budget_exhausted > 0,
        "the tiny budget was hit"
    );
    assert!(
        sink.payloads() >= 10,
        "and progress was still made under it: payloads={} {metrics:?} visits={} leaves={}",
        sink.payloads(),
        harness.backend.leaf_visits(),
        harness.backend.output_sockets.len()
    );
}

/// T. With many parked leaves, a wake re-examines only a bounded rotating
/// slice of them, never the whole population.
#[test]
fn a_wake_examines_a_bounded_slice_of_parked_leaves() {
    let (_hold, silent) = silent_peer();
    let mut harness = Harness::with_settings(SrtOwnerSettings::new(128, Duration::from_secs(30)));
    let unit = Bytes::from(vec![0x47u8; 1316]);
    let outputs: usize = 48;
    for index in 0..outputs {
        harness.add_resolved(
            srt_spec(&format!("out-{index}"), 1, &url_for(silent)),
            vec![silent],
        );
    }
    let all_parked = harness.feed_until(&unit, Duration::from_secs(20), |backend| {
        backend.blocked.len() == outputs
    });
    assert!(
        all_parked,
        "leaves parked: {} of {outputs}",
        harness.backend.blocked.len()
    );

    // One batch with no new media re-examines a bounded slice.
    let batches_before = harness.backend.batches();
    let visits_before = harness.backend.leaf_visits();
    harness.backend.on_ready();
    let mut guard = 0;
    while !harness.backend.ready.is_empty() && guard < 128 {
        harness.backend.on_ready();
        guard += 1;
    }
    let batches = harness.backend.batches() - batches_before;
    let visits = harness.backend.leaf_visits() - visits_before;
    assert!(batches >= 1);
    assert!(
        visits <= batches * BLOCKED_RECHECK_PER_BATCH as u64,
        "{visits} leaf visits in {batches} batches with {outputs} parked leaves"
    );
    assert!(visits < outputs as u64, "fewer than the whole population");
}

/// A shard that never idles (media always ready) must still reap TX
/// completions and receive datagrams: `block_on` alone does not poll the
/// driver, so without a per-batch driver poll the 16 TX slots stay in flight
/// forever and the leaf starves.
#[test]
fn a_shard_that_never_parks_still_reaps_tx_completions() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    harness.add_resolved(srt_spec("busy", 1, &url_for(sink.addr)), vec![sink.addr]);
    assert!(harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 3));

    let before = sink.payloads();
    let start = std::time::Instant::now();
    while start.elapsed() < Duration::from_secs(3) && sink.payloads() < before + 200 {
        harness.publish(unit.clone());
        harness.backend.on_media_tick();
        harness.backend.on_ready();
    }
    assert!(
        sink.payloads() >= before + 200,
        "busy loop delivered {} payloads (metrics {:?})",
        sink.payloads() - before,
        v4_metrics(&harness)
    );
}
