use bytes::Bytes;
use rml_rtmp::time::RtmpTimestamp;

use crate::media::codec;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};

use super::egress_packets::{
    cache_h264_parameter_sets, rtmp_video_packet_can_be_dropped, video_sequence_header_for_keyframe,
};
use super::enhanced::cache_hevc_parameter_sets;
use super::flv::{FlvVideoPacketKind, classify_flv_video_packet};
use super::timestamps::{RtmpTimestampGuard, refreshed_video_sequence_header_timestamp};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RtmpMediaAction {
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

pub(super) struct RtmpMediaEncoder {
    enhanced_hevc: bool,
    video_ready: bool,
    raw_parameter_sets: Vec<u8>,
    last_video_config: Option<Vec<u8>>,
    timestamp_guard: RtmpTimestampGuard,
    video_buffer: Vec<u8>,
    audio_buffer: Vec<u8>,
}

impl RtmpMediaEncoder {
    pub(super) fn new(enhanced_hevc: bool, raw_parameter_sets: Vec<u8>) -> Self {
        Self {
            enhanced_hevc,
            video_ready: false,
            raw_parameter_sets,
            last_video_config: None,
            timestamp_guard: RtmpTimestampGuard::new(),
            video_buffer: Vec::new(),
            audio_buffer: Vec::new(),
        }
    }

    pub(super) fn encode(&mut self, packet: &MediaPacket, actions: &mut Vec<RtmpMediaAction>) {
        match packet.media_type {
            MediaType::Video => self.encode_video(packet, actions),
            MediaType::Audio => self.encode_audio(packet, actions),
        }
    }

    pub(super) fn set_startup_video_config(&mut self, config: Option<Vec<u8>>) {
        self.last_video_config = config;
    }

    pub(super) fn video_ready(&self) -> bool {
        self.video_ready
    }

    fn encode_video(&mut self, packet: &MediaPacket, actions: &mut Vec<RtmpMediaAction>) {
        let mut timestamp = self.timestamp_guard.packet_timestamp(packet);
        let payload = match packet.format {
            PayloadFormat::Raw => {
                if self.enhanced_hevc {
                    cache_hevc_parameter_sets(&packet.payload, &mut self.raw_parameter_sets);
                } else {
                    cache_h264_parameter_sets(&packet.payload, &mut self.raw_parameter_sets);
                }
                if !self.video_ready && !packet.is_keyframe {
                    return;
                }
                if packet.is_keyframe
                    && let Some((sequence_header, config)) = video_sequence_header_for_keyframe(
                        self.enhanced_hevc,
                        &packet.payload,
                        &self.raw_parameter_sets,
                    )
                    && self.last_video_config.as_ref() != config.as_ref()
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
                    self.video_ready = true;
                    self.last_video_config = config;
                } else if packet.is_keyframe {
                    self.video_ready = true;
                }
                if !self.video_ready {
                    return;
                }
                let composition = (packet.pts - packet.dts).clamp(-8_388_608, 8_388_607) as i32;
                let encoded = if self.enhanced_hevc {
                    codec::hevc_video_for_enhanced_rtmp_with_composition_into(
                        &packet.payload,
                        packet.is_keyframe,
                        composition,
                        &mut self.video_buffer,
                    )
                } else {
                    codec::video_for_rtmp_with_composition_into(
                        &packet.payload,
                        packet.is_keyframe,
                        composition,
                        &mut self.video_buffer,
                    )
                };
                if !encoded {
                    return;
                }
                Bytes::copy_from_slice(&self.video_buffer)
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
                codec::audio_for_rtmp_into(&packet.payload, &mut self.audio_buffer);
                Bytes::copy_from_slice(&self.audio_buffer)
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
mod tests {
    use super::*;

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

    #[test]
    fn flv_interframe_waits_for_keyframe() {
        let mut encoder = RtmpMediaEncoder::new(false, Vec::new());
        let mut actions = Vec::new();

        encoder.encode(
            &video(
                Bytes::from_static(&[0x27, 1, 0, 0, 0]),
                false,
                PayloadFormat::Flv,
            ),
            &mut actions,
        );

        assert!(actions.is_empty());
    }

    #[test]
    fn raw_keyframe_emits_config_before_media() {
        let mut encoder = RtmpMediaEncoder::new(false, Vec::new());
        let mut actions = Vec::new();
        let payload = Bytes::from_static(&[
            0, 0, 0, 1, 0x67, 0x42, 0, 0x1e, 0xf4, 0x05, 1, 0xec, 0x80, 0, 0, 0, 1, 0x68, 0xce,
            0x06, 0xe2, 0, 0, 0, 1, 0x65, 0x88,
        ]);

        encoder.encode(&video(payload, true, PayloadFormat::Raw), &mut actions);

        assert_eq!(actions.len(), 2);
        assert!(matches!(
            actions[0],
            RtmpMediaAction::Video {
                can_be_dropped: false,
                ..
            }
        ));
        assert!(matches!(
            actions[1],
            RtmpMediaAction::Video {
                can_be_dropped: false,
                ..
            }
        ));
    }
}
