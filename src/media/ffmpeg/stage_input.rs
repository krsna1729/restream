use std::sync::Arc;

use tokio_util::sync::CancellationToken;

use crate::media::engine::MediaEngine;
use crate::media::feeder::{PacketFeedConfig, TsPacketFeeder};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::{MediaPacket, MediaType};
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
            PacketFeedConfig::default(),
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

    /// Turn this pump into the input of an in-process FFmpeg stage: the
    /// stage's AVIO reads pull the ring and encode TS on the FFmpeg thread
    /// (see [`crate::media::avio::QueueRefill`]), so no Tokio task feeds it.
    pub fn into_queue_refill(self, cancel: CancellationToken) -> StageInputRefill {
        StageInputRefill {
            pump: self,
            cancel,
            packets: Vec::with_capacity(MEDIA_PULL_BURST_PACKETS),
        }
    }

    /// Pull one burst from the ring and append its TS to `ts_batch`. Awaits
    /// only for the rare engine sequence-header refresh.
    async fn encode_burst(&mut self, packets: &mut Vec<Arc<MediaPacket>>, ts_batch: &mut Vec<u8>) {
        packets.clear();
        if self
            .reader
            .pull_burst(packets, MEDIA_PULL_BURST_PACKETS)
            .is_err()
        {
            return;
        }

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
            if pkt.media_type == MediaType::Video && self.feeder.needs_raw_video_parameter_sets() {
                if let Some(parameter_sets) = self.reader.current_ring().video_parameter_sets() {
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
                    let (video_sh, _) = engine.get_sequence_headers(pipeline_id).await;
                    if let Some(header) = video_sh {
                        self.feeder.set_video_sequence_header_from_avcc(&header);
                    }
                }
            }

            let in_bytes = pkt.payload.len() as u64;
            let extended = self.feeder.extend_ts_for_packet(&pkt, ts_batch);
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
    }
}

/// A [`StageInputPump`] driven by its FFmpeg stage's AVIO reads.
pub struct StageInputRefill {
    pump: StageInputPump,
    cancel: CancellationToken,
    packets: Vec<Arc<MediaPacket>>,
}

impl crate::media::avio::QueueRefill for StageInputRefill {
    fn refill(&mut self, out: &mut Vec<u8>) -> bool {
        loop {
            let data = block_on(async {
                tokio::select! {
                    biased;
                    _ = self.cancel.cancelled() => false,
                    _ = self.pump.reader.wait_for_data() => true,
                }
            });
            if !data || self.pump.reader.is_caught_up_to_end_of_stream() {
                return false;
            }
            block_on(self.pump.encode_burst(&mut self.packets, out));
            if !out.is_empty() {
                return true;
            }
        }
    }
}

