//! Shared H.265→H.264 transcoder stage.
//!
//! Runs a single decode→encode pipeline per pipeline_id. All RTMP egresses
//! on the same source pipeline share one OS thread that decodes H.265 and
//! re-encodes H.264. Audio packets pass through unchanged.
//!
//! Architecture (same pattern as `transcoder.rs`):
//!
//!   tokio task:  source RingBuffer → TsMuxer → MemoryQueue
//!   std::thread: MemoryQueue → FFmpeg demux → decode H.265 → encode H.264 → output RingBuffer
//!
//! The output RingBuffer carries H.264 video (PayloadFormat::Raw) plus
//! passthrough audio — exactly what the RTMP egress reader expects.

use bytes::Bytes;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

use crate::domain::stage::StageKey;
use crate::media::avio::MemoryQueue;
use crate::media::ffmpeg::stage_input::StageInputPump;
use crate::media::ffmpeg::stage_output::{StageOutputNormalizer, StageOutputSink};
use crate::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat};
use crate::media::transcoder::InternalMemoryQueueSink;

#[cfg(test)]
use crate::media::ring_buffer::RingBuffer;

/// Zero-copy wrapper: holds an `ffmpeg_next::Packet` so `Bytes::from_owner`
/// can serve the encoded/demuxed buffer to ring-buffer readers without a `memcpy`.
///
/// Drop calls `av_packet_unref`, decrementing the AVBufferRef refcount. The data
/// remains valid until every downstream `Bytes` clone is released.
///
/// `ffmpeg_next::Packet` is `unsafe impl Send + Sync`, satisfying `from_owner`'s bounds.
struct OwnedFfmpegPacket(ffmpeg_next::Packet);
impl AsRef<[u8]> for OwnedFfmpegPacket {
    #[inline]
    fn as_ref(&self) -> &[u8] {
        self.0.data().unwrap_or(&[])
    }
}

fn ffmpeg_error_is_again(error: ffmpeg_next::Error) -> bool {
    matches!(
        error,
        ffmpeg_next::Error::Other { errno } if errno == ffmpeg_next::error::EAGAIN
    )
}

fn ffmpeg_error_is_eof(error: ffmpeg_next::Error) -> bool {
    matches!(error, ffmpeg_next::Error::Eof)
}

fn drain_h264_encoder_packets(
    enc: &mut ffmpeg_next::codec::encoder::video::Encoder,
    enc_pkt: &mut ffmpeg_next::Packet,
    normalizer: &mut StageOutputNormalizer,
) -> usize {
    let mut drained = 0;
    loop {
        match enc.receive_packet(enc_pkt) {
            Ok(()) => {
                let pts_ms = enc_pkt.pts().unwrap_or(0);
                // DTS can differ from PTS when B-frames are enabled: the encoder
                // returns the decode timestamp separately.  Setting dts=pts would
                // break B-frame reordering in downstream muxers (TS, MP4).
                let dts_ms = enc_pkt.dts().unwrap_or(pts_ms);
                // enc_pkt is reused across iterations; clone() calls av_packet_ref (refcount
                // bump only, no data copy) so the ring buffer holds the AVBufferRef alive.
                normalizer.push(MediaPacket {
                    media_type: MediaType::Video,
                    track_index: 0,
                    pts: pts_ms,
                    dts: dts_ms,
                    is_keyframe: enc_pkt.is_key(),
                    format: PayloadFormat::Raw,
                    payload: Bytes::from_owner(OwnedFfmpegPacket(enc_pkt.clone())),
                });
                drained += 1;
            }
            Err(error) if ffmpeg_error_is_again(error) || ffmpeg_error_is_eof(error) => break,
            Err(_) => break,
        }
    }
    drained
}

