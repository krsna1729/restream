use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::media::engine::{AudioMeta, MediaEngine, VideoMeta};
use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::media::ring_buffer::{MediaType, Reader, RingBuffer};
use crate::media::stage_lifecycle::StageLifecycle;
use crate::media::stage_metrics::StageMetrics;
use crate::media::{MEDIA_PULL_BURST_PACKETS, MEDIA_TS_BATCH_TARGET_BYTES};

/// Shared input pump that provides identical byte-feeding semantics to both
/// external-process and in-process FFmpeg backends.
///
/// Owns:
/// - `Reader::new_with_keyframe_preroll`
/// - dynamic raw parameter-set refresh
/// - TS packet feeding
/// - packet burst handling
/// - input metrics
/// - lifecycle FirstInput
/// - cancellation
///
/// Reconnect robustness:
/// - Transient reconnect (same stream params): the pump blocks on
///   `wait_for_data` and naturally resumes when the ring refills.
/// - Reconnect with new stream parameters: if an `engine` + `pipeline_id` are
///   attached via [`with_engine`], the pump will re-fetch the video sequence
///   header from the engine's ingest state on each new keyframe after the
///   feeder's SPS/PPS cache is cleared, so new parameters are picked up
///   without restarting the whole stage.
pub struct StageInputPump {
    reader: Reader,
    feeder: TsPacketFeeder,
    metrics: Arc<StageMetrics>,
    include_audio: bool,
    lifecycle: Option<Arc<StageLifecycle>>,
    has_emitted_first_input: bool,
    /// Optional engine + pipeline for dynamic sequence-header refresh on
    /// publisher reconnect with new stream parameters.
    engine_refresh: Option<(Arc<MediaEngine>, String)>,
}

pub trait StageByteSink {
    fn write_ts(
        &mut self,
        bytes: &[u8],
        cancel: &CancellationToken,
    ) -> impl std::future::Future<Output = Result<(), String>> + Send;
}

impl StageInputPump {
    pub fn new(
        name: String,
        ring: Arc<RingBuffer>,
        preroll_packets: usize,
        video_meta: Option<&VideoMeta>,
        audio_tracks: &[AudioMeta],
        include_audio: bool,
        metrics: Arc<StageMetrics>,
    ) -> Self {
        let reader = Reader::new_stage_input(name, ring, preroll_packets);

        let feeder = TsPacketFeeder::new(
            video_meta,
            Arc::new(audio_tracks.to_vec()),
            PacketFeedConfig {
                ..PacketFeedConfig::default()
            },
        );

        Self {
            reader,
            feeder,
            metrics,
            include_audio,
            lifecycle: None,
            has_emitted_first_input: false,
            engine_refresh: None,
        }
    }

