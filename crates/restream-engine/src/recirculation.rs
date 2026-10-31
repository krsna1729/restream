use std::sync::Arc;

use crate::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::egress::feed::{EgressFeed, FeedCursor, FeedRead, ReadBudget};
use crate::egress::journal::{FeedEpoch, RingFeed};
use crate::egress::visit::live_start_cursor;
use crate::engine::IngestRegistration;
use crate::engine::{EgressRegistration, MediaEngine};
use crate::input_gate::{InputForwardState, InputPacketBoundary, InputTimestampMapper};
use restream_core::{MediaPacket, MediaType};
use restream_sync::standby_gop::StandbyGopCache;
use restream_sync::{MEDIA_PULL_BURST_PACKETS, Reader, RingBuffer, VideoConfigFollower};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecirculationPublishOutcome {
    pub units: usize,
    pub bytes: usize,
}

#[derive(Debug, Default)]
pub struct RecirculationInputPublisher {
    timestamp_mapper: InputTimestampMapper,
    standby_gop: StandbyGopCache,
    video_config: VideoConfigFollower,
    /// Reused publication batch, split where the source's configuration
    /// changes.
    pending: Vec<MediaPacket>,
}

impl RecirculationInputPublisher {
    /// Republish `units`, read from `source` starting at `first_sequence`,
    /// into `ring`.
    pub fn publish(
        &mut self,
        source: &RingBuffer,
        first_sequence: usize,
        units: &[Arc<MediaPacket>],
        ring: &RingBuffer,
        registration: &IngestRegistration,
    ) -> RecirculationPublishOutcome {
        if units.is_empty() {
            return RecirculationPublishOutcome::default();
        }

        let mut packets = units.iter().map(|packet| (**packet).clone()).collect();
        self.forward_packets(&mut packets, source, first_sequence, ring, registration)
    }

