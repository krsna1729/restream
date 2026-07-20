use std::sync::Arc;

use crate::media::codec::AnnexbParameterSetAccumulator;
use crate::media::packet::{MediaPacket, MediaType};
use crate::media::ring_buffer::RingBuffer;
use crate::media::stage_lifecycle::StageLifecycle;
use crate::media::stage_metrics::StageMetrics;

use super::timeline::{NormalizedTs, StageTimeline};

/// Shared output normalizer used by both external and internal backends.
///
/// No backend writes directly to a `RingBuffer`. Every backend emits packets
/// through `StageOutputNormalizer`, which handles:
///
/// - Timestamp normalization to a shared stage-local epoch.
/// - DTS monotonicity enforcement per output stream.
/// - Parameter-set extraction from encoded video keyframes.
/// - First-output lifecycle tracking and metrics.
pub struct StageOutputNormalizer {
    timeline: StageTimeline,
    out_ring: Arc<RingBuffer>,
    metrics: Arc<StageMetrics>,
    lifecycle: Option<Arc<StageLifecycle>>,
    has_emitted: bool,
    video_track_count: usize,
    parameter_sets: AnnexbParameterSetAccumulator,
}

impl StageOutputNormalizer {
    pub fn new(out_ring: Arc<RingBuffer>, stream_count: usize, metrics: Arc<StageMetrics>) -> Self {
        Self {
            timeline: StageTimeline::new(stream_count),
            out_ring,
            metrics,
            lifecycle: None,
            has_emitted: false,
            video_track_count: 1,
            parameter_sets: AnnexbParameterSetAccumulator::default(),
        }
    }

    pub fn with_lifecycle(mut self, lifecycle: Arc<StageLifecycle>) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }

    /// Configure the number of video tracks. Audio tracks are assumed to start
    /// immediately after the video tracks.
    pub fn with_video_track_count(mut self, count: usize) -> Self {
        self.video_track_count = count.max(1);
        self
    }

    pub fn timeline(&mut self) -> &mut StageTimeline {
        &mut self.timeline
    }

    /// Normalize and push a single packet through the output pipeline.
    pub fn push(&mut self, mut packet: MediaPacket) {
        let stream_idx = self.stream_index(packet.media_type, packet.track_index);

        if packet.media_type == MediaType::Video
            && !packet.is_keyframe
            && crate::media::codec::raw_annexb_is_keyframe(&packet.payload)
        {
            packet.is_keyframe = true;
        }

        // Extract and set parameter sets from encoded video. FFmpeg encoders
        // may emit VPS/SPS/PPS as separate packets before the IDR packet, so
        // the accumulator must run for every video packet, not only keyframes.
        if packet.media_type == MediaType::Video
            && let Some(ps) = self.parameter_sets.push_payload(&packet.payload)
        {
            self.out_ring.set_video_parameter_sets(ps);
        }

        // Normalize timestamps to stage-local epoch.
        let NormalizedTs { pts_ms, dts_ms } =
            self.timeline.normalize(stream_idx, packet.pts, packet.dts);
        packet.pts = pts_ms;
        packet.dts = dts_ms;

        if !self.has_emitted {
            self.has_emitted = true;
            if let Some(lifecycle) = &self.lifecycle {
                lifecycle.record_first_output();
            }
        }

        self.metrics.record_out(packet.payload.len() as u64);
        self.out_ring.push(packet);
        if let Some(lifecycle) = &self.lifecycle {
            lifecycle.record_producing();
        }
    }

    /// Normalize and push a batch of packets.
    pub fn push_batch(&mut self, packets: &mut Vec<MediaPacket>) {
        for packet in packets.drain(..) {
            self.push(packet);
        }
    }

    pub fn mark_end_of_stream(&self) {
        self.out_ring.mark_end_of_stream();
    }

    fn stream_index(&self, media_type: MediaType, track_index: u32) -> usize {
        // TODO: replace with a real StageStreamMap once multi-video/multi-audio
        // outputs are supported. For now assume video tracks occupy indices
        // [0, video_track_count) and audio tracks follow immediately.
        match media_type {
            MediaType::Video => track_index as usize,
            MediaType::Audio => self.video_track_count + track_index as usize,
        }
    }
}