    pub fn with_lifecycle(mut self, lifecycle: Arc<StageLifecycle>) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }

    /// Pre-load the AVCC/FLV video sequence header so that `TsPacketFeeder`
    /// can decode SPS/PPS from RTMP-shaped (FLV) packets on the very first
    /// keyframe, before any annex-B data appears in the ring.
    ///
    /// This is the sequence header returned by
    /// `engine.get_sequence_headers(pipeline_id)`.
    pub fn with_video_sequence_header(mut self, header: Option<bytes::Bytes>) -> Self {
        if let Some(h) = header {
            self.feeder.set_video_sequence_header_from_avcc(&h);
        }
        self
    }

    /// Attach an engine + pipeline so that the pump can re-fetch the video
    /// sequence header from the ingest state when the publisher reconnects
    /// with new stream parameters (SPS/PPS change).
    pub fn with_engine(mut self, engine: Arc<MediaEngine>, pipeline_id: String) -> Self {
        self.engine_refresh = Some((engine, pipeline_id));
        self
    }

    /// Return the current input codec hint without exposing the source ring.
    pub fn codec_hint(&self) -> String {
        self.reader.current_ring().codec_hint_str().to_string()
    }

    /// Read from the input ring, feed MPEG-TS bytes to `sink`, until
    /// cancellation or error.
    pub async fn pump_to<S: StageByteSink>(
        &mut self,
        sink: &mut S,
        cancel: &CancellationToken,
    ) -> Result<(), String> {
        let mut ts_batch = Vec::with_capacity(MEDIA_TS_BATCH_TARGET_BYTES);
        let mut packets = Vec::with_capacity(MEDIA_PULL_BURST_PACKETS);

        loop {
            tokio::select! {
                _ = cancel.cancelled() => {
                    debug!("stage input pump cancelled");
                    return Ok(());
                }
                _ = self.reader.wait_for_data() => {
                    if self.reader.is_caught_up_to_end_of_stream() {
                        return Ok(());
                    }

                    packets.clear();
                    if self
                        .reader
                        .pull_burst(&mut packets, MEDIA_PULL_BURST_PACKETS)
                        .is_err()
                    {
                        continue;
                    }

                    ts_batch.clear();

                    for pkt in packets.drain(..) {
                        if !self.include_audio && pkt.media_type == MediaType::Audio {
                            continue;
                        }

                        // Dynamic parameter-set refresh:
                        // When the feeder needs raw video parameter sets (SPS/PPS
                        // for H.264, or VPS/SPS/PPS for HEVC), try to obtain them
                        // from multiple sources in priority order:
                        //   1. Ring buffer annex-B parameter sets (TS/SRT sources)
                        //   2. Per-packet annex-B payload (raw TS frames)
                        //   3. Engine ingest video_sequence_header (RTMP/FLV sources)
                        //
                        // Source 3 also handles publisher reconnect with new stream
                        // parameters: if the ring clears the parameter sets (or the
                        // feeder's cache becomes stale), we re-fetch from the engine.
                        if pkt.media_type == MediaType::Video
                            && self.feeder.needs_raw_video_parameter_sets()
                        {
                            if let Some(parameter_sets) =
                                self.reader.current_ring().video_parameter_sets()
                            {
                                self.feeder
                                    .set_raw_video_parameter_sets_if_empty(&parameter_sets);
                            } else if let Some(parameter_sets) =
                                crate::media::codec::annexb_parameter_sets(&pkt.payload)
                            {
                                self.feeder
                                    .set_raw_video_parameter_sets_if_empty(&parameter_sets);
                            } else if let Some((engine, pipeline_id)) = &self.engine_refresh {
                                // Fallback: fetch AVCC sequence header from engine
                                // ingest state (set by RTMP handler on connect/reconnect).
                                let (video_sh, _) =
                                    engine.get_sequence_headers(pipeline_id).await;
                                if let Some(header) = video_sh {
                                    self.feeder.set_video_sequence_header_from_avcc(&header);
                                }
                            }
                        }

                        let in_bytes = pkt.payload.len() as u64;
                        let extended = self.feeder.extend_ts_for_packet(&pkt, &mut ts_batch);
                        if extended {
                            self.metrics.record_in(in_bytes);
                            if !self.has_emitted_first_input {
                                self.has_emitted_first_input = true;
                                if let Some(lc) = &self.lifecycle {
                                    lc.record_first_input();
                                }
                            }
                        }
                    }

                    if !ts_batch.is_empty()
                        && let Err(e) = sink.write_ts(&ts_batch, cancel).await
                    {
                        return Err(format!("stage byte sink write error: {e:?}"));
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codec_hint_reports_ring_hint_without_exposing_ring() {
        let ring = Arc::new(RingBuffer::new(8));
        ring.set_codec_hint("hevc");
        let pump = StageInputPump::new(
            "test-pump".to_string(),
            ring,
            0,
            None,
            &[],
            true,
            Arc::new(StageMetrics::new()),
        );

        assert_eq!(pump.codec_hint(), "hevc");
    }
}
