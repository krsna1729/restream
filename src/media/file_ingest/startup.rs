use std::slice;
use std::sync::Arc;

use ffmpeg_next::{codec, format, media};
use tokio::runtime::Handle;

use crate::media::engine::{IngestRegistration, MediaEngine};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::ring_buffer::RingBuffer;

pub(super) fn prime_video_from_stream(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    ring_buffer: &Arc<RingBuffer>,
    registration: &IngestRegistration,
    input: &format::context::Input,
) {
    let Some(video_stream) = input
        .streams()
        .find(|stream| stream.parameters().medium() == media::Type::Video)
    else {
        return;
    };

    let Some((parameter_sets, sequence_header)) = h264_state_from_stream(&video_stream) else {
        return;
    };

    if let Some(preview_ring) = registration.preview_ring.load_full() {
        preview_ring.set_video_parameter_sets(parameter_sets.clone());
    }
    if registration.gate.state() == crate::media::input_gate::InputForwardState::Active {
        ring_buffer.set_video_parameter_sets(parameter_sets);
    }
    runtime_handle.block_on(async {
        engine
            .cache_ingest_session_sequence_header(registration, true, sequence_header)
            .await;
    });
}

pub(super) fn prime_container_metadata(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    pipeline_id: &str,
    registration: &IngestRegistration,
    input: &format::context::Input,
) {
    // The paced TS probe resolves only after every stream emits a packet.
    // Container headers unblock metadata consumers before that timeline point.
    let mut video_meta = None;
    let mut audio_tracks = Vec::new();
    let mut track_index = 0u32;

    for stream in input.streams() {
        let params = stream.parameters();
        match params.medium() {
            media::Type::Video if video_meta.is_none() => unsafe {
                let ptr = params.as_ptr();
                if ptr.is_null() {
                    continue;
                }
                let width = (*ptr).width.max(0) as u32;
                let height = (*ptr).height.max(0) as u32;
                if width > 0 && height > 0 {
                    video_meta = Some(VideoMeta {
                        codec: codec_name(params.id()),
                        width,
                        height,
                        fps: 0.0,
                        bw: None,
                        pid: None,
                        language: None,
                        title: None,
                        profile: None,
                        level: None,
                        pixel_format: None,
                    });
                }
            },
            media::Type::Audio => {
                let (sample_rate, channels) = unsafe {
                    let ptr = params.as_ptr();
                    if ptr.is_null() {
                        (0, 0)
                    } else {
                        (
                            (*ptr).sample_rate.max(0) as u32,
                            (*ptr).ch_layout.nb_channels.max(0) as u32,
                        )
                    }
                };
                if sample_rate > 0 && channels > 0 {
                    audio_tracks.push(AudioMeta {
                        codec: codec_name(params.id()),
                        sample_rate,
                        channels,
                        track_index,
                        ..Default::default()
                    });
                }
                track_index += 1;
            }
            _ => {}
        }
    }

    if video_meta.is_none() && audio_tracks.is_empty() {
        return;
    }

    let first_audio = audio_tracks.first().cloned();
    runtime_handle.block_on(async {
        engine
            .update_ingest_session_meta(pipeline_id, registration, video_meta, first_audio, None)
            .await;
        if !audio_tracks.is_empty() {
            engine
                .update_ingest_session_audio_tracks(pipeline_id, registration, audio_tracks)
                .await;
        }
    });
}

pub(super) fn prime_video_from_packet(
    engine: &Arc<MediaEngine>,
    runtime_handle: &Handle,
    ring_buffer: &Arc<RingBuffer>,
    registration: &IngestRegistration,
    payload: &[u8],
) -> bool {
    let Some(parameter_sets) = crate::media::codec::annexb_parameter_sets(payload) else {
        return false;
    };

    // The FLV sequence-header cache is H.264-only, but the ring must retain
    // HEVC parameter sets so downstream stages can start from a clean boundary.
    let sequence_header = crate::media::codec::build_avcc_sequence_header(&parameter_sets);
    if let Some(preview_ring) = registration.preview_ring.load_full() {
        preview_ring.set_video_parameter_sets(parameter_sets.clone());
    }
    if registration.gate.state() == crate::media::input_gate::InputForwardState::Active {
        ring_buffer.set_video_parameter_sets(parameter_sets);
    }
    if let Some(sequence_header) = sequence_header {
        runtime_handle.block_on(async {
            engine
                .cache_ingest_session_sequence_header(registration, true, sequence_header)
                .await;
        });
    }
    true
}

fn h264_state_from_stream(stream: &ffmpeg_next::Stream<'_>) -> Option<(Vec<u8>, bytes::Bytes)> {
    if stream.parameters().id() != codec::Id::H264 {
        return None;
    }

    let extradata = unsafe {
        let params = stream.parameters().as_ptr();
        if params.is_null() || (*params).extradata.is_null() || (*params).extradata_size <= 0 {
            return None;
        }
        slice::from_raw_parts(
            (*params).extradata.cast::<u8>(),
            (*params).extradata_size as usize,
        )
    };

    let annexb = if extradata.starts_with(&[0x00, 0x00, 0x01])
        || extradata.starts_with(&[0x00, 0x00, 0x00, 0x01])
    {
        extradata.to_vec()
    } else if extradata.first() == Some(&0x01) {
        let (_, annexb) = crate::media::codec::parse_avcc_config(extradata);
        annexb
    } else {
        Vec::new()
    };

    if annexb.is_empty() {
        return None;
    }

    let sequence_header = crate::media::codec::build_avcc_sequence_header(&annexb)?;
    Some((annexb, sequence_header))
}

fn codec_name(id: codec::Id) -> String {
    match id {
        codec::Id::H264 => "h264",
        codec::Id::HEVC => "hevc",
        codec::Id::AAC => "aac",
        other => return format!("{other:?}").to_ascii_lowercase(),
    }
    .to_string()
}