pub(crate) enum StageOutputSink {
    Existing(Box<StageOutputNormalizer>),
    Ring {
        out_ring: Arc<RingBuffer>,
        metrics: Option<Arc<StageMetrics>>,
    },
}

impl StageOutputSink {
    pub(crate) fn from_ring(out_ring: Arc<RingBuffer>, metrics: Option<Arc<StageMetrics>>) -> Self {
        Self::Ring { out_ring, metrics }
    }

    pub(crate) fn into_normalizer(self, stream_count: usize) -> StageOutputNormalizer {
        match self {
            Self::Existing(normalizer) => *normalizer,
            Self::Ring { out_ring, metrics } => {
                let normalizer_metrics = metrics.unwrap_or_else(|| Arc::new(StageMetrics::new()));
                StageOutputNormalizer::new(out_ring, stream_count, normalizer_metrics)
                    .with_video_track_count(1)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::packet::PayloadFormat;
    use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};
    use bytes::Bytes;
    use proptest::prelude::*;
    use std::collections::HashMap;
    use std::sync::atomic::Ordering;

    fn video_packet(pts: i64) -> MediaPacket {
        MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts,
            dts: pts,
            is_keyframe: true,
            format: PayloadFormat::Raw,
            payload: Bytes::from_static(&[0, 0, 1, 9]),
        }
    }

    fn generated_packet(
        is_video: bool,
        track_index: u32,
        pts: i64,
        dts: i64,
        payload_len: usize,
    ) -> MediaPacket {
        let media_type = if is_video {
            MediaType::Video
        } else {
            MediaType::Audio
        };
        MediaPacket {
            media_type,
            track_index,
            pts,
            dts,
            is_keyframe: is_video,
            format: PayloadFormat::Raw,
            payload: Bytes::from(vec![0x55; payload_len.max(1)]),
        }
    }

    #[test]
    fn normalizer_records_first_output_and_marks_stage_producing() {
        let ring = Arc::new(RingBuffer::new(8));
        let lifecycle = Arc::new(StageLifecycle::new(StagePhase::BackendSpawned {
            backend: StageBackendKind::InternalFfmpeg,
            pid: None,
        }));
        let mut normalizer = StageOutputNormalizer::new(ring, 1, Arc::new(StageMetrics::new()))
            .with_lifecycle(lifecycle.clone());

        normalizer.push(video_packet(10));

        let first_snapshot = lifecycle.snapshot();
        assert_eq!(first_snapshot.phase, StagePhase::Producing);
        let first_output_at = first_snapshot
            .first_output_at
            .expect("first packet should record first_output_at");

        normalizer.push(video_packet(20));

        let second_snapshot = lifecycle.snapshot();
        assert_eq!(second_snapshot.phase, StagePhase::Producing);
        assert_eq!(second_snapshot.first_output_at, Some(first_output_at));
    }

    #[test]
    fn normalizer_marks_annexb_idr_payload_as_keyframe() {
        let ring = Arc::new(RingBuffer::new(8));
        let mut normalizer =
            StageOutputNormalizer::new(ring.clone(), 1, Arc::new(StageMetrics::new()));

        normalizer.push(MediaPacket {
            media_type: MediaType::Video,
            track_index: 0,
            pts: 0,
            dts: 0,
            is_keyframe: false,
            format: PayloadFormat::Raw,
            payload: Bytes::from_static(&[0, 0, 0, 1, 0x65]),
        });

        let mut reader =
            crate::media::ring_buffer::Reader::new("normalizer-keyframe-reader".to_string(), ring);
        let packet = reader
            .pull()
            .expect("reader should not overflow")
            .expect("reader should start at inferred keyframe");
        assert!(packet.is_keyframe);
    }

    #[test]
    fn normalizer_caches_split_hevc_parameter_sets_before_idr() {
        let ring = Arc::new(RingBuffer::new(8));
        let mut normalizer =
            StageOutputNormalizer::new(ring.clone(), 1, Arc::new(StageMetrics::new()));

        for (pts, payload) in [
            (0, Bytes::from_static(&[0, 0, 0, 1, 0x40, 0x01])),
            (1, Bytes::from_static(&[0, 0, 0, 1, 0x42, 0x01])),
            (2, Bytes::from_static(&[0, 0, 0, 1, 0x44, 0x01])),
            (3, Bytes::from_static(&[0, 0, 0, 1, 0x26, 0x01])),
        ] {
            normalizer.push(MediaPacket {
                media_type: MediaType::Video,
                track_index: 0,
                pts,
                dts: pts,
                is_keyframe: false,
                format: PayloadFormat::Raw,
                payload,
            });
        }

        assert_eq!(
            ring.video_parameter_sets().as_deref(),
            Some(
                &[
                    0, 0, 0, 1, 0x40, 0x01, 0, 0, 0, 1, 0x42, 0x01, 0, 0, 0, 1, 0x44, 0x01
                ][..]
            )
        );

        let mut reader =
            crate::media::ring_buffer::Reader::new("split-hevc-param-reader".to_string(), ring);
        let packet = reader
            .pull()
            .expect("reader should not overflow")
            .expect("reader should start at inferred HEVC IDR");
        assert_eq!(packet.payload.as_ref(), &[0, 0, 0, 1, 0x26, 0x01]);
        assert!(packet.is_keyframe);
    }

    proptest! {
        #[test]
        fn normalizer_preserves_stage_boundary_timestamp_invariants(
            packets in prop::collection::vec(
                (
                    any::<bool>(),
                    0_u32..2,
                    -5_000_i64..20_000,
                    -5_000_i64..20_000,
                    1_usize..32,
                ),
                1..64,
            )
        ) {
            let ring = Arc::new(RingBuffer::new(128));
            let metrics = Arc::new(StageMetrics::new());
            let mut normalizer =
                StageOutputNormalizer::new(ring.clone(), 4, metrics.clone())
                    .with_video_track_count(2);
            let mut reader =
                crate::media::ring_buffer::Reader::new("normalizer-boundary-proptest".to_string(), ring);

            normalizer.push(video_packet(0));
            for (is_video, track, pts, dts, payload_len) in &packets {
                normalizer.push(generated_packet(*is_video, *track, *pts, *dts, *payload_len));
            }

            let mut last_dts_by_stream: HashMap<usize, i64> = HashMap::new();
            let mut pulled = 0_u64;

            while let Some(packet) = reader.pull().expect("reader should not overflow") {
                prop_assert!(
                    packet.pts >= 0,
                    "normalizer emitted negative PTS {} for packet {:?}",
                    packet.pts,
                    packet
                );
                prop_assert!(
                    packet.dts >= 0,
                    "normalizer emitted negative DTS {} for packet {:?}",
                    packet.dts,
                    packet
                );
                let stream_key = match packet.media_type {
                    MediaType::Video => packet.track_index as usize,
                    MediaType::Audio => 2 + packet.track_index as usize,
                };
                if let Some(previous_dts) = last_dts_by_stream.insert(stream_key, packet.dts) {
                    prop_assert!(
                        packet.dts >= previous_dts,
                        "DTS regressed for stream {}: previous={}, current={}",
                        stream_key,
                        previous_dts,
                        packet.dts
                    );
                }
                pulled += 1;
            }

            prop_assert_eq!(pulled as usize, packets.len() + 1);
            prop_assert_eq!(metrics.packets_out.load(Ordering::Relaxed), pulled);
            let expected_bytes: u64 =
                video_packet(0).payload.len() as u64
                    + packets.iter().map(|(_, _, _, _, len)| *len as u64).sum::<u64>();
            prop_assert_eq!(metrics.bytes_out.load(Ordering::Relaxed), expected_bytes);
        }
    }
}
