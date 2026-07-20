//! In-process FFmpeg transcoder — demuxes input MPEG-TS, applies stream filtering,
//! and pushes `MediaPacket`s directly to the output `RingBuffer`. Uses a single
//! `MemoryQueue` for input (source `RingBuffer` → TsMuxer → FFmpeg demux).
//!
//! Audio routing: compound encodings like `720p+atrack:0,1` or `source+remap:0:1`
//! are parsed to select/remap audio streams.

use crate::domain::output_spec::StagePresetSpec;
use crate::domain::stage::{StageKey, StageKind};
use crate::media::ffmpeg::backend::{BackendError, StageRunContext};
use crate::media::ffmpeg::stage_input::StageInputPump;
use crate::media::ffmpeg::stage_output::{StageOutputNormalizer, StageOutputSink};
use crate::media::ffmpeg::stage_plan::{FfmpegStagePlan, VideoCodecKind};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::{MEDIA_PRODUCER_BATCH_PACKETS, RingBuffer};

use crate::media::stage_metrics::StageMetrics;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use tracing::{debug, error};

mod audio_router;

#[cfg(test)]
use audio_router::route_audio_packet;
pub use audio_router::{apply_audio_routing, start_audio_router};

/// Zero-copy wrapper: holds an `ffmpeg_next::Packet` so `bytes::Bytes::from_owner`
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

use crate::domain::audio_routing::{AudioRouting, parse_audio_routing};

/// Byte sink that writes MPEG-TS batches into an in-process `MemoryQueue`.
pub(crate) struct InternalMemoryQueueSink {
    queue: Arc<crate::media::avio::MemoryQueue>,
    cancel: CancellationToken,
}

impl InternalMemoryQueueSink {
    pub(crate) fn new(
        queue: Arc<crate::media::avio::MemoryQueue>,
        cancel: CancellationToken,
    ) -> Self {
        Self { queue, cancel }
    }
}

impl crate::media::ffmpeg::stage_input::StageByteSink for InternalMemoryQueueSink {
    async fn write_ts(&mut self, bytes: &[u8], _cancel: &CancellationToken) -> Result<(), String> {
        if bytes.is_empty() {
            return Ok(());
        }
        if !self.queue.write_cancellable(bytes, &self.cancel).await {
            return Err("input queue closed or cancelled".into());
        }
        Ok(())
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_internal_video_stage(
    pipeline_id: String,
    preset: String,
    engine: Arc<crate::media::engine::MediaEngine>,
    cancel_token: CancellationToken,
    stage_key: StageKey,
    mut input_pump: crate::media::ffmpeg::stage_input::StageInputPump,
    output_normalizer: StageOutputNormalizer,
    needs_scale: bool,
    output_codec: VideoCodecKind,
) {
    let input_queue = Arc::new(crate::media::avio::MemoryQueue::new_with_capacity(
        engine.config.avio_capacity,
    ));
    let stage_lifecycle = engine
        .get_or_create_stage_lifecycle(
            stage_key.clone(),
            crate::media::stage_lifecycle::StagePhase::Registered,
        )
        .await;
    engine
        .register_input_queue(stage_key.clone(), input_queue.clone())
        .await;
    stage_lifecycle.transition(crate::media::stage_lifecycle::StagePhase::BackendSpawned {
        backend: crate::media::stage_lifecycle::StageBackendKind::InternalFfmpeg,
        pid: None,
    });

    // Spawn thread to run FFmpeg processing: demux input MPEG-TS, push packets
    // directly to the output RingBuffer (no output mux/demux round-trip).
    let input_queue_clone = input_queue.clone();
    let preset_clone = preset.clone();
    let output_codec_clone = output_codec.clone();
    let cancel_token_clone = cancel_token.clone();
    let cancel_on_exit = cancel_token.clone();
    let pipeline_id_clone = pipeline_id.clone();
    let stage_lifecycle_for_thread = stage_lifecycle.clone();
    let handle = std::thread::spawn(move || {
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            if needs_scale {
                run_ffmpeg_transcode_with_scale_with_normalizer(
                    input_queue_clone,
                    &preset_clone,
                    output_codec_clone,
                    cancel_token_clone,
                    StageOutputSink::Existing(Box::new(output_normalizer)),
                )
            } else {
                run_ffmpeg_transcoder_stage_with_normalizer(
                    input_queue_clone,
                    &preset_clone,
                    cancel_token_clone,
                    StageOutputSink::Existing(Box::new(output_normalizer)),
                )
            }
        }));
        match result {
            Ok(Err(e)) => {
                stage_lifecycle_for_thread.record_error(e);
                error!(pipeline_id = %pipeline_id_clone, preset = %preset_clone, err = ?e, "FFmpeg transcode thread failed")
            }
            Err(_) => {
                stage_lifecycle_for_thread.record_error("FFmpeg transcode thread panicked");
                error!(pipeline_id = %pipeline_id_clone, preset = %preset_clone, "FFmpeg transcode thread panicked")
            }
            _ => {}
        }
        cancel_on_exit.cancel();
    });
    engine.register_os_thread(handle);

    let mut queue_sink = InternalMemoryQueueSink::new(input_queue.clone(), cancel_token.clone());
    if let Err(e) = input_pump.pump_to(&mut queue_sink, &cancel_token).await {
        if cancel_token.is_cancelled() && e.contains("closed or cancelled") {
            debug!(
                pipeline_id = %pipeline_id,
                preset = %preset,
                "internal transcoder shared pump stopped during cancellation: {}",
                e
            );
        } else {
            error!(
                pipeline_id = %pipeline_id,
                preset = %preset,
                "internal transcoder shared pump failed: {}",
                e
            );
        }
    }

    input_queue.close();
    engine.remove_input_queue(&stage_key).await;
    engine.remove_stage_metrics(&stage_key).await;
    engine.remove_stage_lifecycle(&stage_key).await;
    engine.remove_stage_runtime(&stage_key).await;
    engine
        .runtime
        .event_log
        .emit(crate::events::EventKind::StageStopped {
            pipeline_id: pipeline_id.clone(),
            encoding: preset.clone(),
        });
}

