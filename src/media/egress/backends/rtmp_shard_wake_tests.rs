//! The completion wake-source rule (`local_followup_visit`). Production RTMP
//! egress is edge-triggered: every combination below that returns `None` must
//! have a real transmit, receive or feed wake pending, and every combination
//! without one must get a local visit, or the leaf parks forever.

use super::*;
use crate::media::egress::backend::{Interest, WaitCondition};

const IO_WAITS: [WaitCondition; 6] = [
    WaitCondition::Io(Interest::READ),
    WaitCondition::Io(Interest::WRITE),
    WaitCondition::Io(Interest::READ_WRITE),
    WaitCondition::FeedOrIo(Interest::READ),
    WaitCondition::FeedOrIo(Interest::WRITE),
    WaitCondition::FeedOrIo(Interest::READ_WRITE),
];

fn progress_shapes(wait: WaitCondition) -> [EngineProgress; 2] {
    [
        EngineProgress::Needs(wait),
        EngineProgress::Progress {
            bytes: 0,
            units: 0,
            wait,
        },
    ]
}

#[test]
fn every_io_wait_without_a_pending_completion_gets_a_local_visit() {
    for wait in IO_WAITS {
        let interest = wait.io_interest();
        for progress in progress_shapes(wait) {
            for transmit_in_flight in [false, true] {
                for buffered_receive in [false, true] {
                    let followup =
                        local_followup_visit(&progress, transmit_in_flight, buffered_receive);
                    let write_stranded = interest.writable && !transmit_in_flight;
                    let read_stranded = interest.readable && buffered_receive;
                    assert_eq!(
                        followup.is_some(),
                        write_stranded || read_stranded,
                        "{progress:?} in_flight={transmit_in_flight} buffered={buffered_receive}"
                    );
                    if let Some(readiness) = followup {
                        assert_eq!(readiness.writable, write_stranded, "{progress:?}");
                        assert_eq!(readiness.readable, read_stranded, "{progress:?}");
                    }
                }
            }
        }
    }
}

/// The negotiation deadlock seen live: a visit writes `connect` and returns
/// `Pending(READ)` while the server's reply is already staged by a consumed
/// receive completion.
#[test]
fn read_wait_with_staged_receive_state_is_revisited_readable() {
    let followup = local_followup_visit(
        &EngineProgress::Needs(WaitCondition::Io(Interest::READ)),
        true,
        true,
    );
    assert_eq!(followup, Some(Readiness::READABLE));
}

/// Bytes in flight guarantee a transmit completion, so waiting for it must not
/// manufacture a spinning local visit.
#[test]
fn write_wait_with_bytes_in_flight_waits_for_the_transmit_completion() {
    let followup = local_followup_visit(
        &EngineProgress::Needs(WaitCondition::Io(Interest::WRITE)),
        true,
        false,
    );
    assert_eq!(followup, None);
}

#[test]
fn feed_wait_relies_on_the_feed_wake() {
    for progress in progress_shapes(WaitCondition::Feed) {
        for transmit_in_flight in [false, true] {
            for buffered_receive in [false, true] {
                assert_eq!(
                    local_followup_visit(&progress, transmit_in_flight, buffered_receive),
                    None
                );
            }
        }
    }
}

/// A budget yield has no wake source of its own: `Yield` clears the feed-wake
/// flag and may have written nothing.
#[test]
fn yield_and_state_transitions_always_get_a_local_visit() {
    for progress in [
        EngineProgress::Yield,
        EngineProgress::HandshakeComplete,
        EngineProgress::FeedOverrun,
    ] {
        assert_eq!(
            local_followup_visit(&progress, true, false),
            Some(Readiness::BOTH),
            "{progress:?}"
        );
    }
}

#[test]
fn closing_progress_is_never_revisited() {
    assert_eq!(
        local_followup_visit(&EngineProgress::PeerClosed, false, true),
        None
    );
}
