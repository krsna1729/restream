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
use crate::media::ffmpeg::stage_output::StageOutputNormalizer;
use crate::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat, RingBuffer};
use crate::media::transcoder::InternalMemoryQueueSink;

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

/// Tokio task entry point for the shared H.265→H.264 transcoder.
///
/// 1. Waits for ingest metadata (video + audio tracks).
/// 2. Spawns a blocking OS thread for FFmpeg decode→encode.
/// 3. Forwards source RingBuffer packets to the MemoryQueue as MPEG-TS.
#[doc(hidden)]
#[allow(clippy::too_many_arguments)]
pub async fn start_h264_transcoder_inner(
    pipeline_id: String,
    _input_buffer: Arc<RingBuffer>,
    output_buffer: Arc<RingBuffer>,
    engine: Arc<crate::media::engine::MediaEngine>,
    cancel_token: CancellationToken,
    stage_key: StageKey,
    mut input_pump: StageInputPump,
    output_normalizer: StageOutputNormalizer,
) {
    let input_queue = Arc::new(MemoryQueue::new());
    engine
        .register_input_queue(stage_key.clone(), input_queue.clone())
        .await;

    // Spawn OS thread for FFmpeg decode→encode
    let iq_clone = input_queue.clone();
    let out_clone = output_buffer.clone();
    let cancel_clone = cancel_token.clone();
    let cancel_on_exit = cancel_token.clone();
    let pid = pipeline_id.clone();
    let handle = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            run_ffmpeg_h264_stage_with_normalizer(
                iq_clone,
                out_clone,
                cancel_clone,
                &pid,
                Some(output_normalizer),
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
    run_ffmpeg_h264_stage_with_normalizer(in_queue, out_ring, cancel, _pipeline_id, None)
}

fn run_ffmpeg_h264_stage_with_normalizer(
    in_queue: Arc<MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    cancel: CancellationToken,
    _pipeline_id: &str,
    existing_normalizer: Option<StageOutputNormalizer>,
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
    let mut normalizer = if let Some(n) = existing_normalizer {
        n
    } else {
        let normalizer_metrics = Arc::new(crate::media::stage_metrics::StageMetrics::new());
        crate::media::ffmpeg::stage_output::StageOutputNormalizer::new(
            out_ring,
            stream_count,
            normalizer_metrics,
        )
        .with_video_track_count(1)
    };

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

        // Video: decode H.265 → encode H.264
        if decoder.send_packet(&pkt).is_err() {
            continue;
        }

        let mut dec_frame = ffmpeg_next::frame::Video::empty();
        while decoder.receive_frame(&mut dec_frame).is_ok() {
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

            if enc.send_frame(&enc_frame).is_err() {
                continue;
            }
            while enc.receive_packet(&mut enc_pkt).is_ok() {
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
            }
        }
    }

    // Flush remaining encoder
    if let Some(enc) = encoder.as_mut() {
        let _ = enc.send_eof();
        while enc.receive_packet(&mut enc_pkt).is_ok() {
            let pts_ms = enc_pkt.pts().unwrap_or(0);
            let dts_ms = enc_pkt.dts().unwrap_or(pts_ms);
            normalizer.push(MediaPacket {
                media_type: MediaType::Video,
                track_index: 0,
                pts: pts_ms,
                dts: dts_ms,
                is_keyframe: enc_pkt.is_key(),
                format: PayloadFormat::Raw,
                payload: Bytes::from_owner(OwnedFfmpegPacket(enc_pkt.clone())),
            });
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::ring_buffer::Reader;
    use std::process::Command;
    use std::sync::Arc;

    fn extract_2v16a_hevc_ts_sample() -> Vec<u8> {
        let ffmpeg = crate::ffmpeg_extract::ensure_ffmpeg_extracted();
        let fixture = crate::test_fixtures::checked_in_fixture("media/colorbar-timer-2v16a.mp4")
            .expect("2v16a fixture should exist");
        let output = Command::new(ffmpeg)
            .args([
                "-v",
                "error",
                "-i",
                fixture.to_str().expect("utf-8 fixture path"),
                "-map",
                "0:v:1",
                "-map",
                "0:a",
                "-c",
                "copy",
                "-t",
                "1",
                "-f",
                "mpegts",
                "pipe:1",
            ])
            .output()
            .expect("spawn bundled ffmpeg for 2v16a HEVC sample extraction");
        assert!(
            output.status.success(),
            "ffmpeg 2v16a HEVC sample extraction failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !output.stdout.is_empty(),
            "2v16a HEVC TS sample should not be empty"
        );
        output.stdout
    }

    #[test]
    fn h264_transcoder_emits_packets_from_checked_in_hevc_fixture() {
        let fixture =
            crate::test_fixtures::canonical_h265_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
        let fixture_bytes = std::fs::read(&fixture)
            .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));

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
    fn h264_transcoder_emits_packets_from_2v16a_hevc_stream() {
        let fixture_bytes = extract_2v16a_hevc_ts_sample();

        let input_queue = Arc::new(MemoryQueue::new());
        input_queue.write_sync(&fixture_bytes);
        input_queue.close();

        let output_ring = Arc::new(RingBuffer::new(16_384));
        let cancel = CancellationToken::new();

        run_ffmpeg_h264_stage(
            input_queue,
            output_ring.clone(),
            cancel,
            "test-2v16a-hevc-h264",
        )
        .unwrap_or_else(|e| panic!("HEVC->H.264 stage failed on 2v16a sample: {e}"));

        let mut reader = Reader::new("test_2v16a_h264_tc_output".to_string(), output_ring);
        let mut packets = Vec::new();
        while let Ok(Some(packet)) = reader.pull() {
            packets.push(packet);
        }

        assert!(
            !packets.is_empty(),
            "HEVC->H.264 stage should emit packets for the 2v16a HEVC sample"
        );
        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Video),
            "2v16a HEVC sample should produce transcoded video packets"
        );
        assert!(
            packets
                .iter()
                .any(|packet| packet.media_type == MediaType::Audio),
            "2v16a HEVC sample should preserve audio packets"
        );
    }

    #[test]
    fn h264_transcoder_emits_video_after_input_queue_is_closed() {
        let fixture_bytes = extract_2v16a_hevc_ts_sample();

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
                "test-2v16a-hevc-h264",
            )
        });

        // Close the queue so FFmpeg sees EOF, flushes the encoder, and
        // writes all output before the thread exits.
        input_queue.close();
        handle
            .join()
            .expect("HEVC->H.264 stage thread should join")
            .unwrap_or_else(|e| panic!("HEVC->H.264 stage failed on 2v16a sample: {e}"));

        let mut reader = Reader::new("test_live_2v16a_h264_tc_output".to_string(), output_ring);
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
}
