//! What a MoQ broadcast needs before its first frame: the tracks to publish
//! and their decoder configurations (the catalog inputs), plus, for an
//! egress leaf, the broadcast name and the relay trust material.
//!
//! [`publish_startup`] assembles the media part on Tokio (it queries the
//! engine's ingest metadata); the application adds the egress part and
//! writes the snapshot into the shared source before dispatching
//! `EgressCommand::Add`, and the origin builds it once per served pipeline.
//! Shard and listener threads only read it.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use bytes::Bytes;

use crate::media::egress::command::OutputId;
use crate::media::engine::MediaEngine;
use crate::media::rtmp::{output_ring_video_codec_kind, resolved_output_audio_tracks};
use restream_core::codec::build_aac_sequence_header;
use restream_core::codec::parse_avcc_config;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_domain::domain::output_spec::VideoCodecKind;
use restream_moq::media::AudioConfig;
use restream_moq::media::TrackContainer;
use restream_moq::media::VideoCodec;
use restream_moq::media::VideoConfig;
use restream_sync::RingBuffer;

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct MoqVideoStartup {
    pub(crate) codec: VideoCodec,
    /// The configuration known at startup; `None` when the ring has
    /// produced none yet (the broadcast then waits for its first keyframe).
    pub(crate) config: Option<VideoConfig>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MoqAudioStartup {
    /// The ring audio track published (one AAC track per broadcast).
    pub(crate) track_index: u32,
    pub(crate) config: AudioConfig,
}

#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct MoqPublishStartup {
    pub(crate) video: Option<MoqVideoStartup>,
    pub(crate) audio: Option<MoqAudioStartup>,
    /// The broadcast name announced to the relay (egress only).
    pub(crate) broadcast: String,
    /// Extra trusted CA certificates (PEM) from the URL's `ca` file.
    pub(crate) extra_ca_pem: Option<Vec<u8>>,
}

/// The tracks and configurations of `ring`, which carries `pipeline_id`'s
/// media: H.264 or HEVC video and the first AAC track.
pub(crate) async fn publish_startup(
    engine: &MediaEngine,
    pipeline_id: &str,
    ring: &Arc<RingBuffer>,
) -> MoqPublishStartup {
    let codec = output_ring_video_codec_kind(engine, pipeline_id, ring).await;
    let (ingest_video_header, ingest_audio_header) = engine.get_sequence_headers(pipeline_id).await;
    let video_meta = engine
        .with_active_ingest(pipeline_id, |ingest| ingest.metadata().video)
        .await
        .flatten();
    let video = startup_video(codec, video_meta.as_ref(), ring, ingest_video_header);
    let audio = resolved_output_audio_tracks(engine, pipeline_id, ring)
        .await
        .into_iter()
        .find(|track| track.codec.eq_ignore_ascii_case("aac"))
        .map(|track| startup_audio(&track, ingest_audio_header));
    MoqPublishStartup {
        video,
        audio,
        ..MoqPublishStartup::default()
    }
}

/// The video track: H.264 or HEVC only. The configuration comes from the
/// ring's recorded parameter sets, else the ingest's sequence header —
/// unless the ring is a fresh transform with no unit yet, whose
/// configuration the source's would misdescribe.
pub(crate) fn startup_video(
    codec: VideoCodecKind,
    meta: Option<&VideoMeta>,
    ring: &RingBuffer,
    ingest_sequence_header: Option<Bytes>,
) -> Option<MoqVideoStartup> {
    let codec = match codec {
        VideoCodecKind::H264 => VideoCodec::H264,
        VideoCodecKind::Hevc => VideoCodec::H265,
        _ => return None,
    };
    let config = if let Some(config) = ring.video_config() {
        VideoConfig::from_parameter_sets(codec, &config.bytes)
    } else if ring.get_write_idx() == 0 && !ring.codec_hint_str().is_empty() {
        None
    } else {
        ingest_sequence_header.and_then(|header| flv_video_config(codec, &header))
    };
    let config = config.map(|config| VideoConfig {
        coded_width: config
            .coded_width
            .or(meta.map(|meta| meta.width).filter(|width| *width > 0)),
        coded_height: config
            .coded_height
            .or(meta.map(|meta| meta.height).filter(|height| *height > 0)),
        framerate: config
            .framerate
            .or(meta.map(|meta| meta.fps).filter(|fps| *fps > 0.0)),
        ..config
    });
    Some(MoqVideoStartup { codec, config })
}

/// The configuration an FLV sequence header carries: a legacy AVC header
/// (`0x17 0x00`) or an enhanced-RTMP `hvc1` SequenceStart.
fn flv_video_config(codec: VideoCodec, tag: &Bytes) -> Option<VideoConfig> {
    match (codec, tag.as_ref()) {
        (VideoCodec::H264, [0x17, 0x00, _, _, _, record @ ..]) if !record.is_empty() => {
            VideoConfig::from_parameter_sets(codec, &parse_avcc_config(record).1)
        }
        (VideoCodec::H265, [tag_byte, b'h', b'v', b'c', b'1', record @ ..])
            if tag_byte & 0x8F == 0x80 && !record.is_empty() =>
        {
            let config = VideoConfig {
                codec,
                description: Some(tag.slice(5..)),
                coded_width: None,
                coded_height: None,
                framerate: None,
                jitter: None,
                container: TrackContainer::Legacy,
            };
            let (sets, _) = config.parameter_sets()?;
            let parsed = VideoConfig::from_parameter_sets(codec, &sets)?;
            Some(VideoConfig {
                description: config.description,
                ..parsed
            })
        }
        _ => None,
    }
}

/// The AAC track, with the ingest's AudioSpecificConfig when it has one (it
/// keeps profile and extensions), else one built from the track metadata.
pub(crate) fn startup_audio(
    track: &AudioMeta,
    ingest_sequence_header: Option<Bytes>,
) -> MoqAudioStartup {
    let flv_header = ingest_sequence_header
        .filter(|header| matches!(header.as_ref(), [0xaf, 0x00, _, ..]))
        .unwrap_or_else(|| build_aac_sequence_header(track.sample_rate, track.channels));
    MoqAudioStartup {
        track_index: track.track_index,
        config: AudioConfig {
            sample_rate: track.sample_rate,
            channels: track.channels,
            asc: Some(flv_header.slice(2..)),
            container: TrackContainer::Legacy,
        },
    }
}

/// Startup snapshots by output, shared (cloned) across every shard of one
/// MoQ fabric runtime, since any output can land on any shard.
#[derive(Debug, Clone, Default)]
pub(crate) struct SharedMoqStartupSource {
    pending: Arc<Mutex<HashMap<OutputId, MoqPublishStartup>>>,
}

impl SharedMoqStartupSource {
    pub(crate) fn set(&self, output_id: OutputId, startup: MoqPublishStartup) {
        restream_sync::lock::lock(&self.pending).insert(output_id, startup);
    }

    pub(crate) fn remove(&self, output_id: &OutputId) {
        restream_sync::lock::lock(&self.pending).remove(output_id);
    }

    /// The snapshot for `output_id`; it stays until the output is removed,
    /// so a reconnect of the same generation finds it again.
    pub(crate) fn take_startup(&self, output_id: &OutputId) -> Option<MoqPublishStartup> {
        restream_sync::lock::lock(&self.pending)
            .get(output_id)
            .cloned()
    }
}