fn send_h264_frame_with_drain(
    enc: &mut ffmpeg_next::codec::encoder::video::Encoder,
    frame: &ffmpeg_next::frame::Video,
    enc_pkt: &mut ffmpeg_next::Packet,
    normalizer: &mut StageOutputNormalizer,
) -> bool {
    for _attempt in 0..2 {
        match enc.send_frame(frame) {
            Ok(()) => return true,
            Err(error) if ffmpeg_error_is_again(error) => {
                if drain_h264_encoder_packets(enc, enc_pkt, normalizer) == 0 {
                    return false;
                }
            }
            Err(error) if ffmpeg_error_is_eof(error) => return false,
            Err(_) => return false,
        }
    }
    false
}

/// Backend implementation for the shared H.265→H.264 codec-edge stage.
///
/// 1. Waits for ingest metadata (video + audio tracks).
/// 2. Spawns a blocking OS thread for FFmpeg decode→encode.
/// 3. Forwards source RingBuffer packets to the MemoryQueue as MPEG-TS.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_h264_codec_edge_stage(
    pipeline_id: String,
    engine: Arc<crate::media::engine::MediaEngine>,
    cancel_token: CancellationToken,
    stage_key: StageKey,
    mut input_pump: StageInputPump,
    output_normalizer: StageOutputNormalizer,
) {
    let input_queue = Arc::new(MemoryQueue::new_with_capacity(engine.config.avio_capacity));
    engine
        .register_input_queue(stage_key.clone(), input_queue.clone())
        .await;

    // Spawn OS thread for FFmpeg decode→encode
    let iq_clone = input_queue.clone();
    let cancel_clone = cancel_token.clone();
    let cancel_on_exit = cancel_token.clone();
    let pid = pipeline_id.clone();
    let handle = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_ffmpeg_h264_stage_with_normalizer(
                iq_clone,
                cancel_clone,
                &pid,
                StageOutputSink::Existing(Box::new(output_normalizer)),
            )
        }));
        match result {
            Ok(Err(err)) => error!(pipeline_id = %pid, err, "FFmpeg H.264 stage failed"),
            Err(_) => error!("FFmpeg stage panicked for pipeline {pid}"),
            _ => {}
        }
        cancel_on_exit.cancel();
    });
    engine.register_os_thread(handle);

    // Input pumping: shared pump (plan dispatch)
    let mut sink = InternalMemoryQueueSink::new(input_queue.clone(), cancel_token.clone());
    let _ = input_pump.pump_to(&mut sink, &cancel_token).await;

    input_queue.close();
    engine.remove_input_queue(&stage_key).await;
    engine.remove_stage_metrics(&stage_key).await;
    engine.remove_stage_runtime(&stage_key).await;
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id: pipeline_id.clone(),
            encoding: stage_key.kind.to_string(),
        });
}

/// Blocking FFmpeg decode→encode loop, runs on a dedicated OS thread.
///
/// Demuxes MPEG-TS from `in_queue`, decodes H.265 video, encodes H.264,
/// and pushes packets to `out_ring`. Audio passes through unchanged.
#[cfg(test)]
fn run_ffmpeg_h264_stage(
    in_queue: Arc<MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    cancel: CancellationToken,
    _pipeline_id: &str,
) -> Result<(), &'static str> {
    run_ffmpeg_h264_stage_with_normalizer(
        in_queue,
        cancel,
        _pipeline_id,
        StageOutputSink::from_ring(out_ring, None),
    )
}