/// Backend entry point for the in-process FFmpeg adapter.
pub async fn run_internal_ffmpeg_backend(
    plan: FfmpegStagePlan,
    input_pump: StageInputPump,
    output_normalizer: StageOutputNormalizer,
    ctx: StageRunContext,
) -> Result<(), BackendError> {
    if matches!(
        plan.video,
        crate::media::ffmpeg::stage_plan::VideoStageOp::CodecEdge { .. }
    ) {
        crate::media::h264_transcoder::run_h264_codec_edge_stage(
            ctx.pipeline_id.clone(),
            ctx.engine,
            ctx.cancel,
            ctx.stage_key,
            input_pump,
            output_normalizer,
        )
        .await;
    } else {
        let needs_scale = matches!(
            plan.video,
            crate::media::ffmpeg::stage_plan::VideoStageOp::ScalePreset { .. }
        );
        let preset = internal_video_stage_preset_name(&plan, &ctx.stage_key.kind);
        run_internal_video_stage(
            ctx.pipeline_id,
            preset,
            ctx.engine,
            ctx.cancel,
            ctx.stage_key,
            input_pump,
            output_normalizer,
            needs_scale,
            plan.output_codec,
        )
        .await;
    }
    Ok(())
}

fn internal_video_stage_preset_name(plan: &FfmpegStagePlan, stage_kind: &StageKind) -> String {
    match &plan.video {
        crate::media::ffmpeg::stage_plan::VideoStageOp::ScalePreset { preset } => preset.clone(),
        _ => stage_kind.to_string(),
    }
}

#[cfg(test)]
#[allow(clippy::items_after_test_module)]
mod tests;

/// Execute the FFmpeg-backed processing stage used by `start_transcoder`.
///
/// Demuxes input MPEG-TS from `in_queue`, applies stream filtering (audio
/// routing), and pushes `MediaPacket`s directly to the output `RingBuffer`.
/// No output muxer or demux thread needed.
#[doc(hidden)]
pub fn run_ffmpeg_transcoder_stage(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    preset: &str,
    token: CancellationToken,
) -> Result<(), &'static str> {
    run_ffmpeg_transcoder_stage_with_metrics(in_queue, out_ring, preset, token, None)
}

fn run_ffmpeg_transcoder_stage_with_metrics(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    preset: &str,
    token: CancellationToken,
    metrics: Option<Arc<StageMetrics>>,
) -> Result<(), &'static str> {
    run_ffmpeg_transcoder_stage_with_normalizer(
        in_queue,
        preset,
        token,
        StageOutputSink::from_ring(out_ring, metrics),
    )
}

