//! Bonded egress: one output = one leaf = one `LogicalCallerId`; upstream's
//! `SrtGroup` owns the legs, Backup/Broadcast selection and failover.

use super::super::*;
use super::support::*;
use bytes::Bytes;
use std::sync::atomic::Ordering;
use std::time::Duration;

fn bond_url(first: std::net::SocketAddr, second: std::net::SocketAddr, mode: &str) -> String {
    format!("srt://{first}?streamid=publish%3Akey&bond={second}&type={mode}")
}

fn deliver_until(harness: &mut Harness, until: impl Fn() -> bool, unit: &Bytes) -> bool {
    harness.feed_until(unit, Duration::from_secs(15), |_| until())
}

fn group_legs(harness: &Harness) -> Vec<(u32, String, u64)> {
    let key = harness.backend.output_sockets[&OutputId::new("bond")];
    let caller = harness.backend.leaves[key.0].as_ref().unwrap().caller();
    match harness.backend.owners.stats(&caller) {
        Some(srt_transport::advanced::caller::LogicalCallerStats::Group(group)) => group
            .legs
            .iter()
            .map(|leg| {
                (
                    leg.member_id,
                    format!("{:?}", leg.state),
                    leg.connection
                        .sender
                        .map_or(0, |s| s.total_data_packets_sent),
                )
            })
            .collect(),
        other => panic!("expected a bonded caller, got {:?}", other.is_some()),
    }
}

/// AD/AE. A Broadcast bond is ONE leaf and ONE logical caller, and the group
/// core offers every payload to every healthy leg (bonded to one listener, as
/// upstream's group tests do; the sink merges the legs into one stream).
#[test]
fn broadcast_bond_is_one_leaf_one_logical_caller_and_reaches_every_leg() {
    let sink = SinkPeer::v4();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    harness.add_resolved(
        srt_spec("bond", 1, &bond_url(sink.addr, sink.addr, "broadcast")),
        vec![sink.addr, sink.addr],
    );

    assert_eq!(harness.backend.output_sockets.len(), 1, "one Restream leaf");
    assert_eq!(harness.backend.callers.len(), 1, "one logical caller");
    let owner = harness.backend.owners.owner(AddressFamily::V4).unwrap();
    assert_eq!(
        owner.caller().unwrap().table().len(),
        1,
        "the Owner holds one caller, not one per leg"
    );
    assert_eq!(
        owner.caller_pool_stats().unwrap().in_flight,
        1,
        "one permit for the group"
    );

    assert!(deliver_until(&mut harness, || sink.payloads() >= 20, &unit));
    let legs = group_legs(&harness);
    assert_eq!(legs.len(), 2);
    for (member, state, sent) in &legs {
        assert!(
            *sent >= 20,
            "leg {member} ({state}) was offered only {sent} data packets"
        );
    }
    assert_eq!(
        harness.backend.output_sockets.len(),
        1,
        "legs never become leaves"
    );
}

/// AC. A Backup bond with one dead leg still delivers over the live one: the
/// group core selects the connected leg; Restream does nothing leg-specific.
#[test]
fn backup_bond_delivers_over_the_working_leg() {
    let live = SinkPeer::v4();
    let dead = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead.local_addr().unwrap();
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    // The dead leg is listed first, so it is the preferred one.
    harness.add_resolved(
        srt_spec("bond", 1, &bond_url(dead_addr, live.addr, "backup")),
        vec![dead_addr, live.addr],
    );
    assert_eq!(harness.backend.callers.len(), 1);
    assert!(deliver_until(&mut harness, || live.payloads() >= 4, &unit));
}

/// AF. A leg that never establishes (or fails) does not remove an otherwise
/// healthy logical output: the group keeps delivering over the live leg and
/// Restream still holds exactly one leaf.
#[test]
fn one_failed_leg_does_not_remove_the_bonded_output() {
    let live = SinkPeer::v4();
    let dead = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead.local_addr().unwrap();
    let (spec, flag) = srt_spec_with_flag("bond", 1, &bond_url(live.addr, dead_addr, "broadcast"));
    let mut harness = Harness::new();
    let unit = Bytes::from(vec![0x47u8; 1316]);
    harness.add_resolved(spec, vec![live.addr, dead_addr]);

    assert!(deliver_until(&mut harness, || live.payloads() >= 10, &unit));
    let before = live.payloads();
    assert!(deliver_until(
        &mut harness,
        || live.payloads() >= before + 10,
        &unit
    ));
    assert!(
        !flag.load(Ordering::Relaxed),
        "the output was not torn down"
    );
    assert!(
        harness
            .backend
            .output_sockets
            .contains_key(&OutputId::new("bond"))
    );
    assert_eq!(harness.backend.callers.len(), 1);
}
