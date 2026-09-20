//! Connect deadlines are request-local: each output's `LeafPolicy.connect_timeout`
//! rides on its own `CallerConfig`, the Owner only governs capacity, and queue
//! wait never consumes the attempt window.

use super::super::*;
use super::support::*;
use std::time::{Duration, Instant};

fn silent_peer() -> (std::net::UdpSocket, std::net::SocketAddr) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let addr = socket.local_addr().expect("addr");
    (socket, addr)
}

fn spec_with_timeout(
    id: &str,
    addr: std::net::SocketAddr,
    timeout: Duration,
) -> (OutputSpec, std::sync::Arc<std::sync::atomic::AtomicBool>) {
    let (mut spec, flag) = srt_spec_with_flag(id, 1, &url_for(addr));
    spec.policy.connect_timeout = timeout;
    (spec, flag)
}

fn failed(flag: &std::sync::Arc<std::sync::atomic::AtomicBool>) -> bool {
    flag.load(std::sync::atomic::Ordering::Relaxed)
}

/// C. Two requests on ONE family Owner keep their own deadlines: the Owner's
/// capacity never rewrites either.
#[test]
fn two_outputs_on_one_owner_expire_at_their_own_timeouts() {
    let (_hold_a, a) = silent_peer();
    let (_hold_b, b) = silent_peer();
    let mut harness = Harness::with_settings(SrtOwnerSettings::new(4));
    let (fast, fast_flag) = spec_with_timeout("fast", a, Duration::from_millis(150));
    let (slow, slow_flag) = spec_with_timeout("slow", b, Duration::from_secs(30));
    harness.add_resolved(fast, vec![a]);
    harness.add_resolved(slow, vec![b]);
    assert_eq!(harness.backend.callers.len(), 2, "one Owner, two requests");

    let expired = harness.pump(Duration::from_secs(5), |backend| {
        !backend.output_sockets.contains_key(&OutputId::new("fast"))
    });
    assert!(expired, "the 150 ms output expired at its own timeout");
    assert!(failed(&fast_flag));
    assert!(
        harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("slow")),
        "the 30 s output was not shortened by its neighbour"
    );
    assert!(!failed(&slow_flag));
}

/// D. Queue wait is not part of the attempt window: with capacity 1, a request
/// whose timeout is far shorter than its wait survives the queue, then gets
/// its FULL window once admitted.
#[test]
fn queue_wait_does_not_consume_the_connect_timeout() {
    let (_hold_a, a) = silent_peer();
    let (_hold_b, b) = silent_peer();
    let mut harness = Harness::with_settings(SrtOwnerSettings::new(1));
    let (holder, _) = spec_with_timeout("holder", a, Duration::from_secs(60));
    let (queued, queued_flag) = spec_with_timeout("queued", b, Duration::from_millis(300));
    harness.add_resolved(holder, vec![a]);
    harness.add_resolved(queued, vec![b]);
    assert!(
        harness
            .backend
            .pending_connects
            .contains_key(&OutputId::new("queued"))
    );

    // Wait three times the queued request's own timeout: no application-side
    // timer may kill it, and it stays queued behind the permit.
    let waited = harness.pump(Duration::from_millis(900), |_| false);
    assert!(!waited);
    assert!(
        !failed(&queued_flag),
        "queue wait counted against the timeout"
    );
    assert!(
        harness
            .backend
            .pending_connects
            .contains_key(&OutputId::new("queued"))
    );

    // Admit it now: it must stay alive for a good part of ITS window (a
    // deadline counted from queueing would already have passed)...
    harness
        .backend
        .on_command(EgressCommand::Remove(OutputId::new("holder")));
    let admitted = harness.pump(Duration::from_secs(2), |backend| {
        backend
            .output_sockets
            .contains_key(&OutputId::new("queued"))
    });
    assert!(admitted, "the queued output was admitted");
    let start = Instant::now();
    let alive_at = harness.pump(Duration::from_millis(150), |backend| {
        !backend
            .output_sockets
            .contains_key(&OutputId::new("queued"))
    });
    assert!(!alive_at, "still inside its own fresh 300 ms window");
    assert!(start.elapsed() >= Duration::from_millis(140));
    // ...and then expires at its own timeout.
    let expired = harness.pump(Duration::from_secs(3), |backend| {
        !backend
            .output_sockets
            .contains_key(&OutputId::new("queued"))
    });
    assert!(
        expired && failed(&queued_flag),
        "expired after its own window"
    );
}

/// The Owner's service budget is upstream's, independent of pool capacity and
/// fan-out: no capacity-derived multiplier exists any more.
#[test]
fn service_budget_does_not_scale_with_caller_capacity() {
    let small = SrtOwnerSettings::new(1);
    let large = SrtOwnerSettings::new(4096);
    assert_ne!(small.caller_max_in_flight, large.caller_max_in_flight);
    assert_eq!(
        small.service_budget, large.service_budget,
        "changing capacity alone must not change the service budget"
    );
    assert_eq!(
        small.service_budget,
        srt_transport::compio::OwnerServiceBudget::default()
    );
}
