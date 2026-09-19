//! Shard topology: one Compio runtime per shard, at most one Owner per
//! address family, one shared caller socket per Owner, no per-output
//! transport object.

use super::super::*;
use super::support::*;
use crate::media::egress::shard::{EgressShardConfig, EgressShardHandle};
use bytes::Bytes;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

// A. The backend and the runtime/Owner holder are `!Send`: this fails to
// compile if either ever becomes `Send`, because the trait would then have
// two applicable impls and inference would be ambiguous.
trait AmbiguousIfSend<A> {
    fn probe() {}
}
impl<T: ?Sized> AmbiguousIfSend<()> for T {}
impl<T: ?Sized + Send> AmbiguousIfSend<u8> for T {}
const _: fn() = || <SrtShardBackend as AmbiguousIfSend<_>>::probe();
const _: fn() = || <SrtOwners as AmbiguousIfSend<_>>::probe();

/// A. The runtime and Owners are built, used and dropped on the shard thread.
/// `SrtOwners` debug-asserts its home thread on every entry point and on
/// drop, so any off-thread use would panic the shard; a clean run through the
/// production shard loop (including shutdown) proves none happened.
#[test]
fn runtime_and_owners_live_and_die_on_the_shard_thread() {
    let sink = SinkPeer::v4();
    let feed = TestFeed::new();
    let built_on = Arc::new(Mutex::new(None));
    let shard_thread = Arc::clone(&built_on);
    let reader = feed.reader();
    let config = EgressShardConfig::new(16, 4, 4, 4, Duration::from_millis(20))
        .unwrap()
        .with_drain_timeout(Duration::from_millis(300));
    let handle = EgressShardHandle::try_spawn_with(
        crate::media::egress::command::ShardId::new(0),
        config,
        move || {
            *shard_thread.lock().unwrap() = Some(std::thread::current().id());
            resolve_runtime::resolving_srt_shard_backend(
                reader,
                budget(),
                Duration::from_millis(300),
                64,
                settings(),
            )
        },
    )
    .expect("the production runtime builds on the shard thread");
    assert_ne!(
        built_on.lock().unwrap().unwrap(),
        std::thread::current().id()
    );

    handle
        .try_send(EgressCommand::Add(srt_spec("out", 1, &url_for(sink.addr))))
        .unwrap();
    std::thread::sleep(Duration::from_millis(300));
    handle
        .try_send(EgressCommand::Remove(OutputId::new("out")))
        .unwrap();
    let snapshot = handle.shutdown_and_join();
    assert!(snapshot.stopped);
    assert!(!snapshot.panicked, "no off-thread runtime use or drop");
}

/// B/C/E. Many outputs on one shard share ONE runtime and ONE Owner per
/// family; every caller leaves from the same socket.
#[test]
fn many_ipv4_outputs_share_one_owner_and_one_caller_socket() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    for index in 0..6 {
        harness.add_resolved(
            srt_spec(&format!("out-{index}"), 1, &url_for(sink.addr)),
            vec![sink.addr],
        );
    }
    assert_eq!(harness.backend.output_sockets.len(), 6);
    assert_eq!(
        harness.backend.owners.owner_count(),
        1,
        "one Owner for IPv4"
    );

    harness.feed_until(&unit, Duration::from_secs(10), |_| sink.payloads() >= 12);
    assert!(sink.payloads() >= 12);
    let sources = sink.stats.sources.lock().unwrap().clone();
    assert_eq!(sources.len(), 1, "six callers, one socket: {sources:?}");

    // D. The transport population is the Owner's fixed one, not per caller.
    let owner = harness.backend.owners.owner(AddressFamily::V4).unwrap();
    assert_eq!(
        owner.tx_pool_snapshot().capacity,
        owner_set::SRT_OWNER_TX_CAPACITY
    );
    assert_eq!(
        owner.caller_pool_stats().unwrap().queued,
        0,
        "six callers admitted without any per-caller queue"
    );
}

/// C/F. IPv4 and IPv6 direct outputs coexist on one shard through exactly two
/// Owners, each with its own single socket.
#[test]
fn ipv4_and_ipv6_outputs_coexist_on_two_family_owners() {
    let Some(sink6) = SinkPeer::v6() else {
        eprintln!("skipping: no IPv6 loopback on this host");
        return;
    };
    let sink4 = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    for index in 0..3 {
        harness.add_resolved(
            srt_spec(&format!("v4-{index}"), 1, &url_for(sink4.addr)),
            vec![sink4.addr],
        );
        harness.add_resolved(
            srt_spec(&format!("v6-{index}"), 1, &url_for(sink6.addr)),
            vec![sink6.addr],
        );
    }
    assert_eq!(harness.backend.output_sockets.len(), 6);
    assert_eq!(
        harness.backend.owners.owner_count(),
        2,
        "at most two Owners"
    );

    let start = std::time::Instant::now();
    while (sink4.payloads() < 6 || sink6.payloads() < 6)
        && start.elapsed() < Duration::from_secs(10)
    {
        harness.publish(unit.clone());
        harness.turn(Duration::from_millis(2));
    }
    assert!(sink4.payloads() >= 6 && sink6.payloads() >= 6);
    assert_eq!(sink4.stats.sources.lock().unwrap().len(), 1);
    assert_eq!(sink6.stats.sources.lock().unwrap().len(), 1);
}

/// G. A bond whose legs resolve to mixed IPv4/IPv6 addresses fails the output
/// explicitly: no Owner, leaf, pending or request state is left behind.
#[test]
fn mixed_family_bond_is_rejected_without_partial_admission() {
    let mut harness = Harness::new();
    let v4: std::net::SocketAddr = "127.0.0.1:9000".parse().unwrap();
    let v6: std::net::SocketAddr = "[::1]:9001".parse().unwrap();
    let (spec, flag) = srt_spec_with_flag(
        "bond",
        1,
        "srt://127.0.0.1:9000?bond=[::1]:9001&type=broadcast",
    );
    harness.add_resolved(spec, vec![v4, v6]);

    assert!(
        flag.load(Ordering::Relaxed),
        "the output is reported failed"
    );
    assert!(harness.backend.output_sockets.is_empty());
    assert!(harness.backend.pending_connects.is_empty());
    assert!(harness.backend.queued_requests.is_empty());
    assert!(harness.backend.callers.is_empty());
    assert_eq!(
        harness.backend.owners.owner_count(),
        0,
        "no Owner was created for it"
    );
}

/// The shard factory surfaces an unbuildable runtime as a typed error; here we
/// only assert the success path leaves the flag untouched (the failure branch
/// is a `Result::Err` from `production_runtime_builder`, exercised by the
/// `Backend` error mapping in `factory`).
#[test]
fn shard_factory_builds_backend_or_returns_a_typed_error() {
    let feed = TestFeed::new();
    let built = resolve_runtime::resolving_srt_shard_backend(
        feed.reader(),
        budget(),
        Duration::from_millis(100),
        8,
        settings(),
    );
    let ok = AtomicBool::new(built.is_ok());
    assert!(ok.load(Ordering::Relaxed));
}
