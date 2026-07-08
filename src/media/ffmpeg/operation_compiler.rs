//! Compile an `FfmpegStagePlan` into a backend-neutral `FfmpegOperation`.

use crate::media::ffmpeg::operation::{
    AudioOperation, FfmpegOperation, VideoCodec, VideoEncoderSettings,
};
use crate::media::ffmpeg::stage_plan::{AudioStageOp, CodecEdgeOp, FfmpegStagePlan, VideoStageOp};

/// Compile a stage plan into the operation consumed by both FFmpeg backends.
pub fn compile_operation(plan: &FfmpegStagePlan) -> FfmpegOperation {
    let input_codec = match plan.input.codec_hint {
        crate::media::ffmpeg::stage_plan::VideoCodecKind::Hevc => VideoCodec::Hevc,
        crate::media::ffmpeg::stage_plan::VideoCodecKind::H264 => VideoCodec::H264,
    };

    let output_codec = match plan.output_codec {
        crate::media::ffmpeg::stage_plan::VideoCodecKind::Hevc => VideoCodec::Hevc,
        crate::media::ffmpeg::stage_plan::VideoCodecKind::H264 => VideoCodec::H264,
    };

    let scale = match &plan.video {
        VideoStageOp::ScalePreset { preset } | VideoStageOp::Preview { preset } => {
            let p = crate::media::profiles::try_get_cached(preset);
            if p.width > 0 && p.height > 0 {
                Some((p.width, p.height))
            } else {
                None
            }
        }
        _ => None,
    };

    let video_encoder = match &plan.video {
        VideoStageOp::Passthrough => VideoEncoderSettings::default(),
        VideoStageOp::ScalePreset { preset } => {
            encoder_settings_for_preset(preset, output_codec.clone())
        }
        VideoStageOp::CodecEdge { op } => match op {
            CodecEdgeOp::HevcToH264 => encoder_settings_for_preset("h264", VideoCodec::H264),
        },
        VideoStageOp::Preview { preset } => {
            encoder_settings_for_preset(preset, output_codec.clone())
        }
    };

    let audio = match &plan.audio {
        AudioStageOp::Passthrough => AudioOperation::CopyAll,
        AudioStageOp::Drop => AudioOperation::Drop,
        AudioStageOp::SelectTracks(tracks) => AudioOperation::SelectTracks(tracks.clone()),
        AudioStageOp::Downmix { track } => AudioOperation::Downmix { track: *track },
        AudioStageOp::Remap { track, channels } => AudioOperation::Remap {
            track: *track,
            channels: channels.clone(),
        },
    };

    FfmpegOperation {
        input_codec,
        output_codec,
        scale,
        video_encoder,
        audio,
        video_meta: plan.input.video_meta.clone(),
        audio_tracks: plan.input.audio_tracks.clone(),
    }
}

fn encoder_settings_for_preset(preset: &str, output_codec: VideoCodec) -> VideoEncoderSettings {
    let profile = crate::media::profiles::try_get_cached(preset);
    let (width, height) = if profile.width > 0 && profile.height > 0 {
        (profile.width, profile.height)
    } else {
        (0, 0)
    };

    VideoEncoderSettings {
        codec: output_codec,
        width,
        height,
        bitrate: profile.bitrate as usize,
        max_bitrate: profile.max_bitrate as usize,
        gop: profile.gop,
        bframes: profile.bframes,
        preset: profile.preset.clone(),
        tune: profile.tune.clone(),
        crf: profile.crf,
        use_crf: profile.bitrate == 0,
    }
}
