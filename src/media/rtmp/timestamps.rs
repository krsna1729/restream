use rml_rtmp::time::RtmpTimestamp;

use crate::media::packet::{MediaPacket, MediaType};

pub(super) struct RtmpTimestampGuard {
    last_video_ms: i64,
    last_audio_ms: i64,
}

impl RtmpTimestampGuard {
    pub(super) fn new() -> Self {
        Self {
            last_video_ms: i64::MIN,
            last_audio_ms: i64::MIN,
        }
    }

    pub(super) fn packet_timestamp(&mut self, packet: &MediaPacket) -> RtmpTimestamp {
        let timestamp_ms = match packet.media_type {
            MediaType::Video => packet.dts,
            MediaType::Audio => packet.pts,
        };
        RtmpTimestamp::new(self.enforce_ms(packet.media_type, timestamp_ms) as u32)
    }

    pub(super) fn enforce_ms(&mut self, media_type: MediaType, timestamp_ms: i64) -> i64 {
        let mut timestamp_ms = timestamp_ms.clamp(0, u32::MAX as i64);
        let slot = match media_type {
            MediaType::Video => &mut self.last_video_ms,
            MediaType::Audio => &mut self.last_audio_ms,
        };
        if timestamp_ms <= *slot {
            timestamp_ms = (*slot + 1).min(u32::MAX as i64);
        }
        *slot = timestamp_ms;
        timestamp_ms
    }
}

pub(super) fn refreshed_video_sequence_header_timestamp(packet_ts: RtmpTimestamp) -> RtmpTimestamp {
    // Startup sequence headers are sent before media at timestamp 0. Refreshes
    // happen in-band immediately ahead of a keyframe, so prefer the preceding
    // millisecond when possible to avoid duplicate DTS for the config packet
    // and the following keyframe on downstream remuxers.
    RtmpTimestamp::new(packet_ts.value.saturating_sub(1))
}