fn run_ffmpeg_transcoder_stage_with_normalizer(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    preset: &str,
    token: CancellationToken,
    output_sink: StageOutputSink,
) -> Result<(), &'static str> {
    use crate::media::avio::CustomInput;

    let stage_spec = StagePresetSpec::parse(preset);
    let video_preset = stage_spec.video_encoding();
    let audio_routing = stage_spec
        .audio_operation()
        .map(crate::domain::audio_routing::parse_audio_operation)
        .unwrap_or_else(|| parse_audio_routing(preset));

    let mut custom_input = CustomInput::new(&*in_queue)?;
    let ictx = custom_input
        .input
        .as_mut()
        .ok_or("Failed to get CustomInput context")?;

    let mut audio_stream_index = 0usize;
    let mut audio_out_index = 0u32;
    let mut stream_meta: Vec<Option<(MediaType, u32)>> = Vec::new();

    let _force_h264 = video_preset == "h264";

    for stream in ictx.streams() {
        let medium = stream.parameters().medium();
        if medium == ffmpeg_next::media::Type::Video {
            stream_meta.push(Some((MediaType::Video, 0)));
        } else if medium == ffmpeg_next::media::Type::Audio {
            let include = match &audio_routing {
                AudioRouting::Passthrough => true,
                AudioRouting::SelectTracks { tracks } => tracks.contains(&audio_stream_index),
                AudioRouting::Remap { track, .. } => audio_stream_index == *track,
                AudioRouting::Downmix { track } => audio_stream_index == *track,
            };
            if include {
                stream_meta.push(Some((MediaType::Audio, audio_out_index)));
                audio_out_index += 1;
            } else {
                stream_meta.push(None);
            }
            audio_stream_index += 1;
        } else {
            stream_meta.push(None);
        }
    }

    let stream_count = stream_meta.iter().filter(|m| m.is_some()).count().max(1);
    let mut normalizer = output_sink.into_normalizer(stream_count);

    let mut batch: Vec<MediaPacket> = Vec::with_capacity(MEDIA_PRODUCER_BATCH_PACKETS);
    for (stream, packet) in ictx.packets() {
        if token.is_cancelled() {
            break;
        }

        let idx = stream.index();
        let Some(&Some((media_type, track_index))) = stream_meta.get(idx) else {
            continue;
        };

        let tb = stream.time_base();
        // Skip packets with AV_NOPTS_VALUE — using 0 on a long-running stream
        // would cause a massive backward jump through DtsEnforcer (M7 fix).
        let Some(pts) = packet.pts() else { continue };
        let dts = packet.dts().unwrap_or(pts);
        let pts_ms = if tb.1 != 0 {
            // i128 avoids f64 precision loss for large pts values (e.g. after
            // hours of streaming at 90 kHz: pts ≈ 3×10¹¹, f64 has only 53-bit
            // mantissa ≈ 9×10¹⁵ exact range but loses sub-ms precision before that).
            (pts as i128 * tb.0 as i128 * 1000 / tb.1 as i128) as i64
        } else {
            pts
        };
        let dts_ms = if tb.1 != 0 {
            (dts as i128 * tb.0 as i128 * 1000 / tb.1 as i128) as i64
        } else {
            dts
        };
        let is_keyframe = packet.is_key();

        let output_packet = MediaPacket {
            media_type,
            track_index,
            pts: pts_ms,
            dts: dts_ms,
            is_keyframe,
            format: PayloadFormat::Raw,
            payload: bytes::Bytes::from_owner(OwnedFfmpegPacket(packet)),
        };
        batch.push(output_packet);
        if batch.len() >= MEDIA_PRODUCER_BATCH_PACKETS {
            normalizer.push_batch(&mut batch);
        }
    }
    if !batch.is_empty() {
        normalizer.push_batch(&mut batch);
    }

    Ok(())
}

/// Real decode -> scale -> encode transcoder stage.
pub fn run_ffmpeg_transcode_with_scale(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    video_preset: &str,
    token: CancellationToken,
) -> Result<(), &'static str> {
    run_ffmpeg_transcode_with_scale_with_metrics(in_queue, out_ring, video_preset, token, None)
}

fn run_ffmpeg_transcode_with_scale_with_metrics(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    out_ring: Arc<RingBuffer>,
    video_preset: &str,
    token: CancellationToken,
    metrics: Option<Arc<StageMetrics>>,
) -> Result<(), &'static str> {
    run_ffmpeg_transcode_with_scale_with_normalizer(
        in_queue,
        video_preset,
        VideoCodecKind::H264,
        token,
        StageOutputSink::from_ring(out_ring, metrics),
    )
}

