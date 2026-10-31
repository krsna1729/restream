//! The Owner's bounded inbox drain.

use super::{TRANSFERS_PER_VISIT, drain_bounded};

/// An inbox refilled while it drains — each accepted transfer queues another,
/// as a peer can keep sending handshakes — cannot extend one visit past its
/// allowance; what is left waits for the next visit.
#[test]
fn a_refilled_inbox_cannot_extend_one_visit() {
    let (sender, receiver) = flume::unbounded();
    for item in 0..4 {
        sender.send(item).unwrap();
    }
    let mut stash = Some(-1);
    let mut applied = Vec::new();
    let count = drain_bounded(&mut stash, &receiver, TRANSFERS_PER_VISIT, |item| {
        applied.push(item);
        // Refill, up to a ceiling so an unbounded drain still ends.
        if applied.len() < 1_000 {
            sender.send(item + 100).unwrap();
        }
    });
    assert_eq!(count, TRANSFERS_PER_VISIT);
    assert_eq!(applied.len(), TRANSFERS_PER_VISIT);
    assert_eq!(applied[0], -1, "the stashed transfer goes first");
    assert!(!receiver.is_empty(), "the rest waits for the next visit");
}

/// A short inbox drains in the one visit: the cap adds no latency.
#[test]
fn a_short_inbox_drains_in_one_visit() {
    let (sender, receiver) = flume::unbounded();
    for item in 0..5 {
        sender.send(item).unwrap();
    }
    let mut stash = None;
    let mut applied = Vec::new();
    assert_eq!(
        drain_bounded(&mut stash, &receiver, TRANSFERS_PER_VISIT, |item| applied
            .push(item)),
        5
    );
    assert_eq!(applied, vec![0, 1, 2, 3, 4]);
    assert!(receiver.is_empty());
}
