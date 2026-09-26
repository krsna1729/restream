use super::super::*;
use super::support::*;
use bytes::Bytes;
use std::time::Duration;

/// H: a direct connect is admitted immediately by the family Owner, becomes
/// exactly one leaf for the right output/generation, and delivers TS payload.
#[test]
fn direct_connect_admits_immediately_and_delivers_payload() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 188 * 7]);

    harness.add_resolved(srt_spec("out-a", 3, &url_for(sink.addr)), vec![sink.addr]);

    assert_eq!(harness.backend.output_sockets.len(), 1, "one leaf");
    let key = harness.backend.output_sockets[&OutputId::new("out-a")];
    let leaf = harness.backend.leaves[key.0].as_ref().expect("leaf");
    assert_eq!(leaf.common().generation, 3);
    assert_eq!(leaf.caller().family, AddressFamily::V4);
    assert_eq!(harness.backend.owners.owner_count(), 1);

    // The leaf anchors at the feed's live edge on its first visit, so media
    // published after the handshake is what it delivers.
    harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 5);
    assert!(
        sink.payloads() >= 5,
        "sink saw {} payloads",
        sink.payloads()
    );
}

/// A peer that never answers: a bound UDP socket nobody services, so a
/// handshake to it stays in flight until the attempt deadline.
fn silent_peer() -> (std::net::UdpSocket, std::net::SocketAddr) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let addr = socket.local_addr().expect("addr");
    (socket, addr)
}

fn one_permit() -> SrtOwnerSettings {
    // max_in_flight 1 => queue capacity 1: a second request queues, a third
    // is refused.
    SrtOwnerSettings::new(1)
}

fn terminated(flag: &Arc<AtomicBool>) -> bool {
    flag.load(Ordering::Relaxed)
}

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

/// I: a queued Owner connect is correlated to its exact output, and the later
/// admission installs exactly that output's leaf.
#[test]
fn queued_connect_is_correlated_then_admitted_to_the_same_output() {
    let (_hold_a, dead_a) = silent_peer();
    let (_hold_b, dead_b) = silent_peer();
    let mut harness = Harness::with_settings(one_permit());

    harness.add_resolved(srt_spec("out-a", 1, &url_for(dead_a)), vec![dead_a]);
    assert_eq!(harness.backend.output_sockets.len(), 1, "first is admitted");

    harness.add_resolved(srt_spec("out-b", 1, &url_for(dead_b)), vec![dead_b]);
    assert_eq!(harness.backend.queued_requests.len(), 1, "second is queued");
    assert!(
        !harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("out-b"))
    );
    let ((family, _), queued) = harness.backend.queued_requests.iter().next().unwrap();
    assert_eq!(*family, AddressFamily::V4);
    assert_eq!((queued.output_id.as_str(), queued.generation), ("out-b", 1));

    // Retire the first output: its permit frees, the pool admits the queued
    // request, and the Owner reports it as an event the backend attributes.
    harness
        .backend
        .on_command(EgressCommand::Remove(OutputId::new("out-a")));
    let attached = harness.pump(Duration::from_secs(5), |backend| {
        backend.output_sockets.contains_key(&OutputId::new("out-b"))
    });
    assert!(
        attached,
        "queued output must be admitted once a permit frees"
    );
    let key = harness.backend.output_sockets[&OutputId::new("out-b")];
    let leaf = harness.backend.leaves[key.0].as_ref().unwrap();
    assert_eq!(leaf.common().output_id.as_str(), "out-b");
    assert_eq!(leaf.common().generation, 1);
    assert!(
        harness.backend.queued_requests.is_empty(),
        "no request state leaks"
    );
    assert!(harness.backend.pending_connects.is_empty());
}

