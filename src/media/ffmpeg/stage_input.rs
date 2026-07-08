use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::media::engine::{AudioMeta, VideoMeta};
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
pub struct StageInputPump {
    reader: Reader,
    feeder: TsPacketFeeder,
    metrics: Arc<StageMetrics>,
    include_audio: bool,
    lifecycle: Option<Arc<StageLifecycle>>,
    has_emitted_first_input: bool,
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
        let reader = Reader::new_with_keyframe_preroll(name, ring, preroll_packets);

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
        }
    }

    pub fn with_lifecycle(mut self, lifecycle: Arc<StageLifecycle>) -> Self {
        self.lifecycle = Some(lifecycle);
        self
    }

    /// Access the source ring the pump reads from. This is a temporary
    /// compatibility helper while existing backend functions still take rings.
    pub fn source_ring(&self) -> Arc<RingBuffer> {
        self.reader.current_ring().clone()
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
                    packets.clear();
                    if self.reader.pull_burst(&mut packets, MEDIA_PULL_BURST_PACKETS).is_err() {
                        continue;
                    }

                    ts_batch.clear();

                    for pkt in packets.drain(..) {
                        if !self.include_audio && pkt.media_type == MediaType::Audio {
                            continue;
                        }

                        // Dynamic parameter-set refresh:
                        // Check if we need raw video parameter sets (VPS/SPS/PPS
                        // for HEVC, SPS/PPS for H.264) and try to get them from
                        // the ring or from the packet's annex B payload.
                        if pkt.media_type == MediaType::Video
                            && self.feeder.needs_raw_video_parameter_sets()
                        {
                            if let Some(parameter_sets) = self.reader.current_ring().video_parameter_sets() {
                                self.feeder.set_raw_video_parameter_sets_if_empty(&parameter_sets);
                            } else if let Some(parameter_sets) =
                                crate::media::codec::annexb_parameter_sets(&pkt.payload)
                            {
                                self.feeder.set_raw_video_parameter_sets_if_empty(&parameter_sets);
                            }
                        }

                        let in_bytes = pkt.payload.len() as u64;
                        if self.feeder.extend_ts_for_packet(&pkt, &mut ts_batch) {
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
