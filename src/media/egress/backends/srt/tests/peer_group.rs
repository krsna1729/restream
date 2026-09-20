//! Bonded peer-group collisions: a bond whose legs reach different remote
//! receiving groups is not an SRT bond, so the WHOLE output fails -- exactly
//! that output, attributed by logical caller, generation-safe, never an Owner
//! fault. (Collision DETECTION is upstream's; these tests deliver the typed
//! fault the way the drain does.)

use super::super::*;
use super::support::*;
use crate::media::egress::backends::srt::owner_set::SrtOwnerEvent;
use srt_transport::advanced::caller::CallerGroupFault;
use srt_transport::advanced::sink::TxAttribution;
use std::sync::atomic::{AtomicBool, Ordering};

fn silent_peer() -> (std::net::UdpSocket, std::net::SocketAddr) {
    let socket = std::net::UdpSocket::bind("127.0.0.1:0").expect("bind");
    let addr = socket.local_addr().expect("addr");
    (socket, addr)
}

fn bond_url(first: std::net::SocketAddr, second: std::net::SocketAddr) -> String {
    format!("srt://{first}?streamid=publish%3Akey&bond={second}&type=broadcast")
}

fn fault(caller: SrtCaller, peer: std::net::SocketAddr) -> SrtOwnerEvent {
    SrtOwnerEvent::PeerGroupCollision {
        family: caller.family,
        fault: CallerGroupFault {
            id: caller.id,
            peer,
            collision: srt_proto::PeerGroupCollision {
                member_id: 2,
                expected_peer_group_id: 0x4000_0001,
                actual_peer_group_id: 0x4000_0002,
            },
        },
    }
}

fn caller_of(harness: &Harness, output: &str) -> SrtCaller {
    let key = harness.backend.output_sockets[&OutputId::new(output)];
    harness.backend.leaves[key.0]
        .as_ref()
        .expect("leaf")
        .caller()
}

fn set(flag: &Arc<AtomicBool>) -> bool {
    flag.load(Ordering::Relaxed)
}

use std::sync::Arc;

/// E. Immediate admission: the collision retires exactly that output. The
/// sibling stays and the Owner is not faulted.
#[test]
fn a_collision_fails_the_immediately_admitted_bonded_output_only() {
    let sink = SinkPeer::v4();
    let (_hold, silent) = silent_peer();
    let mut harness = Harness::new();
    let (bond_spec, bond_flag) = srt_spec_with_flag("bond", 1, &bond_url(silent, silent));
    harness.add_resolved(bond_spec, vec![silent, silent]);
    let (sibling_spec, sibling_flag) = srt_spec_with_flag("sibling", 1, &url_for(sink.addr));
    harness.add_resolved(sibling_spec, vec![sink.addr]);
    assert_eq!(harness.backend.callers.len(), 2);

    let caller = caller_of(&harness, "bond");
    harness.backend.handle_owner_event(fault(caller, silent));

    assert!(
        !harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("bond"))
    );
    assert!(set(&bond_flag), "terminated unexpectedly");
    assert!(
        !harness.backend.callers.contains_key(&caller),
        "caller retired"
    );
    assert!(
        harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("sibling"))
    );
    assert!(!set(&sibling_flag), "the sibling is untouched");
    assert!(
        harness
            .backend
            .owners
            .owner(AddressFamily::V4)
            .unwrap()
            .fault()
            .is_none(),
        "a peer-group collision is session-local, never an Owner fault"
    );

    // Delivered again (e.g. a duplicate): nothing else is affected.
    let stale = harness.backend.stale_events;
    harness.backend.handle_owner_event(fault(caller, silent));
    assert_eq!(harness.backend.stale_events, stale + 1);
    assert!(
        harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("sibling"))
    );
}

