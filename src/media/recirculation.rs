use std::sync::Arc;

use crate::media::engine::IngestRegistration;
use crate::media::input_gate::{InputForwardState, InputPacketBoundary, InputTimestampMapper};
use crate::media::packet::{MediaPacket, MediaType};
use crate::media::ring_buffer::RingBuffer;
use crate::media::standby_gop::StandbyGopCache;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecirculationPublishOutcome {
    pub units: usize,
    pub bytes: usize,
}

#[derive(Debug, Default)]
pub struct RecirculationInputPublisher {
    timestamp_mapper: InputTimestampMapper,
    standby_gop: StandbyGopCache,
}

impl RecirculationInputPublisher {
    pub fn publish(
        &mut self,
        units: &[Arc<MediaPacket>],
        ring: &RingBuffer,
        registration: &IngestRegistration,
    ) -> RecirculationPublishOutcome {
        if units.is_empty() {
            return RecirculationPublishOutcome::default();
        }

        let mut packets = units.iter().map(|packet| (**packet).clone()).collect();
        self.forward_packets(&mut packets, ring, registration)
    }

    fn forward_packets(
        &mut self,
        packets: &mut Vec<MediaPacket>,
        ring: &RingBuffer,
        registration: &IngestRegistration,
    ) -> RecirculationPublishOutcome {
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
                return RecirculationPublishOutcome::default();
            };
            lease
        } else {
            for packet in packets.drain(..) {
                self.standby_gop.push(packet);
            }
            let replay_boundary = if self.standby_gop.is_replay_ready() {
                InputPacketBoundary::ReplayReady
            } else {
                InputPacketBoundary::Other
            };
            let Some(lease) = registration.gate.try_enter(replay_boundary) else {
                return RecirculationPublishOutcome::default();
            };
            *packets = self.standby_gop.take_replay();
            lease
        };

        for (index, packet) in packets.iter_mut().enumerate() {
            self.timestamp_mapper.map_packet(
                packet,
                lease.activated() && index == 0,
                &registration.last_forwarded_dts,
            );
        }
        if let Some(last) = packets.iter().max_by_key(|packet| packet.dts) {
            InputTimestampMapper::record_forwarded(last, &registration.last_forwarded_dts);
        }
        let bytes = packets.iter().map(|packet| packet.payload.len()).sum();
        let units = ring.push_drained_batch_capped(packets);
        drop(lease);

        RecirculationPublishOutcome { units, bytes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};

    use arc_swap::ArcSwapOption;
    use bytes::Bytes;
    use tokio_util::sync::CancellationToken;

    use crate::media::input_gate::InputPacketGate;
    use crate::media::packet::PayloadFormat;

    fn packet(media_type: MediaType, dts: i64, is_keyframe: bool) -> Arc<MediaPacket> {
        Arc::new(MediaPacket {
            media_type,
            format: PayloadFormat::Raw,
            is_keyframe,
            track_index: 0,
            pts: dts,
            dts,
            payload: Bytes::from_static(&[1, 2, 3, 4]),
        })
    }

    fn registration(gate: Arc<InputPacketGate>, last_dts: i64) -> IngestRegistration {
        IngestRegistration {
            cancel_token: CancellationToken::new(),
            attempt_id: 1,
            input_id: "recirculated-input".to_string(),
            gate,
            last_forwarded_dts: Arc::new(AtomicI64::new(last_dts)),
            preview_ring: Arc::new(ArcSwapOption::empty()),
        }
    }

    #[test]
    fn recirculation_publisher_forwards_active_input_batch() {
        let ring = RingBuffer::new(16);
        let registration = registration(Arc::new(InputPacketGate::active()), i64::MIN);
        let mut publisher = RecirculationInputPublisher::default();

        let outcome = publisher.publish(
            &[
                packet(MediaType::Video, 10, true),
                packet(MediaType::Audio, 12, false),
            ],
            &ring,
            &registration,
        );

        assert_eq!(outcome, RecirculationPublishOutcome { units: 2, bytes: 8 });
        assert_eq!(registration.last_forwarded_dts.load(Ordering::Acquire), 12);
        let first = ring.read_at(0).unwrap();
        let second = ring.read_at(1).unwrap();
        assert_eq!(first.dts, 10);
        assert_eq!(second.dts, 12);
    }

    #[test]
    fn recirculation_publisher_replays_standby_gop_on_promotion() {
        let ring = RingBuffer::new(16);
        let gate = Arc::new(InputPacketGate::standby());
        let registration = registration(gate.clone(), 100);
        let mut publisher = RecirculationInputPublisher::default();

        let standby = publisher.publish(
            &[
                packet(MediaType::Video, 10, true),
                packet(MediaType::Video, 20, false),
            ],
            &ring,
            &registration,
        );
        gate.arm_for_promotion();
        let promoted =
            publisher.publish(&[packet(MediaType::Audio, 30, false)], &ring, &registration);

        assert_eq!(standby, RecirculationPublishOutcome::default());
        assert_eq!(
            promoted,
            RecirculationPublishOutcome {
                units: 3,
                bytes: 12
            }
        );
        let first = ring.read_at(0).unwrap();
        let second = ring.read_at(1).unwrap();
        let third = ring.read_at(2).unwrap();
        assert!(first.is_keyframe);
        assert_eq!(first.dts, 101);
        assert_eq!(second.dts, 111);
        assert_eq!(third.dts, 121);
        assert_eq!(registration.last_forwarded_dts.load(Ordering::Acquire), 121);
    }
}
