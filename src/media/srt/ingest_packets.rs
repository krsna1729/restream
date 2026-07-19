use std::sync::{Arc, Mutex};

use crate::media::engine::IngestRegistration;
use crate::media::input_gate::{InputForwardState, InputPacketBoundary, InputTimestampMapper};
use crate::media::ring_buffer::{MediaPacket, MediaType, RingBuffer};
use crate::media::standby_gop::StandbyGopCache;

pub(super) fn forward_ingest_packets(
    packets: &mut Vec<MediaPacket>,
    ring: &RingBuffer,
    registration: &IngestRegistration,
    timestamp_mapper: &mut InputTimestampMapper,
    standby_gop: &mut StandbyGopCache,
    keyframe_times: Option<&Arc<Mutex<Vec<i64>>>>,
) {
    if let Some(preview_ring) = registration.preview_ring.load_full() {
        update_parameter_sets(&preview_ring, packets);
        preview_ring.push_batch(packets.iter().cloned());
    }

    let first_keyframe = packets
        .iter()
        .position(|packet| packet.media_type == MediaType::Video && packet.is_keyframe);
    let boundary = if first_keyframe.is_some() {
        InputPacketBoundary::VideoKeyframe
    } else {
        InputPacketBoundary::Other
    };
    let lease = if registration.gate.state() == InputForwardState::Active {
        let Some(lease) = registration.gate.try_enter(boundary) else {
            packets.clear();
            return;
        };
        lease
    } else {
        for packet in packets.drain(..) {
            standby_gop.push(packet);
        }
        let replay_boundary = if standby_gop.is_replay_ready() {
            InputPacketBoundary::ReplayReady
        } else {
            InputPacketBoundary::Other
        };
        let Some(lease) = registration.gate.try_enter(replay_boundary) else {
            return;
        };
        *packets = standby_gop.take_replay();
        lease
    };

    for (index, packet) in packets.iter_mut().enumerate() {
        timestamp_mapper.map_packet(
            packet,
            lease.activated() && index == 0,
            &registration.last_forwarded_dts,
        );
    }
    update_parameter_sets(ring, packets);
    if let Some(keyframe_times) = keyframe_times {
        record_keyframes(keyframe_times, packets);
    }
    if let Some(last) = packets.iter().max_by_key(|packet| packet.dts) {
        InputTimestampMapper::record_forwarded(last, &registration.last_forwarded_dts);
    }
    ring.push_drained_batch_capped(packets);
    drop(lease);
}

fn update_parameter_sets(ring: &RingBuffer, packets: &[MediaPacket]) {
    for packet in packets {
        if packet.media_type == MediaType::Video
            && let Some(parameter_sets) =
                crate::media::codec::annexb_parameter_sets(&packet.payload)
        {
            ring.set_video_parameter_sets(parameter_sets);
        }
    }
}

fn record_keyframes(keyframe_times: &Arc<Mutex<Vec<i64>>>, packets: &[MediaPacket]) {
    for packet in packets {
        if packet.media_type != MediaType::Video || !packet.is_keyframe {
            continue;
        }
        let mut times = keyframe_times
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        times.push(packet.pts);
        if times.len() > 30 {
            times.remove(0);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicI64, Ordering};

    use arc_swap::ArcSwapOption;
    use bytes::Bytes;
    use tokio_util::sync::CancellationToken;

    use super::*;
    use crate::media::input_gate::InputPacketGate;
    use crate::media::ring_buffer::PayloadFormat;

    fn packet(media_type: MediaType, dts: i64, is_keyframe: bool) -> MediaPacket {
        MediaPacket {
            media_type,
            format: PayloadFormat::Raw,
            is_keyframe,
            track_index: 0,
            pts: dts,
            dts,
            payload: Bytes::from_static(&[1, 2, 3]),
        }
    }

    fn registration(gate: Arc<InputPacketGate>, last_dts: i64) -> IngestRegistration {
        IngestRegistration {
            cancel_token: CancellationToken::new(),
            attempt_id: 1,
            input_id: "standby".to_string(),
            gate,
            last_forwarded_dts: Arc::new(AtomicI64::new(last_dts)),
            preview_ring: Arc::new(ArcSwapOption::empty()),
        }
    }

    #[test]
    fn promotion_replays_cached_gop_before_the_next_live_keyframe() {
        let gate = Arc::new(InputPacketGate::standby());
        let registration = registration(gate.clone(), 1_000);
        let ring = RingBuffer::new(16);
        let mut mapper = InputTimestampMapper::default();
        let mut standby_gop = StandbyGopCache::default();
        let mut packets = vec![
            packet(MediaType::Video, 100, true),
            packet(MediaType::Video, 110, false),
        ];

        forward_ingest_packets(
            &mut packets,
            &ring,
            &registration,
            &mut mapper,
            &mut standby_gop,
            None,
        );
        assert_eq!(ring.get_write_idx(), 0);
        assert!(standby_gop.is_replay_ready());

        gate.arm_for_promotion();
        packets.push(packet(MediaType::Audio, 115, false));
        forward_ingest_packets(
            &mut packets,
            &ring,
            &registration,
            &mut mapper,
            &mut standby_gop,
            None,
        );

        let forwarded = (0..ring.get_write_idx())
            .map(|index| ring.read_at(index).expect("forwarded packet"))
            .collect::<Vec<_>>();
        assert_eq!(forwarded.len(), 3);
        assert!(forwarded[0].is_keyframe);
        assert_eq!(forwarded[0].dts, 1_001);
        assert_eq!(forwarded[1].dts, 1_011);
        assert_eq!(forwarded[2].dts, 1_016);
        assert_eq!(
            registration.last_forwarded_dts.load(Ordering::Acquire),
            1_016
        );
        assert!(!standby_gop.is_replay_ready());
    }
}
