use std::sync::Arc;

use crate::media::ring_buffer::{MediaPacket, MediaType, RingBuffer};
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
    has_emitted: bool,
    video_track_count: usize,
}

impl StageOutputNormalizer {
    pub fn new(out_ring: Arc<RingBuffer>, stream_count: usize, metrics: Arc<StageMetrics>) -> Self {
        Self {
            timeline: StageTimeline::new(stream_count),
            out_ring,
            metrics,
            has_emitted: false,
            video_track_count: 1,
        }
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

        // Extract and set parameter sets from encoded video.
        if packet.media_type == MediaType::Video
            && packet.is_keyframe
            && let Some(ps) = crate::media::codec::annexb_parameter_sets(&packet.payload)
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
        }

        self.metrics.record_out(packet.payload.len() as u64);
        self.out_ring.push(packet);
    }

    /// Normalize and push a batch of packets.
    pub fn push_batch(&mut self, packets: &mut Vec<MediaPacket>) {
        for packet in packets.drain(..) {
            self.push(packet);
        }
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
    Existing(StageOutputNormalizer),
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
            Self::Existing(normalizer) => normalizer,
            Self::Ring { out_ring, metrics } => {
                let normalizer_metrics = metrics.unwrap_or_else(|| Arc::new(StageMetrics::new()));
                StageOutputNormalizer::new(out_ring, stream_count, normalizer_metrics)
                    .with_video_track_count(1)
            }
        }
    }
}
