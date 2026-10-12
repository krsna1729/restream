//! Timestamp continuity across file-ingest passes: each replayed pass is
//! shifted so every stream's timestamps keep increasing.

use crate::media::packet::MediaPacket;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct ContinuousTimestampState {
    offset_ms: i64,
    last_timestamp_ms_by_stream: HashMap<u64, i64>,
}

impl ContinuousTimestampState {
    fn stream_key(packet: &MediaPacket) -> u64 {
        ((packet.media_type as u64) << 32) | u64::from(packet.track_index)
    }

    fn continuity_timestamp_ms(packet: &MediaPacket) -> i64 {
        if packet.dts >= 0 {
            packet.dts
        } else {
            packet.pts
        }
    }

    pub(super) fn apply(&mut self, packet: &mut MediaPacket) {
        let stream_key = Self::stream_key(packet);
        let raw_timestamp_ms = Self::continuity_timestamp_ms(packet);
        if let Some(last_timestamp_ms) = self.last_timestamp_ms_by_stream.get(&stream_key).copied()
        {
            let adjusted_timestamp_ms = raw_timestamp_ms.saturating_add(self.offset_ms);
            if adjusted_timestamp_ms <= last_timestamp_ms {
                self.offset_ms = last_timestamp_ms
                    .saturating_add(1)
                    .saturating_sub(raw_timestamp_ms);
            }
        }

        packet.pts = packet.pts.saturating_add(self.offset_ms);
        packet.dts = packet.dts.saturating_add(self.offset_ms);
        let adjusted_timestamp_ms = Self::continuity_timestamp_ms(packet);
        self.last_timestamp_ms_by_stream.insert(
            stream_key,
            self.last_timestamp_ms_by_stream
                .get(&stream_key)
                .copied()
                .map_or(adjusted_timestamp_ms, |current| {
                    current.max(adjusted_timestamp_ms)
                }),
        );
    }
}

#[cfg(test)]
mod tests {
    use super::ContinuousTimestampState;
    use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
    use bytes::Bytes;

    #[test]
    fn continuous_timestamp_state_offsets_replayed_subprocess_packets() {
        let mut timestamps = ContinuousTimestampState::default();

        let mut first = test_packet(MediaType::Video, 0, 0, 0);
        let mut second = test_packet(MediaType::Video, 0, 40, 40);
        timestamps.apply(&mut first);
        timestamps.apply(&mut second);

        let mut replayed_first = test_packet(MediaType::Video, 0, 0, 0);
        let mut replayed_second = test_packet(MediaType::Video, 0, 40, 40);
        timestamps.apply(&mut replayed_first);
        timestamps.apply(&mut replayed_second);

        assert_eq!(first.pts, 0);
        assert_eq!(second.pts, 40);
        assert_eq!(replayed_first.pts, 41);
        assert_eq!(replayed_first.dts, 41);
        assert_eq!(replayed_second.pts, 81);
        assert_eq!(replayed_second.dts, 81);
    }

    #[test]
    fn continuous_timestamp_state_preserves_interleaved_audio_video_timestamps() {
        let mut timestamps = ContinuousTimestampState::default();

        let mut video0 = test_packet(MediaType::Video, 0, 0, 0);
        let mut audio0 = test_packet(MediaType::Audio, 0, 0, 0);
        let mut audio1 = test_packet(MediaType::Audio, 0, 21, 21);
        let mut video1 = test_packet(MediaType::Video, 0, 33, 33);

        timestamps.apply(&mut video0);
        timestamps.apply(&mut audio0);
        timestamps.apply(&mut audio1);
        timestamps.apply(&mut video1);

        assert_eq!(video0.pts, 0);
        assert_eq!(audio0.pts, 0);
        assert_eq!(audio1.pts, 21);
        assert_eq!(video1.pts, 33);
    }

    #[test]
    fn continuous_timestamp_state_uses_dts_for_reordered_video_packets() {
        let mut timestamps = ContinuousTimestampState::default();

        let mut anchor = test_packet(MediaType::Video, 0, 0, 0);
        let mut reordered_p = test_packet(MediaType::Video, 0, 100, 33);
        let mut reordered_b = test_packet(MediaType::Video, 0, 66, 66);

        timestamps.apply(&mut anchor);
        timestamps.apply(&mut reordered_p);
        timestamps.apply(&mut reordered_b);

        assert_eq!(anchor.pts, 0);
        assert_eq!(reordered_p.pts, 100);
        assert_eq!(reordered_p.dts, 33);
        assert_eq!(reordered_b.pts, 66);
        assert_eq!(reordered_b.dts, 66);
    }

    fn test_packet(media_type: MediaType, track_index: u32, pts: i64, dts: i64) -> MediaPacket {
        MediaPacket {
            media_type,
            format: PayloadFormat::Raw,
            is_keyframe: false,
            track_index,
            pts,
            dts,
            payload: Bytes::from_static(b"packet"),
        }
    }
}