    fn forward_packets(
        &mut self,
        packets: &mut Vec<MediaPacket>,
        source: &RingBuffer,
        first_sequence: usize,
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
        let last_sequence = first_sequence + packets.len() - 1;
        let (lease, sequenced) = if registration.gate.state() == InputForwardState::Active {
            let Some(lease) = registration.gate.try_enter(boundary) else {
                packets.clear();
                return RecirculationPublishOutcome::default();
            };
            (lease, true)
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
            (lease, false)
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
        let units = packets.len();
        if !sequenced {
            // The replay is the source's newest GOP: its latest configuration.
            self.video_config
                .follow(source, last_sequence, ring, &mut self.pending);
        }
        for (index, packet) in packets.drain(..).enumerate() {
            if sequenced && packet.media_type == MediaType::Video {
                self.video_config
                    .follow(source, first_sequence + index, ring, &mut self.pending);
            }
            self.pending.push(packet);
        }
        ring.push_drained_batch_capped(&mut self.pending);
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
                let first_sequence = next_cursor.next_sequence as usize - units.len();
                *driver.cursor = next_cursor;
                let outcome = driver.publisher.publish(
                    &driver.feed.current_ring(),
                    first_sequence,
                    &units,
                    driver.target_ring,
                    driver.input_registration,
                );
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
                *driver.cursor = live_start_cursor(driver.feed);
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

    use crate::input_gate::InputPacketGate;
    use restream_core::PayloadFormat;

    fn packet(media_type: MediaType, dts: i64, is_keyframe: bool) -> Arc<MediaPacket> {
        packet_with_payload(
            media_type,
            dts,
            is_keyframe,
            Bytes::from_static(&[1, 2, 3, 4]),
        )
    }

    fn packet_with_payload(
        media_type: MediaType,
        dts: i64,
        is_keyframe: bool,
        payload: Bytes,
    ) -> Arc<MediaPacket> {
        Arc::new(MediaPacket {
            media_type,
            format: PayloadFormat::Raw,
            is_keyframe,
            track_index: 0,
            pts: dts,
            dts,
            payload,
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
        let video = packet_with_payload(
            MediaType::Video,
            10,
            true,
            Bytes::from(vec![0x11, 0x12, 0x13, 0x14]),
        );
        let audio = packet_with_payload(
            MediaType::Audio,
            12,
            false,
            Bytes::from(vec![0x21, 0x22, 0x23, 0x24]),
        );

        let outcome = publisher.publish(
            &RingBuffer::new(1),
            0,
            &[video.clone(), audio.clone()],
            &ring,
            &registration,
        );

        assert_eq!(outcome, RecirculationPublishOutcome { units: 2, bytes: 8 });
        assert_eq!(registration.last_forwarded_dts.load(Ordering::Acquire), 12);
        let first = ring.read_at(0).unwrap();
        let second = ring.read_at(1).unwrap();
        assert_eq!(first.dts, 10);
        assert_eq!(second.dts, 12);
        assert_eq!(first.payload.as_ptr(), video.payload.as_ptr());
        assert_eq!(first.payload.len(), video.payload.len());
        assert_eq!(second.payload.as_ptr(), audio.payload.as_ptr());
        assert_eq!(second.payload.len(), audio.payload.len());
    }

    /// The target ring announces the source's configuration from the packet
    /// where the source changed it, not from the start of the batch.
    #[test]
    fn recirculation_publisher_copies_the_source_configuration_at_its_packet() {
        let source = RingBuffer::new(16);
        source.push(MediaPacket::clone(&packet(MediaType::Video, 0, false)));
        source.set_video_parameter_sets(vec![0, 0, 0, 1, 0x67, 0x42]);
        source.push(MediaPacket::clone(&packet(MediaType::Video, 40, true)));
        let units: Vec<_> = (0..2)
            .map(|sequence| source.read_at(sequence).unwrap())
            .collect();
        let ring = RingBuffer::new(16);
        let registration = registration(Arc::new(InputPacketGate::active()), i64::MIN);

        let outcome = RecirculationInputPublisher::default().publish(
            &source,
            0,
            &units,
            &ring,
            &registration,
        );

        assert_eq!(outcome.units, 2);
        let generations: Vec<u32> = (0..2)
            .map(|sequence| ring.frame_unit(sequence).unwrap().config_generation)
            .collect();
        assert_eq!(generations, [0, 1]);
        assert_eq!(
            ring.video_parameter_sets(),
            Some(vec![0, 0, 0, 1, 0x67, 0x42])
        );
    }

    #[test]
    fn recirculation_publisher_keeps_target_ring_bounded_when_reader_lags() {
        let ring = RingBuffer::new(2);
        let registration = registration(Arc::new(InputPacketGate::active()), i64::MIN);
        let mut publisher = RecirculationInputPublisher::default();

        let outcome = publisher.publish(
            &RingBuffer::new(1),
            0,
            &[
                packet(MediaType::Video, 10, true),
                packet(MediaType::Audio, 20, false),
                packet(MediaType::Video, 30, false),
                packet(MediaType::Audio, 40, false),
            ],
            &ring,
            &registration,
        );

        assert_eq!(
            outcome,
            RecirculationPublishOutcome {
                units: 4,
                bytes: 16
            }
        );
        assert_eq!(ring.get_write_idx(), 4);
        assert_eq!(ring.fill_and_capacity(), (2, 2));
        assert_eq!(ring.read_at(2).unwrap().dts, 30);
        assert_eq!(ring.read_at(3).unwrap().dts, 40);
    }

    #[test]
    fn recirculation_publisher_replays_standby_gop_on_promotion() {
        let ring = RingBuffer::new(16);
        let gate = Arc::new(InputPacketGate::standby());
        let registration = registration(gate.clone(), 100);
        let mut publisher = RecirculationInputPublisher::default();

        let standby = publisher.publish(
            &RingBuffer::new(1),
            0,
            &[
                packet(MediaType::Video, 10, true),
                packet(MediaType::Video, 20, false),
            ],
            &ring,
            &registration,
        );
        gate.arm_for_promotion();
        let promoted = publisher.publish(
            &RingBuffer::new(1),
            0,
            &[packet(MediaType::Audio, 30, false)],
            &ring,
            &registration,
        );

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

    /// An overrun with no keyframe left in the source window resumes at the
    /// live edge: the target ring gets no interframe between the gap and the
    /// next keyframe.
    #[tokio::test]
    async fn recirculation_forwards_no_interframe_after_a_resync_without_a_retained_keyframe() {
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
        let caught_up = |ring: &RingBuffer| {
            ring.reader_snapshots().iter().any(|reader| {
                reader.name == "pipeline_egress:out-pipeline" && reader.lag_slots == 0
            })
        };
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
            while target_ring.get_write_idx() < 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        // The recirculation falls a whole window behind (it cannot run while
        // this test does not yield): the keyframe is gone.
        for index in 0..source_ring.capacity() as i64 + 8 {
            source_ring.push((*packet(MediaType::Video, 20 + index, false)).clone());
        }
        timeout(Duration::from_secs(1), async {
            while !caught_up(&source_ring) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        assert_eq!(
            target_ring.get_write_idx(),
            1,
            "nothing of the lapped window"
        );

        source_ring.push((*packet(MediaType::Video, 5_000, false)).clone());
        source_ring.push((*packet(MediaType::Video, 5_040, true)).clone());
        source_ring.push((*packet(MediaType::Video, 5_080, false)).clone());
        timeout(Duration::from_secs(1), async {
            while !caught_up(&source_ring) || target_ring.get_write_idx() < 3 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let forwarded: Vec<bool> = (1..target_ring.get_write_idx())
            .map(|sequence| target_ring.read_at(sequence).unwrap().is_keyframe)
            .collect();
        assert_eq!(
            forwarded,
            [true, false],
            "the keyframe is the first unit after the gap"
        );

        egress_registration.cancel_token.cancel();
        timeout(Duration::from_secs(1), task)
            .await
            .unwrap()
            .unwrap();
        engine.unregister_egress("out-pipeline").await;
    }
}
