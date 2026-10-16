//! Timestamp continuity across TS source passes: each replayed file pass,
//! and each stream after an HLS discontinuity, is shifted so every stream's
//! timestamps keep increasing.

use crate::media::packet::MediaPacket;
use std::collections::HashMap;

#[derive(Default)]
pub(super) struct ContinuousTimestampState {
    offset_ms: i64,
    last_timestamp_ms_by_stream: HashMap<u64, i64>,
    /// The next packet starts an unrelated timeline: re-anchor it right after
    /// the latest timestamp forwarded on any stream, whichever way it jumped.
    rebase_pending: bool,
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

    /// Backward jumps re-anchor on their own; a forward jump (an encoder
    /// restart, an HLS `EXT-X-DISCONTINUITY`) re-anchors only when marked.
    pub(super) fn mark_discontinuity(&mut self) {
        self.rebase_pending = true;
    }

    pub(super) fn apply(&mut self, packet: &mut MediaPacket) {
        let stream_key = Self::stream_key(packet);
        let raw_timestamp_ms = Self::continuity_timestamp_ms(packet);
        if std::mem::take(&mut self.rebase_pending)
            && let Some(latest_ms) = self.last_timestamp_ms_by_stream.values().copied().max()
        {
            self.offset_ms = latest_ms.saturating_add(1).saturating_sub(raw_timestamp_ms);
        }
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

    #[test]
    fn marked_discontinuity_reanchors_a_forward_jump_after_the_latest_stream() {
        let mut timestamps = ContinuousTimestampState::default();
        let mut video = test_packet(MediaType::Video, 0, 1_000, 1_000);
        let mut audio = test_packet(MediaType::Audio, 0, 1_010, 1_010);
        timestamps.apply(&mut video);
        timestamps.apply(&mut audio);

        timestamps.mark_discontinuity();
        let mut jumped_video = test_packet(MediaType::Video, 0, 9_000_000, 9_000_000);
        let mut jumped_audio = test_packet(MediaType::Audio, 0, 9_000_005, 9_000_005);
        timestamps.apply(&mut jumped_video);
        timestamps.apply(&mut jumped_audio);
        assert_eq!(jumped_video.dts, 1_011);
        assert_eq!(jumped_audio.dts, 1_016);
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
