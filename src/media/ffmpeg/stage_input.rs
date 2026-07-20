use std::sync::Arc;

use tokio_util::sync::CancellationToken;
use tracing::debug;

use crate::media::MEDIA_TS_BATCH_TARGET_BYTES;
use crate::media::engine::MediaEngine;
use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::MediaType;
use crate::media::ring_buffer::MEDIA_PULL_BURST_PACKETS;
use crate::media::ring_buffer::{Reader, RingBuffer};
use crate::media::stage_lifecycle::StageLifecycle;
use crate::media::stage_metrics::StageMetrics;

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
    use crate::media::packet::{MediaPacket, PayloadFormat};
    use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};
    use bytes::Bytes;
    use std::sync::atomic::Ordering;

    struct CapturingSink {
        bytes_written: usize,
        writes: usize,
    }

    impl StageByteSink for CapturingSink {
        async fn write_ts(
            &mut self,
            bytes: &[u8],
            _cancel: &CancellationToken,
        ) -> Result<(), String> {
            self.bytes_written += bytes.len();
            self.writes += 1;
            Ok(())
        }
    }

    fn video_meta() -> VideoMeta {
        VideoMeta {
            codec: "h264".to_string(),
            width: 1920,
            height: 1080,
            fps: 30.0,
            bw: None,
            pid: None,
            language: None,
            title: None,
            profile: None,
            level: None,
            pixel_format: None,
        }
    }

    fn audio_packet(pts: i64) -> MediaPacket {
        MediaPacket {
            media_type: MediaType::Audio,
            format: PayloadFormat::Raw,
            is_keyframe: false,
            track_index: 0,
            pts,
            dts: pts,
            payload: Bytes::from_static(&[0x11; 32]),
        }
    }

    fn video_keyframe(pts: i64) -> MediaPacket {
        MediaPacket {
            media_type: MediaType::Video,
            format: PayloadFormat::Raw,
            is_keyframe: true,
            track_index: 0,
            pts,
            dts: pts,
            payload: Bytes::from_static(&[0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80]),
        }
    }

    fn h264_parameter_sets() -> Vec<u8> {
        vec![
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x1E, 0xAB, 0x00, 0x00, 0x00, 0x01, 0x68,
            0xCE, 0x38, 0x80,
        ]
    }

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

    #[tokio::test]
    async fn pump_suppresses_first_input_for_filtered_audio_until_eos() {
        let ring = Arc::new(RingBuffer::new(8));
        let lifecycle = Arc::new(StageLifecycle::new(StagePhase::BackendSpawned {
            backend: StageBackendKind::InternalFfmpeg,
            pid: None,
        }));
        let metrics = Arc::new(StageMetrics::new());
        let mut pump = StageInputPump::new(
            "filtered-audio-only".to_string(),
            ring.clone(),
            0,
            None,
            &[],
            false,
            metrics.clone(),
        )
        .with_lifecycle(lifecycle.clone());
        let cancel = CancellationToken::new();
        let mut sink = CapturingSink {
            bytes_written: 0,
            writes: 0,
        };

        ring.push(audio_packet(0));
        ring.mark_end_of_stream();

        pump.pump_to(&mut sink, &cancel)
            .await
            .expect("pump should finish at EOS");

        assert_eq!(sink.bytes_written, 0);
        assert_eq!(sink.writes, 0);
        assert_eq!(metrics.packets_in.load(Ordering::Relaxed), 0);
        assert_eq!(
            lifecycle.current_phase(),
            StagePhase::BackendSpawned {
                backend: StageBackendKind::InternalFfmpeg,
                pid: None,
            }
        );
    }

    #[tokio::test]
    async fn pump_records_first_input_once_after_filtered_audio_then_video_eos() {
        let ring = Arc::new(RingBuffer::new(8));
        ring.set_video_parameter_sets(h264_parameter_sets());
        let video = video_meta();
        let lifecycle = Arc::new(StageLifecycle::new(StagePhase::BackendSpawned {
            backend: StageBackendKind::InternalFfmpeg,
            pid: None,
        }));
        let metrics = Arc::new(StageMetrics::new());
        let mut pump = StageInputPump::new(
            "filtered-audio-then-video".to_string(),
            ring.clone(),
            0,
            Some(&video),
            &[],
            false,
            metrics.clone(),
        )
        .with_lifecycle(lifecycle.clone());
        let cancel = CancellationToken::new();
        let mut sink = CapturingSink {
            bytes_written: 0,
            writes: 0,
        };

        ring.push(audio_packet(0));
        ring.push(video_keyframe(33));
        ring.mark_end_of_stream();

        pump.pump_to(&mut sink, &cancel)
            .await
            .expect("pump should finish at EOS");

        assert!(sink.bytes_written > 0);
        assert_eq!(sink.writes, 1);
        assert_eq!(metrics.packets_in.load(Ordering::Relaxed), 1);
        let snapshot = lifecycle.snapshot();
        assert_eq!(snapshot.phase, StagePhase::FirstInput);
        assert!(snapshot.first_input_at.is_some());
    }
}
