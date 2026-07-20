use bytes::Bytes;
use proptest::prelude::*;
use restream::media::input_gate::{
    InputForwardState, InputPacketBoundary, InputPacketGate, InputTimestampMapper,
};
use restream::media::packet::{MediaPacket, MediaType, PayloadFormat};
use std::sync::atomic::{AtomicI64, Ordering};

fn packet(dts: i64, composition_offset: i64) -> MediaPacket {
    MediaPacket {
        media_type: MediaType::Video,
        track_index: 0,
        pts: dts.saturating_add(composition_offset),
        dts,
        is_keyframe: true,
        format: PayloadFormat::Raw,
        payload: Bytes::new(),
    }
}

#[test]
fn promoted_input_waits_for_keyframe_before_forwarding() {
    let gate = InputPacketGate::standby();
    gate.arm_for_promotion();

    let audio = gate.try_enter(InputPacketBoundary::Other);
    let inter_frame = gate.try_enter(InputPacketBoundary::Other);

    assert!(audio.is_none());
    assert!(inter_frame.is_none());
    let activation = gate
        .try_enter(InputPacketBoundary::VideoKeyframe)
        .expect("keyframe activates promoted input");
    assert!(activation.activated());
    drop(activation);
    assert!(gate.try_enter(InputPacketBoundary::Other).is_some());
    assert_eq!(gate.state(), InputForwardState::Active);
}

#[test]
fn promoted_input_can_activate_from_a_buffered_gop() {
    let gate = InputPacketGate::standby();
    gate.arm_for_promotion();

    let activation = gate
        .try_enter(InputPacketBoundary::ReplayReady)
        .expect("buffered GOP activates promoted input");

    assert!(activation.activated());
    drop(activation);
    assert_eq!(gate.state(), InputForwardState::Active);
}

#[tokio::test]
async fn demotion_rejects_new_packets_after_existing_writer_drains() {
    let gate = InputPacketGate::active();
    let lease = gate
        .try_enter(InputPacketBoundary::Other)
        .expect("active lease");

    gate.demote();
    drop(lease);
    gate.wait_until_idle().await;

    assert!(gate.try_enter(InputPacketBoundary::Other).is_none());
    assert_eq!(gate.state(), InputForwardState::Standby);
}

#[test]
fn promoted_timeline_starts_after_previous_writer_and_preserves_composition_offset() {
    let last_forwarded = AtomicI64::new(8_000);
    let mut mapper = InputTimestampMapper::default();
    let mut promoted = packet(120, 40);

    mapper.map_packet(&mut promoted, true, &last_forwarded);
    InputTimestampMapper::record_forwarded(&promoted, &last_forwarded);

    assert_eq!(promoted.dts, 8_001);
    assert_eq!(promoted.pts - promoted.dts, 40);
    assert_eq!(last_forwarded.load(Ordering::Acquire), 8_001);
}

#[test]
fn repeated_promotion_rebases_an_initialized_timestamp_mapper() {
    let last_forwarded = AtomicI64::new(8_000);
    let mut mapper = InputTimestampMapper::default();
    let mut first_active_packet = packet(120, 40);
    mapper.map_packet(&mut first_active_packet, false, &last_forwarded);

    let mut promoted_again = packet(200, -20);
    mapper.map_packet(&mut promoted_again, true, &last_forwarded);

    assert_eq!(promoted_again.dts, 8_001);
    assert_eq!(promoted_again.pts - promoted_again.dts, -20);
}

proptest! {
    #[test]
    fn gate_matches_sequential_selection_model(
        operations in prop::collection::vec((0u8..4, 0u8..3), 1..128)
    ) {
        let gate = InputPacketGate::standby();
        let mut expected = InputForwardState::Standby;

        for (operation, boundary_kind) in operations {
            match operation {
                0 => {
                    gate.arm_for_promotion();
                    expected = InputForwardState::AwaitingKeyframe;
                }
                1 => {
                    gate.demote();
                    expected = InputForwardState::Standby;
                }
                2 => {
                    gate.activate();
                    expected = InputForwardState::Active;
                }
                3 => {
                    let boundary = match boundary_kind {
                        0 => InputPacketBoundary::Other,
                        1 => InputPacketBoundary::VideoKeyframe,
                        _ => InputPacketBoundary::ReplayReady,
                    };
                    let entered = gate.try_enter(boundary).is_some();
                    let expected_entered = match expected {
                        InputForwardState::Standby => false,
                        InputForwardState::AwaitingKeyframe
                            if boundary != InputPacketBoundary::Other =>
                        {
                            expected = InputForwardState::Active;
                            true
                        }
                        InputForwardState::AwaitingKeyframe => false,
                        InputForwardState::Active => true,
                    };
                    prop_assert_eq!(entered, expected_entered);
                }
                _ => unreachable!(),
            }

            prop_assert_eq!(gate.state(), expected);
        }
    }

    #[test]
    fn promoted_timestamp_mapping_is_monotonic_and_preserves_cts(
        previous in -1_000_000i64..1_000_000,
        raw_dts in -1_000_000i64..1_000_000,
        composition_offset in -10_000i64..10_000,
    ) {
        let last_forwarded = AtomicI64::new(previous);
        let mut mapper = InputTimestampMapper::default();
        let mut promoted = packet(raw_dts, composition_offset);

        mapper.map_packet(&mut promoted, true, &last_forwarded);

        prop_assert_eq!(promoted.dts, previous.saturating_add(1));
        prop_assert_eq!(promoted.pts.saturating_sub(promoted.dts), composition_offset);
    }
}