/// Run `future` to completion on the calling (non-Tokio) thread, parking it
/// while pending. The stage futures here only wait on runtime-agnostic
/// primitives (`Notify`, `CancellationToken`, Tokio locks). The waker is
/// cached per thread, so a poll that completes at once allocates nothing.
fn block_on<F: std::future::Future>(future: F) -> F::Output {
    use std::task::{Context, Poll, Wake, Waker};

    struct ThreadUnpark(std::thread::Thread);
    impl Wake for ThreadUnpark {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
        fn wake_by_ref(self: &Arc<Self>) {
            self.0.unpark();
        }
    }
    thread_local! {
        static WAKER: Waker = Waker::from(Arc::new(ThreadUnpark(std::thread::current())));
    }

    WAKER.with(|waker| {
        let mut cx = Context::from_waker(waker);
        let mut future = std::pin::pin!(future);
        loop {
            if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
                return output;
            }
            std::thread::park();
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::packet::{MediaPacket, PayloadFormat};
    use crate::media::stage_lifecycle::{StageBackendKind, StagePhase};
    use bytes::Bytes;
    use std::sync::atomic::Ordering;

    /// Drain a refill-driven pump the way its reading thread would, until
    /// it reports end of input. Returns (TS bytes, non-empty batches).
    fn drain(pump: StageInputPump) -> (usize, usize) {
        use crate::media::avio::QueueRefill;
        let mut refill = pump.into_queue_refill(CancellationToken::new());
        let (mut bytes, mut batches) = (0, 0);
        let mut out = Vec::new();
        loop {
            out.clear();
            if !refill.refill(&mut out) {
                return (bytes, batches);
            }
            bytes += out.len();
            batches += 1;
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

    #[test]
    fn pump_suppresses_first_input_for_filtered_audio_until_eos() {
        let ring = Arc::new(RingBuffer::new(8));
        let lifecycle = Arc::new(StageLifecycle::new(StagePhase::BackendSpawned {
            backend: StageBackendKind::InternalFfmpeg,
            pid: None,
        }));
        let metrics = Arc::new(StageMetrics::new());
        let pump = StageInputPump::new(
            "filtered-audio-only".to_string(),
            ring.clone(),
            0,
            None,
            &[],
            false,
            metrics.clone(),
        )
        .with_lifecycle(lifecycle.clone());
        ring.push(audio_packet(0));
        ring.mark_end_of_stream();

        let (bytes_written, writes) = drain(pump);

        assert_eq!(bytes_written, 0);
        assert_eq!(writes, 0);
        assert_eq!(metrics.packets_in.load(Ordering::Relaxed), 0);
        assert_eq!(
            lifecycle.current_phase(),
            StagePhase::BackendSpawned {
                backend: StageBackendKind::InternalFfmpeg,
                pid: None,
            }
        );
    }

    #[test]
    fn pump_records_first_input_once_after_filtered_audio_then_video_eos() {
        let ring = Arc::new(RingBuffer::new(8));
        ring.set_video_parameter_sets(h264_parameter_sets());
        let video = video_meta();
        let lifecycle = Arc::new(StageLifecycle::new(StagePhase::BackendSpawned {
            backend: StageBackendKind::InternalFfmpeg,
            pid: None,
        }));
        let metrics = Arc::new(StageMetrics::new());
        let pump = StageInputPump::new(
            "filtered-audio-then-video".to_string(),
            ring.clone(),
            0,
            Some(&video),
            &[],
            false,
            metrics.clone(),
        )
        .with_lifecycle(lifecycle.clone());
        ring.push(audio_packet(0));
        ring.push(video_keyframe(33));
        ring.mark_end_of_stream();

        let (bytes_written, writes) = drain(pump);

        assert!(bytes_written > 0);
        assert_eq!(writes, 1);
        assert_eq!(metrics.packets_in.load(Ordering::Relaxed), 1);
        let snapshot = lifecycle.snapshot();
        assert_eq!(snapshot.phase, StagePhase::FirstInput);
        assert!(snapshot.first_input_at.is_some());
    }

    fn video_pump(ring: &Arc<RingBuffer>, metrics: &Arc<StageMetrics>) -> StageInputPump {
        ring.set_video_parameter_sets(h264_parameter_sets());
        StageInputPump::new(
            "refill".to_string(),
            ring.clone(),
            0,
            Some(&video_meta()),
            &[],
            true,
            metrics.clone(),
        )
    }

    /// The FFmpeg-thread input path: AVIO reads through the queue pull the
    /// ring and encode TS on the reading thread, with no Tokio runtime, and
    /// the queue closes at end of stream.
    #[test]
    fn queue_refill_pulls_the_ring_on_the_reading_thread_until_eos() {
        use crate::media::avio::MemoryQueue;

        let ring = Arc::new(RingBuffer::new(8));
        let metrics = Arc::new(StageMetrics::new());
        let queue = Arc::new(MemoryQueue::new());
        queue.set_refill(Box::new(
            video_pump(&ring, &metrics).into_queue_refill(CancellationToken::new()),
        ));

        ring.push(video_keyframe(0));
        let reader = {
            let queue = queue.clone();
            std::thread::spawn(move || {
                let mut total = Vec::new();
                let mut buf = [0u8; 4096];
                loop {
                    let n = queue.read(&mut buf);
                    if n == 0 {
                        return total;
                    }
                    total.extend_from_slice(&buf[..n]);
                }
            })
        };
        // The reader parks waiting for the ring, then wakes on publication.
        std::thread::sleep(std::time::Duration::from_millis(20));
        ring.push(video_keyframe(33));
        ring.mark_end_of_stream();

        let ts = reader.join().unwrap();
        assert!(!ts.is_empty());
        assert_eq!(ts.len() % 188, 0);
        assert!(ts.chunks(188).all(|packet| packet[0] == 0x47));
        assert_eq!(metrics.packets_in.load(Ordering::Relaxed), 2);
        assert!(queue.is_closed());
    }

    #[test]
    fn queue_refill_stops_on_cancel_while_waiting() {
        use crate::media::avio::MemoryQueue;

        let ring = Arc::new(RingBuffer::new(8));
        let metrics = Arc::new(StageMetrics::new());
        let cancel = CancellationToken::new();
        let queue = Arc::new(MemoryQueue::new());
        queue.set_refill(Box::new(
            video_pump(&ring, &metrics).into_queue_refill(cancel.clone()),
        ));
        let reader = {
            let queue = queue.clone();
            std::thread::spawn(move || queue.read(&mut [0u8; 64]))
        };
        std::thread::sleep(std::time::Duration::from_millis(20));
        cancel.cancel();

        assert_eq!(reader.join().unwrap(), 0);
        assert!(queue.is_closed());
    }
}