/// J: a full pool refuses the output without leaking a leaf or request.
#[test]
fn full_pool_fails_the_output_without_leaking_state() {
    let (_h1, p1) = silent_peer();
    let (_h2, p2) = silent_peer();
    let (_h3, p3) = silent_peer();
    let mut harness = Harness::with_settings(one_permit());
    harness.add_resolved(srt_spec("out-a", 1, &url_for(p1)), vec![p1]);
    harness.add_resolved(srt_spec("out-b", 1, &url_for(p2)), vec![p2]);
    let (spec, flag) = srt_spec_with_flag("out-c", 1, &url_for(p3));
    harness.add_resolved(spec, vec![p3]);

    assert!(
        terminated(&flag),
        "the refused output is reported as terminated"
    );
    assert!(
        !harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("out-c"))
    );
    assert!(
        !harness
            .backend
            .pending_connects
            .contains_key(&OutputId::new("out-c"))
    );
    assert_eq!(
        harness.backend.queued_requests.len(),
        1,
        "only out-b is queued"
    );
    assert_eq!(harness.backend.output_sockets.len(), 1);
}

/// K: an output replaced (or removed) while its request is queued never
/// attaches its stale admission to the replacement generation.
#[test]
fn stale_queued_admission_never_attaches_to_a_replacement_generation() {
    let (_h1, p1) = silent_peer();
    let (_h2, p2) = silent_peer();
    let mut harness = Harness::with_settings(one_permit());
    harness.add_resolved(srt_spec("out-a", 1, &url_for(p1)), vec![p1]);
    harness.add_resolved(srt_spec("out-b", 1, &url_for(p2)), vec![p2]);
    assert_eq!(harness.backend.queued_requests.len(), 1);

    // Replace out-b (generation 2, DNS still pending) while gen 1 is queued.
    harness
        .backend
        .on_command(EgressCommand::Update(srt_spec("out-b", 2, &url_for(p2))));
    harness
        .backend
        .on_command(EgressCommand::Remove(OutputId::new("out-a")));
    let retired = harness.pump(Duration::from_secs(5), |backend| {
        backend.stale_events() >= 1
    });
    assert!(retired, "the stale admission must be observed and retired");

    assert!(
        !harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("out-b")),
        "generation 2 has not resolved; gen 1's admission must not have attached to it"
    );
    let pending = &harness.backend.pending_connects[&OutputId::new("out-b")];
    assert_eq!(pending.common.generation, 2);
    assert_eq!(pending.stage, PendingStage::Resolving);
    let stats = harness
        .backend
        .owners
        .owner(AddressFamily::V4)
        .unwrap()
        .caller_pool_stats()
        .unwrap();
    assert_eq!(stats.in_flight, 0, "the stale caller's permit was released");

    // Generation 2's own DNS completion then connects normally.
    harness
        .resolved
        .send(SrtResolvedConnect {
            output_id: OutputId::new("out-b"),
            generation: 2,
            peer_addrs: vec![p2],
        })
        .unwrap();
    harness.backend.on_media_tick();
    let key = harness.backend.output_sockets[&OutputId::new("out-b")];
    assert_eq!(
        harness.backend.leaves[key.0]
            .as_ref()
            .unwrap()
            .common()
            .generation,
        2
    );
}

/// A late DNS result for an old generation never connects the replacement.
#[test]
fn late_dns_for_an_old_generation_is_ignored() {
    let (_h1, p1) = silent_peer();
    let mut harness = Harness::new();
    harness
        .backend
        .on_command(EgressCommand::Add(srt_spec("out-a", 1, &url_for(p1))));
    harness
        .backend
        .on_command(EgressCommand::Update(srt_spec("out-a", 2, &url_for(p1))));
    harness
        .resolved
        .send(SrtResolvedConnect {
            output_id: OutputId::new("out-a"),
            generation: 1,
            peer_addrs: vec![p1],
        })
        .unwrap();
    harness.backend.on_media_tick();
    assert!(
        harness.backend.output_sockets.is_empty(),
        "stale DNS attached nothing"
    );
    assert_eq!(
        harness.backend.owners.owner_count(),
        0,
        "no Owner was even created"
    );
    assert_eq!(
        harness.backend.pending_connects[&OutputId::new("out-a")]
            .common
            .generation,
        2
    );
}