fn internal_video_encoder_id(output_codec: &VideoCodecKind) -> ffmpeg_next::codec::Id {
    match output_codec {
        VideoCodecKind::H264 => ffmpeg_next::codec::Id::H264,
        VideoCodecKind::Hevc => ffmpeg_next::codec::Id::HEVC,
    }
}

fn internal_video_encoder(
    output_codec: &VideoCodecKind,
) -> Result<ffmpeg_next::Codec, &'static str> {
    match internal_video_encoder_id(output_codec) {
        ffmpeg_next::codec::Id::H264 => {
            ffmpeg_next::codec::encoder::find(ffmpeg_next::codec::Id::H264)
                .ok_or("no H.264 encoder")
        }
        ffmpeg_next::codec::Id::HEVC => ffmpeg_next::codec::encoder::find_by_name("libx265")
            .or_else(|| ffmpeg_next::codec::encoder::find(ffmpeg_next::codec::Id::HEVC))
            .ok_or("no HEVC/H.265 encoder"),
        _ => Err("Unsupported video codec for internal transcoding"),
    }
}

#[cfg(test)]
fn internal_video_encoder_id_for_plan(plan: &FfmpegStagePlan) -> ffmpeg_next::codec::Id {
    internal_video_encoder_id(&plan.output_codec)
}