/// F. Queued admission: the request is known only by `PoolRequestId` until the
/// Owner admits it; the collision that follows the admission (same drain
/// pass, in order) installs the output and then fails it, exactly once.
#[test]
fn a_collision_after_a_queued_admission_is_not_lost() {
    let (_hold_a, first_peer) = silent_peer();
    let (_hold_b, bond_peer) = silent_peer();
    let mut harness = Harness::with_settings(SrtOwnerSettings::new(1));
    harness.add_resolved(srt_spec("first", 1, &url_for(first_peer)), vec![first_peer]);
    let (spec, flag) = srt_spec_with_flag("bond", 1, &bond_url(bond_peer, bond_peer));
    harness.add_resolved(spec, vec![bond_peer, bond_peer]);
    assert!(
        harness
            .backend
            .pending_connects
            .contains_key(&OutputId::new("bond")),
        "the bond waits in the pool queue under a request id"
    );
    assert!(
        !harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("bond"))
    );

    // Free the only permit, then take ONE service + drain by hand: the pool
    // admits the queued bond and its Admitted event is in the drain.
    harness
        .backend
        .on_command(EgressCommand::Remove(OutputId::new("first")));
    let _ = harness.backend.owners.service();
    let mut events = Vec::new();
    harness.backend.owners.drain_events(&mut events);
    let admitted = events
        .iter()
        .find_map(|event| match event {
            SrtOwnerEvent::Admitted { family, caller, .. } => Some(SrtCaller {
                family: *family,
                id: *caller,
            }),
            _ => None,
        })
        .expect("the queued bond was admitted");
    // The collision arrives right behind the admission, as the drain orders it.
    events.push(fault(admitted, bond_peer));

    let mut installed_between = false;
    for event in events {
        let is_collision = matches!(event, SrtOwnerEvent::PeerGroupCollision { .. });
        if is_collision {
            installed_between = harness
                .backend
                .output_sockets
                .contains_key(&OutputId::new("bond"));
        }
        harness.backend.handle_owner_event(event);
    }
    assert!(
        installed_between,
        "the admission installed the output first"
    );
    assert!(
        !harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("bond")),
        "then the collision failed it"
    );
    assert!(set(&flag), "failed exactly that output");
    assert!(harness.backend.queued_requests.is_empty());
    assert!(!harness.backend.callers.contains_key(&admitted));
    assert_eq!(
        harness.backend.stale_events, 0,
        "nothing was mistaken for stale"
    );
}

/// G. A collision for a removed/replaced generation never touches the
/// replacement, even though it reuses the Restream leaf slot.
#[test]
fn a_stale_collision_never_hits_a_replacement_generation() {
    let (_hold, silent) = silent_peer();
    let mut harness = Harness::new();
    harness.add_resolved(
        srt_spec("bond", 1, &bond_url(silent, silent)),
        vec![silent, silent],
    );
    let old = caller_of(&harness, "bond");
    let old_key = harness.backend.output_sockets[&OutputId::new("bond")];
    harness
        .backend
        .on_command(EgressCommand::Remove(OutputId::new("bond")));
    assert!(
        !harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("bond"))
    );

    let (spec, flag) = srt_spec_with_flag("bond", 2, &bond_url(silent, silent));
    harness.add_resolved(spec, vec![silent, silent]);
    let new = caller_of(&harness, "bond");
    assert_ne!(old, new, "a new logical caller");
    assert_eq!(
        harness.backend.output_sockets[&OutputId::new("bond")],
        old_key,
        "the leaf slot was reused"
    );

    let stale = harness.backend.stale_events;
    harness.backend.handle_owner_event(fault(old, silent));
    assert_eq!(
        harness.backend.stale_events,
        stale + 1,
        "the old fault is stale"
    );
    assert!(
        harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("bond"))
    );
    assert!(!set(&flag), "generation 2 is untouched");
}

/// H. Different same-family endpoints are a valid bond request: only the
/// handshake's peer-group identity may fail it.
#[test]
fn different_endpoints_are_not_rejected_as_different_receivers() {
    let (_hold_a, a) = silent_peer();
    let (_hold_b, b) = silent_peer();
    assert_ne!(a, b);
    let mut harness = Harness::new();
    let (spec, flag) = srt_spec_with_flag("bond", 1, &bond_url(a, b));
    harness.add_resolved(spec, vec![a, b]);
    assert!(
        harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("bond"))
    );
    assert_eq!(harness.backend.callers.len(), 1, "one logical bond");
    assert!(!set(&flag));
}

/// I. An ordinary leg failure (leg-attributed output failure) is NOT a logical
/// output failure: the group's other legs stay healthy and the output stays.
#[test]
fn an_ordinary_leg_failure_does_not_fail_the_bonded_output() {
    let (_hold, silent) = silent_peer();
    let mut harness = Harness::new();
    let (spec, flag) = srt_spec_with_flag("bond", 1, &bond_url(silent, silent));
    harness.add_resolved(spec, vec![silent, silent]);
    let caller = caller_of(&harness, "bond");
    harness
        .backend
        .handle_owner_event(SrtOwnerEvent::OutputFailure {
            family: caller.family,
            attribution: TxAttribution::caller(caller.id, 2),
        });
    // A leg-level TX failure only bumps a counter.
    harness
        .backend
        .handle_owner_event(SrtOwnerEvent::TxFailure {
            family: caller.family,
            attribution: TxAttribution::caller(caller.id, 2),
            class: srt_transport::compio::TxFailureClass::PeerLocal,
        });
    assert!(!set(&flag), "the output was not failed");
    assert!(
        harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("bond"))
    );
    assert!(harness.backend.callers.contains_key(&caller));
}