/// L: pool expiry closes exactly the timed-out output; a healthy sibling on
/// the same Owner keeps delivering.
#[test]
fn connect_expiry_closes_the_pending_output_and_spares_siblings() {
    let live = SinkPeer::v4();
    let (_h, dead) = silent_peer();
    let mut harness = Harness::with_settings(SrtOwnerSettings::new(8));
    let unit = Bytes::from(vec![0x47u8; 1316]);

    harness.add_resolved(srt_spec("healthy", 1, &url_for(live.addr)), vec![live.addr]);
    let (mut spec, flag) = srt_spec_with_flag("doomed", 1, &url_for(dead));
    // The deadline is this OUTPUT's own connect timeout, not an Owner setting.
    spec.policy.connect_timeout = Duration::from_millis(150);
    harness.add_resolved(spec, vec![dead]);
    assert_eq!(harness.backend.output_sockets.len(), 2);

    let expired = harness.pump(Duration::from_secs(5), |backend| {
        !backend
            .output_sockets
            .contains_key(&OutputId::new("doomed"))
    });
    assert!(expired, "the unanswered attempt must expire");
    assert!(
        terminated(&flag),
        "and be reported as unexpectedly terminated"
    );
    assert!(
        harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("healthy"))
    );

    let before = live.payloads();
    harness.feed_until(&unit, Duration::from_secs(10), |_| {
        live.payloads() >= before + 3
    });
    assert!(live.payloads() >= before + 3, "the sibling kept delivering");
}

/// M: removing one output leaves its siblings operational.
#[test]
fn removing_one_output_leaves_siblings_operational() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    harness.add_resolved(srt_spec("out-a", 1, &url_for(sink.addr)), vec![sink.addr]);
    harness.add_resolved(srt_spec("out-b", 1, &url_for(sink.addr)), vec![sink.addr]);
    assert_eq!(harness.backend.callers.len(), 2);

    harness
        .backend
        .on_command(EgressCommand::Remove(OutputId::new("out-a")));
    assert_eq!(harness.backend.callers.len(), 1);
    assert!(
        harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("out-b"))
    );

    let start = std::time::Instant::now();
    while sink.payloads() < 3 && start.elapsed() < Duration::from_secs(10) {
        harness.publish(unit.clone());
        harness.turn(Duration::from_millis(2));
    }
    assert!(
        sink.payloads() >= 3,
        "out-b still delivers after out-a is gone"
    );
}

/// N: repeated add/remove/update reclaims leaf slots and Owner callers, and a
/// logical caller id is never reused.
#[test]
fn churn_reclaims_slots_and_callers_without_identity_reuse() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let capacity = harness.backend.leaves.len();
    let mut seen = std::collections::HashSet::new();

    for round in 0..12u64 {
        let id = format!("out-{}", round % 3);
        harness.add_resolved(
            srt_spec(&id, round + 1, &url_for(sink.addr)),
            vec![sink.addr],
        );
        let key = harness.backend.output_sockets[&OutputId::new(&id)];
        let caller = harness.backend.leaves[key.0].as_ref().unwrap().caller();
        assert!(
            seen.insert(caller.id),
            "logical caller id {caller:?} was reused"
        );
        if round % 2 == 1 {
            harness
                .backend
                .on_command(EgressCommand::Remove(OutputId::new(&id)));
        }
    }
    for name in ["out-0", "out-1", "out-2"] {
        harness
            .backend
            .on_command(EgressCommand::Remove(OutputId::new(name)));
    }
    harness.pump(Duration::from_secs(5), |backend| {
        backend.owners.closing_len() == 0
    });

    assert!(harness.backend.output_sockets.is_empty());
    assert!(harness.backend.callers.is_empty());
    assert_eq!(
        harness.backend.free_leaf_keys.len(),
        capacity,
        "every slot reclaimed"
    );
    let stats = harness
        .backend
        .owners
        .owner(AddressFamily::V4)
        .unwrap()
        .caller_pool_stats()
        .unwrap();
    assert_eq!(stats.in_flight, 0);
    assert_eq!(
        harness
            .backend
            .owners
            .owner(AddressFamily::V4)
            .unwrap()
            .caller()
            .unwrap()
            .table()
            .len(),
        0
    );
}
