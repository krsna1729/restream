use std::sync::Arc;

use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::media::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::media::egress::journal::{FeedEpoch, RingFeed};
use crate::media::engine::IngestRegistration;
use crate::media::engine::{EgressRegistration, MediaEngine};
use crate::media::input_gate::{InputForwardState, InputPacketBoundary, InputTimestampMapper};
use crate::media::packet::{MediaPacket, MediaType};
use crate::media::ring_buffer::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer};
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

pub async fn start_pipeline_recirculation(
    output_id: String,
    source_ring: Arc<RingBuffer>,
    target_pipeline_id: String,
    target_input_id: String,
    engine: Arc<MediaEngine>,
    egress_registration: EgressRegistration,
) {
    let Some(input_registration) = engine
        .try_register_pipeline_input_attempt(
            &target_pipeline_id,
            &target_input_id,
            &format!("pipeline:{output_id}"),
            "pipeline",
            false,
        )
        .await
    else {
        engine
            .record_egress_error_if_current(
                &output_id,
                &egress_registration,
                "recirculation_input_claim",
                format!("target input {target_pipeline_id}/{target_input_id} is already active"),
            )
            .await;
        return;
    };

    let target_ring = engine.get_or_create_pipeline(&target_pipeline_id).await;
    let feed = RingFeed::new(source_ring.clone(), Arc::new(FeedEpoch::new()));
    let mut cursor = FeedCursor::new(feed.epoch(), feed.head_sequence());
    let mut publisher = RecirculationInputPublisher::default();
    let mut wake_reader = Reader::new_live(format!("pipeline_egress:{output_id}"), source_ring);

    engine
        .update_egress_target_addr_if_current(
            &output_id,
            &egress_registration,
            format!("pipeline://{target_pipeline_id}/{target_input_id}"),
        )
        .await;

    loop {
        tokio::select! {
            _ = egress_registration.cancel_token.cancelled() => break,
            _ = input_registration.cancel_token.cancelled() => break,
            _ = wake_reader.wait_for_data() => {
                drive_recirculation_until_blocked(
                    RecirculationDriver {
                        output_id: &output_id,
                        egress_registration: &egress_registration,
                        engine: &engine,
                        feed: &feed,
                        target_ring: &target_ring,
                        input_registration: &input_registration,
                        cursor: &mut cursor,
                        publisher: &mut publisher,
                    }
                )
                .await;
                wake_reader.sync_read_idx(cursor.next_sequence as usize);
            }
        }
    }

    engine
        .unregister_ingest_if_current(&target_pipeline_id, &input_registration)
        .await;
}

struct RecirculationDriver<'a> {
    output_id: &'a str,
    egress_registration: &'a EgressRegistration,
    engine: &'a MediaEngine,
    feed: &'a RingFeed,
    target_ring: &'a RingBuffer,
    input_registration: &'a IngestRegistration,
    cursor: &'a mut FeedCursor,
    publisher: &'a mut RecirculationInputPublisher,
}

async fn drive_recirculation_until_blocked(driver: RecirculationDriver<'_>) {
    loop {
        match driver.feed.read_from(
            *driver.cursor,
            ReadBudget::new(MEDIA_PULL_BURST_PACKETS, MEDIA_TS_BATCH_TARGET_BYTES),
        ) {
            FeedRead::Units { units, next_cursor } => {
                *driver.cursor = next_cursor;
                let outcome =
                    driver
                        .publisher
                        .publish(&units, driver.target_ring, driver.input_registration);
                if outcome.units == 0 {
                    continue;
                }
                if !driver
                    .engine
                    .record_egress_progress_if_current(
                        driver.output_id,
                        driver.egress_registration,
                        outcome.bytes as u64,
                    )
                    .await
                {
                    break;
                }
            }
            FeedRead::Empty => break,
            FeedRead::Overrun { .. } | FeedRead::EpochMismatch { .. } => {
                if let Some(sync_cursor) = driver.feed.latest_sync_point() {
                    *driver.cursor = sync_cursor;
                } else {
                    *driver.cursor =
                        FeedCursor::new(driver.feed.epoch(), driver.feed.head_sequence());
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicI64, Ordering};
    use std::time::Duration;

    use arc_swap::ArcSwapOption;
    use bytes::Bytes;
    use tokio::time::timeout;
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

    #[tokio::test]
    async fn start_pipeline_recirculation_claims_target_input_and_publishes_after_selection() {
        let engine = Arc::new(MediaEngine::new());
        let source_ring = engine.get_or_create_pipeline("pipe-source").await;
        let target_ring = engine.get_or_create_pipeline("pipe-target").await;
        let egress_registration = engine
            .register_egress_attempt(
                "out-pipeline",
                "pipe-source",
                "pipeline://pipe-target/input-backup",
                None,
            )
            .await;

        let task = tokio::spawn(start_pipeline_recirculation(
            "out-pipeline".to_string(),
            source_ring.clone(),
            "pipe-target".to_string(),
            "input-backup".to_string(),
            engine.clone(),
            egress_registration.clone(),
        ));
        timeout(Duration::from_secs(1), async {
            while engine.connected_input_count("pipe-target").await == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert!(
            engine
                .select_pipeline_input("pipe-target", "input-backup")
                .await
        );

        source_ring.push((*packet(MediaType::Video, 10, true)).clone());
        timeout(Duration::from_secs(1), async {
            while target_ring.get_write_idx() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let forwarded = target_ring.read_at(0).unwrap();
        assert!(forwarded.is_keyframe);
        assert_eq!(forwarded.dts, 10);
        assert_eq!(engine.egress_bytes("out-pipeline").await, 4);

        egress_registration.cancel_token.cancel();
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(engine.connected_input_count("pipe-target").await, 0);
        engine.unregister_egress("out-pipeline").await;
    }
}
