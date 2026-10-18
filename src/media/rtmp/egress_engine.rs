#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]
use bytes::Bytes;
use rml_rtmp::time::RtmpTimestamp;

use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::TrackConfig;

use super::egress_packets::{rtmp_video_packet_can_be_dropped, video_sequence_header};
use super::egress_payload_cache::{RtmpPayloadCache, SharedRtmpPayloadCache};
use super::flv::{FlvVideoPacketKind, classify_flv_video_packet};
use super::timestamps::{RtmpTimestampGuard, refreshed_video_sequence_header_timestamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RtmpMediaAction {
    Video {
        payload: Bytes,
        timestamp: RtmpTimestamp,
        can_be_dropped: bool,
    },
    Audio {
        payload: Bytes,
        timestamp: RtmpTimestamp,
    },
}

pub(crate) struct RtmpMediaEncoder {
    enhanced_hevc: bool,
    video_ready: bool,
    /// Ring configuration generation of the last video sequence header this
    /// output sent; 0 before any.
    announced_video_generation: u32,
    timestamp_guard: RtmpTimestampGuard,
    /// Raw → FLV conversions, shared with the other outputs on this shard
    /// (see `egress_payload_cache`); private until the shard shares its own.
    payloads: SharedRtmpPayloadCache,
}

impl RtmpMediaEncoder {
    pub(crate) fn new(enhanced_hevc: bool) -> Self {
        Self {
            enhanced_hevc,
            video_ready: false,
            announced_video_generation: 0,
            timestamp_guard: RtmpTimestampGuard::new(),
            payloads: RtmpPayloadCache::shared(),
        }
    }

    pub(crate) fn share_payload_cache(&mut self, payloads: SharedRtmpPayloadCache) {
        self.payloads = payloads;
    }

    /// Encode one ring packet. `video_config` is the ring's configuration for
    /// a Raw video keyframe's track (`RingBuffer::track_config_at`); a
    /// generation this output has not announced yet is sent as a sequence
    /// header before the keyframe.
    pub(crate) fn encode(
        &mut self,
        packet: &MediaPacket,
        video_config: Option<&TrackConfig>,
        actions: &mut Vec<RtmpMediaAction>,
    ) {
        match packet.media_type {
            MediaType::Video => self.encode_video(packet, video_config, actions),
            MediaType::Audio => self.encode_audio(packet, actions),
        }
    }

    /// The generation of the configuration the startup sequence header was
    /// built from (0 when it came from elsewhere or none was sent).
    pub(crate) fn set_startup_video_generation(&mut self, generation: u32) {
        self.announced_video_generation = generation;
    }

    pub(crate) fn video_ready(&self) -> bool {
        self.video_ready
    }

    fn encode_video(
        &mut self,
        packet: &MediaPacket,
        video_config: Option<&TrackConfig>,
        actions: &mut Vec<RtmpMediaAction>,
    ) {
        let mut timestamp = self.timestamp_guard.packet_timestamp(packet);
        let payload = match packet.format {
            PayloadFormat::Raw => {
                let converted = self
                    .payloads
                    .borrow_mut()
                    .convert(packet, self.enhanced_hevc);
                if !self.video_ready && !packet.is_keyframe {
                    return;
                }
                if packet.is_keyframe {
                    if let Some(config) = video_config
                        && config.generation != self.announced_video_generation
                        && let Some(sequence_header) =
                            video_sequence_header(self.enhanced_hevc, &config.bytes)
                    {
                        let header_timestamp = refreshed_video_sequence_header_timestamp(timestamp);
                        actions.push(RtmpMediaAction::Video {
                            payload: sequence_header,
                            timestamp: header_timestamp,
                            can_be_dropped: false,
                        });
                        if header_timestamp.value == timestamp.value {
                            timestamp = RtmpTimestamp::new(
                                self.timestamp_guard
                                    .enforce_ms(MediaType::Video, header_timestamp.value as i64)
                                    as u32,
                            );
                        }
                        self.announced_video_generation = config.generation;
                    }
                    self.video_ready = true;
                }
                if !self.video_ready {
                    return;
                }
                let Some(payload) = converted else {
                    return;
                };
                payload
            }
            PayloadFormat::Flv => {
                if !self.video_ready {
                    match classify_flv_video_packet(&packet.payload) {
                        Some(FlvVideoPacketKind::Interframe) | None if !packet.is_keyframe => {
                            return;
                        }
                        _ => self.video_ready = true,
                    }
                }
                packet.payload.clone()
            }
        };

        actions.push(RtmpMediaAction::Video {
            can_be_dropped: rtmp_video_packet_can_be_dropped(&payload, packet.is_keyframe),
            payload,
            timestamp,
        });
    }

    fn encode_audio(&mut self, packet: &MediaPacket, actions: &mut Vec<RtmpMediaAction>) {
        let payload = match packet.format {
            PayloadFormat::Raw => {
                let Some(payload) = self.payloads.borrow_mut().convert(packet, false) else {
                    return;
                };
                payload
            }
            PayloadFormat::Flv => packet.payload.clone(),
        };
        actions.push(RtmpMediaAction::Audio {
            payload,
            timestamp: self.timestamp_guard.packet_timestamp(packet),
        });
    }
}

#[cfg(test)]
#[allow(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::expect_used
)]
mod tests {
    use super::*;
    use crate::media::ring_buffer::RingBuffer;