fn run_ffmpeg_transcode_with_scale_with_normalizer(
    in_queue: Arc<crate::media::avio::MemoryQueue>,
    video_preset: &str,
    output_codec: VideoCodecKind,
    token: CancellationToken,
    output_sink: StageOutputSink,
) -> Result<(), &'static str> {
    use crate::media::avio::CustomInput;
    use ffmpeg_next::format::Pixel;

    let mut custom = CustomInput::new(&*in_queue)?;
    let ictx = custom
        .input
        .as_mut()
        .ok_or("Failed to get CustomInput context")?;

    // Identify streams
    let video_idx = ictx
        .streams()
        .find(|s| s.parameters().medium() == ffmpeg_next::media::Type::Video)
        .map(|s| s.index())
        .ok_or("no video stream")?;

    // Build stream metadata (same pattern as h264_transcoder)
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

    // Look up target dimensions
    let profile = crate::media::profiles::get_blocking(video_preset);

    let target_w = profile.width;
    let target_h = profile.height;
    let skip_scaling = target_w == 0;

    let enc_codec = internal_video_encoder(&output_codec)?;

    let stream_count = 1 + audio_track_counter as usize;
    let mut normalizer = output_sink.into_normalizer(stream_count);

    let mut encoder: Option<ffmpeg_next::codec::encoder::video::Encoder> = None;
    let mut scaler: Option<ffmpeg_next::software::scaling::Context> = None;
    let mut enc_frame = ffmpeg_next::frame::Video::empty();
    let mut enc_pkt = ffmpeg_next::Packet::empty();

    for (stream, pkt) in ictx.packets() {
        if token.is_cancelled() {
            break;
        }

        let idx = stream.index();

        // Audio copy
        if stream.parameters().medium() == ffmpeg_next::media::Type::Audio {
            let Some(&Some((media_type, track_index))) = stream_meta.get(idx) else {
                continue;
            };
            let tb = stream.time_base();
            // Skip packets with AV_NOPTS_VALUE (M7 fix — same as passthrough path).
            let Some(pts) = pkt.pts() else { continue };
            let dts_val = pkt.dts().unwrap_or(pts);
            let pts_ms = if tb.1 != 0 {
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
            let output_packet = MediaPacket {
                media_type,
                track_index,
                pts: pts_ms,
                dts: dts_ms,
                is_keyframe,
                format: PayloadFormat::Raw,
                payload: bytes::Bytes::from_owner(OwnedFfmpegPacket(pkt)),
            };
            normalizer.push(output_packet);
            continue;
        }

        if idx != video_idx {
            continue;
        }

        let video_tb = stream.time_base();
        if decoder.send_packet(&pkt).is_err() {
            continue;
        }

        let mut dec_frame = ffmpeg_next::frame::Video::empty();
        while decoder.receive_frame(&mut dec_frame).is_ok() {
            // Lazy encoder + scaler init
            if encoder.is_none() {
                let width = decoder.width();
                let height = decoder.height();
                let in_fmt = dec_frame.format();

                let out_w = if target_w > 0 { target_w } else { width };
                let out_h = if target_h > 0 { target_h } else { height };

                let need_scaling = !skip_scaling && (out_w != width || out_h != height)
                    || in_fmt != Pixel::YUV420P;
                if need_scaling {
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
                    scaler = Some(sw);
                }

                let fr = stream.avg_frame_rate();
                let (fn_, fd) = if fr.numerator() > 0 && fr.denominator() > 0 {
                    (fr.numerator(), fr.denominator())
                } else {
                    (30, 1)
                };

                // SAFETY: avcodec_alloc_context3 allocates an FFmpeg
                // AVCodecContext. The `enc_codec` pointer was obtained from
                // avcodec_find_encoder_by_name and is valid for the process
                // lifetime. The returned pointer is either null (handled) or
                // a valid heap allocation. Context::wrap takes ownership.
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

                let bitrate = if profile.bitrate > 0 {
                    profile.bitrate as usize
                } else {
                    (out_w * out_h) as usize * 3
                };
                enc_video.set_bit_rate(bitrate);
                if profile.max_bitrate > 0 {
                    enc_video.set_max_bit_rate(profile.max_bitrate as usize);
                }

                let mut opts = ffmpeg_next::Dictionary::new();
                opts.set("preset", &profile.preset);
                opts.set("tune", &profile.tune);
                if profile.bitrate == 0 {
                    opts.set("crf", &profile.crf.to_string());
                }

                let opened = enc_video
                    .open_as_with(enc_codec, opts)
                    .map_err(|_| "failed to open encoder")?;
                encoder = Some(opened);
            }

            let Some(enc) = encoder.as_mut() else {
                continue;
            };

            // Use source-derived timestamp for the frame so encoded video shares
            // the same clock origin as copied audio.
            let source_pts_ms = dec_frame.pts().map(|pts| {
                if video_tb.1 != 0 {
                    (pts as i128 * video_tb.0 as i128 * 1000 / video_tb.1 as i128) as i64
                } else {
                    pts
                }
            });

            let frame_to_encode = if let Some(ref mut sw) = scaler {
                if sw.run(&dec_frame, &mut enc_frame).is_err() {
                    continue;
                }
                enc_frame.set_pts(source_pts_ms);
                // Drop source picture-type hints so the new encoder can choose
                // GOP/B-frame placement from its own settings.
                enc_frame.set_kind(ffmpeg_next::util::picture::Type::None);
                &enc_frame
            } else {
                dec_frame.set_pts(source_pts_ms);
                // Even without scaling, a decode/re-encode stage should not
                // preserve source I/P/B tags across the encoder boundary.
                dec_frame.set_kind(ffmpeg_next::util::picture::Type::None);
                &dec_frame
            };

            if enc.send_frame(frame_to_encode).is_err() {
                continue;
            }

            while enc.receive_packet(&mut enc_pkt).is_ok() {
                // Skip packets with AV_NOPTS_VALUE — same M7 rationale as the
                // passthrough path: defaulting to 0 on a long-running stream
                // would cause a massive backward jump through DtsEnforcer.
                let Some(pts_ms) = enc_pkt.pts() else {
                    continue;
                };
                let dts_ms = enc_pkt.dts().unwrap_or(pts_ms);
                // enc_pkt is reused across iterations; clone() calls av_packet_ref (refcount
                // bump only, no data copy) so the ring buffer holds the AVBufferRef alive.
                let output_packet = MediaPacket {
                    media_type: MediaType::Video,
                    track_index: 0,
                    pts: pts_ms,
                    dts: dts_ms,
                    is_keyframe: enc_pkt.is_key(),
                    format: PayloadFormat::Raw,
                    payload: bytes::Bytes::from_owner(OwnedFfmpegPacket(enc_pkt.clone())),
                };
                normalizer.push(output_packet);
            }
        }
    }

    if let Some(enc) = encoder.as_mut() {
        let _ = enc.send_eof();
        while enc.receive_packet(&mut enc_pkt).is_ok() {
            // Same M7 rationale as above: skip rather than default to 0.
            let Some(pts_ms) = enc_pkt.pts() else {
                continue;
            };
            let dts_ms = enc_pkt.dts().unwrap_or(pts_ms);
            let output_packet = MediaPacket {
                media_type: MediaType::Video,
                track_index: 0,
                pts: pts_ms,
                dts: dts_ms,
                is_keyframe: enc_pkt.is_key(),
                format: PayloadFormat::Raw,
                payload: bytes::Bytes::from_owner(OwnedFfmpegPacket(enc_pkt.clone())),
            };
            normalizer.push(output_packet);
        }
    }

    Ok(())
}