fn run_ffmpeg_h264_stage_with_normalizer(
    in_queue: Arc<MemoryQueue>,
    cancel: CancellationToken,
    _pipeline_id: &str,
    output_sink: StageOutputSink,
) -> Result<(), &'static str> {
    use crate::media::avio::CustomInput;
    use ffmpeg_next::format::Pixel;

    let mut custom = CustomInput::new(&*in_queue)?;
    let ictx = custom
        .input
        .as_mut()
        .ok_or("failed to get CustomInput context")?;

    // Identify streams
    let video_idx = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .map(|s| s.index())
        .ok_or("no video stream")?;

    // Build stream metadata: (media_type, track_index) for each stream
    let mut stream_meta: Vec<Option<(MediaType, u32)>> = Vec::new();
    let mut audio_track_counter = 0u32;
    for s in ictx.streams() {
        match s.parameters().medium() {
            ffmpeg_next::media::Type::Video => {
                stream_meta.push(Some((MediaType::Video, 0)));
            }
            ffmpeg_next::media::Type::Audio => {
                stream_meta.push(Some((MediaType::Audio, audio_track_counter)));
                audio_track_counter += 1;
            }
            _ => {
                stream_meta.push(None);
            }
        }
    }

    let dec_params = ictx
        .stream(video_idx)
        .ok_or("no video stream")?
        .parameters();
    let dec_ctx = ffmpeg_next::codec::Context::from_parameters(dec_params)
        .map_err(|_| "decoder context error")?;
    let mut decoder = dec_ctx
        .decoder()
        .video()
        .map_err(|_| "decoder open error")?;

    let enc_codec = ffmpeg_next::codec::encoder::find(ffmpeg_next::codec::Id::H264)
        .ok_or("no H.264 encoder")?;

    // Build x264 encoder options: CRF mode for quality-based encoding
    // instead of fixed bitrate. CRF 23 is x264's default.

    let stream_count = 1 + audio_track_counter as usize;
    let mut normalizer = output_sink.into_normalizer(stream_count);

    let mut encoder: Option<ffmpeg_next::codec::encoder::video::Encoder> = None;
    let mut scaler: Option<ffmpeg_next::software::scaling::Context> = None;
    let mut enc_frame = ffmpeg_next::frame::Video::empty();
    let mut enc_pkt = ffmpeg_next::Packet::empty();

    for (stream, pkt) in ictx.packets() {
        if cancel.is_cancelled() {
            break;
        }

        let idx = stream.index();

        // Audio passthrough
        if stream.parameters().medium() == ffmpeg_next::media::Type::Audio {
            let Some(&Some((media_type, track_index))) = stream_meta.get(idx) else {
                continue;
            };
            let tb = stream.time_base();
            // Drop packets with AV_NOPTS_VALUE rather than substituting 0.
            // A pts of 0 on a stream running for hours would cause a massive
            // backward jump through DtsEnforcer, corrupting A/V sync (M7 fix).
            let Some(pts) = pkt.pts() else { continue };
            let dts_val = pkt.dts().unwrap_or(pts);
            let pts_ms = if tb.1 != 0 {
                // i128 avoids f64 precision loss for large pts values on long
                // streams (hours of 90 kHz timebase accumulate sub-ms drift).
                (pts as i128 * tb.0 as i128 * 1000 / tb.1 as i128) as i64
            } else {
                pts
            };
            let dts_ms = if tb.1 != 0 {
                (dts_val as i128 * tb.0 as i128 * 1000 / tb.1 as i128) as i64
            } else {
                dts_val
            };
            let is_keyframe = pkt.is_key();
            let payload = Bytes::from_owner(OwnedFfmpegPacket(pkt));
            normalizer.push(MediaPacket {
                media_type,
                track_index,
                pts: pts_ms,
                dts: dts_ms,
                is_keyframe,
                format: PayloadFormat::Raw,
                payload,
            });
            continue;
        }

        if stream.index() != video_idx {
            continue;
        }

        let video_tb = stream.time_base();

        // Video: decode H.265 → encode H.264. FFmpeg may return EAGAIN from
        // send_packet when decoded frames must be drained first; do not drop
        // that input packet.
        let mut packet_sent = false;
        for _attempt in 0..2 {
            match decoder.send_packet(&pkt) {
                Ok(()) => {
                    packet_sent = true;
                }
                Err(error) if ffmpeg_error_is_again(error) => {}
                Err(error) if ffmpeg_error_is_eof(error) => break,
                Err(_) => break,
            }

            let mut drained_frame = false;
            let mut dec_frame = ffmpeg_next::frame::Video::empty();
            loop {
                match decoder.receive_frame(&mut dec_frame) {
                    Ok(()) => {
                        drained_frame = true;
                    }
                    Err(error) if ffmpeg_error_is_again(error) || ffmpeg_error_is_eof(error) => {
                        break;
                    }
                    Err(_) => break,
                }

                // Lazy encoder + scaler init on first decoded frame
                if encoder.is_none() {
                    let width = decoder.width();
                    let height = decoder.height();
                    let in_fmt = dec_frame.format();

                    // Load transcode profile from DB (via runtime cache)
                    let profile = crate::media::profiles::cache()
                        .blocking_read()
                        .get("h264")
                        .cloned()
                        .unwrap_or_default();

                    // Resolve output dimensions: 0 = match source
                    let out_w = if profile.width > 0 {
                        profile.width
                    } else {
                        width
                    };
                    let out_h = if profile.height > 0 {
                        profile.height
                    } else {
                        height
                    };

                    let sw = ffmpeg_next::software::scaling::Context::get(
                        in_fmt,
                        width,
                        height,
                        Pixel::YUV420P,
                        out_w,
                        out_h,
                        ffmpeg_next::software::scaling::Flags::BILINEAR,
                    )
                    .map_err(|_| "failed to create scaler")?;

                    let fr = stream.avg_frame_rate();
                    let (fn_, fd) = if fr.numerator() > 0 && fr.denominator() > 0 {
                        (fr.numerator(), fr.denominator())
                    } else {
                        (30, 1)
                    };

                    // Allocate encoder context with the H.264 codec so codec_id
                    // and codec_type are set correctly (avcodec_alloc_context3
                    // with NULL leaves them unset, causing open to fail).
                    // SAFETY: avcodec_alloc_context3 is an FFmpeg allocation
                    // function. The `enc_codec` pointer was obtained from
                    // avcodec_find_encoder_by_name (a valid codec descriptor
                    // valid for the process lifetime). The returned AVCodecContext
                    // pointer is either null (allocation failure, handled) or
                    // a valid heap allocation owned by the caller.
                    // Context::wrap takes ownership and manages deallocation.
                    let enc_ctx = unsafe {
                        let ptr = ffmpeg_next::ffi::avcodec_alloc_context3(
                            enc_codec.as_ptr() as *mut ffmpeg_next::ffi::AVCodec
                        );
                        if ptr.is_null() {
                            return Err("failed to allocate encoder context");
                        }
                        ffmpeg_next::codec::Context::wrap(ptr, None)
                    };
                    let mut enc_video = enc_ctx
                        .encoder()
                        .video()
                        .map_err(|_| "failed to get encoder video interface")?;

                    enc_video.set_width(out_w);
                    enc_video.set_height(out_h);
                    enc_video.set_format(Pixel::YUV420P);
                    // Use millisecond time base so encoder output timestamps are in ms,
                    // matching the shared stage timeline and copied audio timestamps.
                    enc_video.set_time_base(ffmpeg_next::Rational::new(1, 1000));
                    enc_video.set_frame_rate(Some(ffmpeg_next::Rational::new(fn_, fd)));
                    enc_video.set_gop(profile.gop);
                    enc_video.set_max_b_frames(profile.bframes);
                    if profile.bitrate > 0 {
                        enc_video.set_bit_rate(profile.bitrate as usize);
                        if profile.max_bitrate > 0 {
                            enc_video.set_max_bit_rate(profile.max_bitrate as usize);
                        }
                    }

                    let mut opts = ffmpeg_next::Dictionary::new();
                    opts.set("preset", &profile.preset);
                    opts.set("tune", &profile.tune);
                    if profile.bitrate == 0 {
                        opts.set("crf", &profile.crf.to_string());
                    }

                    info!(
                        "[h264-tc] encoder: {}x{} preset={} tune={} crf={} bitrate={}",
                        out_w, out_h, profile.preset, profile.tune, profile.crf, profile.bitrate
                    );

                    let opened = enc_video
                        .open_as_with(enc_codec, opts)
                        .map_err(|_| "failed to open encoder")?;

                    scaler = Some(sw);
                    encoder = Some(opened);
                }

                let Some(enc) = encoder.as_mut() else {
                    continue;
                };
                let Some(sw) = scaler.as_mut() else { continue };

                // Use source-derived timestamp for the frame so encoded video shares
                // the same clock origin as copied audio.
                let source_pts_ms = dec_frame.pts().map(|pts| {
                    if video_tb.1 != 0 {
                        (pts as i128 * video_tb.0 as i128 * 1000 / video_tb.1 as i128) as i64
                    } else {
                        pts
                    }
                });

                if sw.run(&dec_frame, &mut enc_frame).is_err() {
                    continue;
                }
                enc_frame.set_pts(source_pts_ms);
                // Decoded frames may retain source I/P/B tags; clear them at the
                // transcode boundary so x264 uses this encoder's GOP/B-frame policy.
                enc_frame.set_kind(ffmpeg_next::util::picture::Type::None);

                if !send_h264_frame_with_drain(enc, &enc_frame, &mut enc_pkt, &mut normalizer) {
                    continue;
                }
                drain_h264_encoder_packets(enc, &mut enc_pkt, &mut normalizer);
            }

            if packet_sent || !drained_frame {
                break;
            }
        }
    }

    // Flush remaining encoder
    if let Some(enc) = encoder.as_mut() {
        let _ = enc.send_eof();
        drain_h264_encoder_packets(enc, &mut enc_pkt, &mut normalizer);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ring_buffer::Reader;
    use std::sync::Arc;

    fn canonical_hevc_ts_bytes() -> Vec<u8> {
        let fixture =
            crate::test_fixtures::canonical_h265_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
        std::fs::read(&fixture)
            .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()))
    }

    #[test]
    fn h264_transcoder_emits_packets_from_checked_in_hevc_fixture() {
        let fixture_bytes = canonical_hevc_ts_bytes();

        let input_queue = Arc::new(MemoryQueue::new());
        input_queue.write_sync(&fixture_bytes);
        input_queue.close();

        let output_ring = Arc::new(RingBuffer::new(16_384));
        let cancel = CancellationToken::new();

        run_ffmpeg_h264_stage(input_queue, output_ring.clone(), cancel, "test-hevc-h264")
            .unwrap_or_else(|e| panic!("HEVC->H.264 stage failed on checked-in fixture: {e}"));

        let mut reader = Reader::new("test_h264_tc_output".to_string(), output_ring);
        let mut packets = Vec::new();
        while let Ok(Some(packet)) = reader.pull() {
            packets.push(packet);
        }

        assert!(
            !packets.is_empty(),
            "HEVC->H.264 stage should emit packets for the checked-in HEVC fixture"
        );
        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "HEVC->H.264 stage should emit at least one video packet"
        );
        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Audio),
            "HEVC->H.264 stage should preserve audio packets"
        );
        assert!(
            packets
                .iter()
                .filter(|packet| packet.media_type == MediaType::Video)
                .all(|packet| {
                    packet.track_index == 0
                        && packet.format == PayloadFormat::Raw
                        && !packet.payload.is_empty()
                }),
            "transcoded video packets must remain non-empty raw track-0 packets"
        );
    }

    #[test]
    fn h264_transcoder_preserves_audio_from_checked_in_hevc_fixture() {
        let fixture_bytes = canonical_hevc_ts_bytes();

        let input_queue = Arc::new(MemoryQueue::new());
        input_queue.write_sync(&fixture_bytes);
        input_queue.close();

        let output_ring = Arc::new(RingBuffer::new(16_384));
        let cancel = CancellationToken::new();

        run_ffmpeg_h264_stage(
            input_queue,
            output_ring.clone(),
            cancel,
            "test-canonical-hevc-h264-audio",
        )
        .unwrap_or_else(|e| panic!("HEVC->H.264 stage failed on checked-in fixture: {e}"));

        let mut reader = Reader::new(
            "test_canonical_h264_tc_audio_output".to_string(),
            output_ring,
        );
        let mut packets = Vec::new();
        while let Ok(Some(packet)) = reader.pull() {
            packets.push(packet);
        }

        assert!(
            !packets.is_empty(),
            "HEVC->H.264 stage should emit packets for the checked-in HEVC fixture"
        );
        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "checked-in HEVC fixture should produce transcoded video packets"
        );
        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Audio),
            "checked-in HEVC fixture should preserve audio packets"
        );
    }

    #[test]
    fn h264_transcoder_emits_video_after_input_queue_is_closed() {
        let fixture_bytes = canonical_hevc_ts_bytes();

        let input_queue = Arc::new(MemoryQueue::new());
        input_queue.write_sync(&fixture_bytes);

        let output_ring = Arc::new(RingBuffer::new(16_384));
        let cancel = CancellationToken::new();
        let input_queue_for_thread = input_queue.clone();
        let output_ring_for_thread = output_ring.clone();
        let cancel_for_thread = cancel.clone();

        let handle = std::thread::spawn(move || {
            run_ffmpeg_h264_stage(
                input_queue_for_thread,
                output_ring_for_thread,
                cancel_for_thread,
                "test-canonical-hevc-h264-closed",
            )
        });

        // Close the queue so FFmpeg sees EOF, flushes the encoder, and
        // writes all output before the thread exits.
        input_queue.close();
        handle
            .join()
            .expect("HEVC->H.264 stage thread should join")
            .unwrap_or_else(|e| panic!("HEVC->H.264 stage failed on checked-in fixture: {e}"));

        let mut reader = Reader::new(
            "test_live_canonical_h264_tc_output".to_string(),
            output_ring,
        );
        let mut packets = Vec::new();
        while let Ok(Some(packet)) = reader.pull() {
            packets.push(packet);
        }

        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "HEVC->H.264 stage should emit video packets"
        );
        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video && packet.is_keyframe),
            "HEVC->H.264 stage should emit a keyframe"
        );
    }

    #[test]
    fn ffmpeg_error_classification_distinguishes_again_from_eof() {
        let again = ffmpeg_next::Error::Other {
            errno: ffmpeg_next::error::EAGAIN,
        };
        assert!(ffmpeg_error_is_again(again));
        assert!(!ffmpeg_error_is_eof(again));

        assert!(ffmpeg_error_is_eof(ffmpeg_next::Error::Eof));
        assert!(!ffmpeg_error_is_again(ffmpeg_next::Error::Eof));

        // An unrelated errno wrapped in `Other` must not be misclassified as
        // either retry-signal.
        let unrelated = ffmpeg_next::Error::Other {
            errno: ffmpeg_next::error::EINVAL,
        };
        assert!(!ffmpeg_error_is_again(unrelated));
        assert!(!ffmpeg_error_is_eof(unrelated));
    }

    #[test]
    fn errors_out_instead_of_panicking_on_empty_input() {
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Quiet);
        let input_queue = Arc::new(MemoryQueue::new());
        input_queue.close();

        let output_ring = Arc::new(RingBuffer::new(16_384));
        let cancel = CancellationToken::new();

        let result =
            run_ffmpeg_h264_stage(input_queue, output_ring, cancel, "test-empty-input-h264");

        assert!(
            result.is_err(),
            "an empty, immediately-closed input queue must surface an error, not panic or hang"
        );
    }

    #[test]
    fn errors_out_instead_of_panicking_on_garbage_input() {
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Quiet);
        let input_queue = Arc::new(MemoryQueue::new());
        input_queue.write_sync(&[0xFFu8; 4096]);
        input_queue.close();

        let output_ring = Arc::new(RingBuffer::new(16_384));
        let cancel = CancellationToken::new();

        let result =
            run_ffmpeg_h264_stage(input_queue, output_ring, cancel, "test-garbage-input-h264");

        assert!(
            result.is_err(),
            "non-MPEG-TS garbage input must surface an error, not panic or hang"
        );
    }

    /// A pipeline cancelled before the stage thread reads its first packet
    /// must bail out of the demux loop immediately rather than decoding and
    /// encoding a queue's worth of already-buffered valid input: cancellation
    /// is a bounded-work guarantee, not just an eventual-stop signal.
    #[test]
    fn exits_immediately_without_encoding_when_cancelled_before_first_packet() {
        let fixture_bytes = canonical_hevc_ts_bytes();

        let input_queue = Arc::new(MemoryQueue::new());
        input_queue.write_sync(&fixture_bytes);
        input_queue.close();

        let output_ring = Arc::new(RingBuffer::new(16_384));
        let cancel = CancellationToken::new();
        cancel.cancel();

        run_ffmpeg_h264_stage(
            input_queue,
            output_ring.clone(),
            cancel,
            "test-precancelled-h264",
        )
        .unwrap_or_else(|e| panic!("pre-cancelled stage must exit cleanly, not error: {e}"));

        let mut reader = Reader::new("test_precancelled_h264_output".to_string(), output_ring);
        assert!(
            reader
                .pull()
                .unwrap_or_else(|e| panic!("reader pull failed: {e}"))
                .is_none(),
            "a pre-cancelled stage must not decode or emit any packets"
        );
    }

    /// A stream that stops mid-packet (a dropped connection, not a clean
    /// close) is a different fault shape than pure garbage bytes: the demuxer
    /// sees a well-formed prefix before EOF cuts it off. The stage must
    /// finish without panicking or hanging, whether it surfaces an error or
    /// returns whatever it decoded before truncation.
    #[test]
    fn tolerates_input_truncated_mid_stream_without_panic_or_hang() {
        ffmpeg_next::util::log::set_level(ffmpeg_next::util::log::Level::Quiet);
        let fixture_bytes = canonical_hevc_ts_bytes();
        // Cut off partway through the fixture, deliberately not aligned to a
        // 188-byte TS packet boundary, to mimic a connection dropped
        // mid-packet rather than a clean stream shutdown.
        let truncated = &fixture_bytes[..fixture_bytes.len() / 5 + 37];

        let input_queue = Arc::new(MemoryQueue::new());
        input_queue.write_sync(truncated);
        input_queue.close();

        let output_ring = Arc::new(RingBuffer::new(16_384));
        let cancel = CancellationToken::new();

        let result = run_ffmpeg_h264_stage(
            input_queue,
            output_ring.clone(),
            cancel,
            "test-truncated-mid-stream-h264",
        );

        // No panic and no hang (the test itself would time out) is the core
        // guarantee. If decoding produced output before the cutoff, every
        // packet must still uphold the same shape invariants as a clean run.
        if result.is_ok() {
            let mut reader = Reader::new("test_truncated_h264_tc_output".to_string(), output_ring);
            let mut packets = Vec::new();
            while let Ok(Some(packet)) = reader.pull() {
                packets.push(packet);
            }
            assert!(
                packets
                    .iter()
                    .filter(|packet| packet.media_type == MediaType::Video)
                    .all(|packet| {
                        packet.track_index == 0
                            && packet.format == PayloadFormat::Raw
                            && !packet.payload.is_empty()
                    }),
                "any video packets emitted before truncation must remain \
                 well-formed raw track-0 packets"
            );
        }
    }
}