    const SPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80];
    const PPS_A: &[u8] = &[0, 0, 0, 1, 0x68, 0xce, 0x06, 0xe2];
    const PPS_B: &[u8] = &[0, 0, 0, 1, 0x68, 0xce, 0x3c, 0x80];
    const IDR: &[u8] = &[0, 0, 0, 1, 0x65, 0x88];
    const P_SLICE: &[u8] = &[0, 0, 0, 1, 0x41, 0x9a, 0x01];

    fn video(payload: Bytes, keyframe: bool, format: PayloadFormat) -> MediaPacket {
        MediaPacket {
            media_type: MediaType::Video,
            format,
            is_keyframe: keyframe,
            track_index: 0,
            pts: 100,
            dts: 80,
            payload,
        }
    }

    fn raw_video(parts: &[&[u8]], keyframe: bool, dts: i64) -> MediaPacket {
        MediaPacket {
            pts: dts,
            dts,
            ..video(Bytes::from(parts.concat()), keyframe, PayloadFormat::Raw)
        }
    }

    fn is_avc_sequence_header(action: &RtmpMediaAction) -> bool {
        matches!(action, RtmpMediaAction::Video { payload, .. } if payload.starts_with(&[0x17, 0x00]))
    }

    /// Publishes `packets` through the producer path, then encodes the ring
    /// as an RTMP output leaf does: each Raw video keyframe with the ring's
    /// configuration for its sequence. Returns the actions per packet.
    fn encode_ring(
        encoder: &mut RtmpMediaEncoder,
        packets: Vec<MediaPacket>,
    ) -> Vec<Vec<RtmpMediaAction>> {
        let ring = RingBuffer::new(16);
        let mut batch = packets;
        ring.push_drained_batch_recording_config(&mut batch);
        (0..ring.get_write_idx())
            .map(|sequence| {
                let packet = ring.read_at(sequence).expect("retained packet");
                let config = packet
                    .is_keyframe
                    .then(|| {
                        ring.track_config_at(sequence, packet.media_type, packet.track_index)
                            .expect("retained sequence")
                    })
                    .flatten();
                let mut actions = Vec::new();
                encoder.encode(&packet, config.as_deref(), &mut actions);
                actions
            })
            .collect()
    }

    #[test]
    fn flv_interframe_waits_for_keyframe() {
        let mut encoder = RtmpMediaEncoder::new(false);
        let mut actions = Vec::new();

        encoder.encode(
            &video(
                Bytes::from_static(&[0x27, 1, 0, 0, 0]),
                false,
                PayloadFormat::Flv,
            ),
            None,
            &mut actions,
        );

        assert!(actions.is_empty());
    }

    #[test]
    fn raw_keyframe_emits_config_before_media() {
        let mut encoder = RtmpMediaEncoder::new(false);
        let per_packet = encode_ring(&mut encoder, vec![raw_video(&[SPS, PPS_A, IDR], true, 0)]);

        assert!(matches!(
            per_packet[0].as_slice(),
            [
                RtmpMediaAction::Video {
                    can_be_dropped: false,
                    ..
                },
                RtmpMediaAction::Video {
                    can_be_dropped: false,
                    ..
                },
            ]
        ));
        assert!(is_avc_sequence_header(&per_packet[0][0]));
    }

    /// A sequence header goes out exactly where the ring's configuration
    /// generation changes: not for a repeated header, not for a keyframe that
    /// relies on earlier parameter sets, but for a PPS-only change.
    #[test]
    fn sequence_headers_follow_ring_configuration_generations() {
        let mut encoder = RtmpMediaEncoder::new(false);
        let per_packet = encode_ring(
            &mut encoder,
            vec![
                raw_video(&[SPS, PPS_A, IDR], true, 0),
                raw_video(&[P_SLICE], false, 40),
                raw_video(&[SPS, PPS_A, IDR], true, 80),
                raw_video(&[IDR], true, 120),
                raw_video(&[SPS, PPS_B, IDR], true, 160),
                raw_video(&[P_SLICE], false, 200),
            ],
        );

        let headers: Vec<usize> = per_packet
            .iter()
            .enumerate()
            .filter(|(_, actions)| actions.iter().any(is_avc_sequence_header))
            .map(|(sequence, _)| sequence)
            .collect();
        assert_eq!(headers, [0, 4]);
        let RtmpMediaAction::Video { payload, .. } = &per_packet[4][0] else {
            unreachable!("video packets encode to video actions");
        };
        let expected = crate::media::codec::build_avcc_sequence_header(&[SPS, PPS_B].concat())
            .expect("valid parameter sets");
        assert_eq!(payload, &expected, "the header carries the new PPS");
        assert!(
            per_packet.iter().all(|actions| !actions.is_empty()),
            "every frame is still sent"
        );
    }

    #[test]
    fn startup_generation_suppresses_a_repeated_header() {
        let mut encoder = RtmpMediaEncoder::new(false);
        encoder.set_startup_video_generation(1);
        let per_packet = encode_ring(
            &mut encoder,
            vec![
                raw_video(&[SPS, PPS_A, IDR], true, 0),
                raw_video(&[SPS, PPS_B, IDR], true, 40),
            ],
        );

        assert!(!per_packet[0].iter().any(is_avc_sequence_header));
        assert!(is_avc_sequence_header(&per_packet[1][0]));
    }
}
