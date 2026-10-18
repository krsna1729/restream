//! Ring packets → hang frames for the moq-lite profile, the shared media
//! model of [`crate::media::moq::media`]:
//!
//! - Video: one access unit per frame, 4-byte NAL lengths, parameter sets
//!   out of band (the catalog `description`, an avcC/hvcC record). A `Raw`
//!   (Annex-B) unit is converted; its configuration is the ring's recorded
//!   track configuration. An `Flv` unit is legacy AVC or enhanced-RTMP HEVC
//!   (`hvc1`); its sequence header becomes the configuration.
//! - Audio: one raw AAC access unit per frame (ADTS or FLV header removed).
//!   A `Raw` payload holding several ADTS frames becomes several frames,
//!   1024 samples apart.
//! - Each frame is the hang legacy container, written contiguously (varint
//!   µs PTS, then the payload) so moq-net keeps the allocation and every
//!   subscriber shares it.
//!
//! A configuration change bumps [`MediaConverter::catalog_version`]; the
//! publisher republishes the catalog before the next frame.

use bytes::{BufMut, Bytes, BytesMut};

use crate::media::codec::{adts_frame_ranges, annexb_parameter_sets, avcc_to_annexb, strip_adts};
use crate::media::moq::media::{
    AudioConfig, MediaError, TrackKind, VideoCodec, VideoConfig, annexb_to_length_prefixed,
    hang_catalog, legacy_prefix_len, write_legacy_prefix,
};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};
use crate::media::ring_buffer::TrackConfig;

use super::startup::MoqPublishStartup;

/// Samples per AAC frame (AAC-LC).
const AAC_FRAME_SAMPLES: u64 = 1024;
/// FLV codec id of legacy AVC video.
const FLV_CODEC_AVC: u8 = 7;
/// FLV sound format of AAC.
const FLV_SOUND_AAC: u8 = 10;
/// Enhanced-RTMP video packet types.
const EX_SEQUENCE_START: u8 = 0;
const EX_CODED_FRAMES: u8 = 1;
const EX_CODED_FRAMES_X: u8 = 3;

/// One hang frame: the legacy container (varint µs PTS + payload) in one
/// buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HangFrame {
    pub(crate) track: TrackKind,
    pub(crate) keyframe: bool,
    pub(crate) pts_us: u64,
    pub(crate) data: Bytes,
}

impl HangFrame {
    /// The codec payload after the timestamp prefix.
    #[cfg(test)]
    pub(crate) fn payload(&self) -> Bytes {
        self.data
            .slice(legacy_prefix_len(self.pts_us).min(self.data.len())..)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConvertError {
    /// An FLV video tag that is neither legacy AVC nor enhanced `hvc1`.
    UnsupportedFlvVideo(u8),
    /// An FLV audio tag that is not AAC.
    UnsupportedFlvAudio(u8),
    /// An FLV tag too short to carry its header.
    TruncatedFlv,
    Media(MediaError),
}

impl std::fmt::Display for ConvertError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::UnsupportedFlvVideo(tag) => {
                write!(
                    f,
                    "FLV video tag 0x{tag:02x} is neither AVC nor enhanced hvc1"
                )
            }
            Self::UnsupportedFlvAudio(tag) => write!(f, "FLV audio tag 0x{tag:02x} is not AAC"),
            Self::TruncatedFlv => f.write_str("FLV tag shorter than its header"),
            Self::Media(error) => error.fmt(f),
        }
    }
}

impl From<MediaError> for ConvertError {
    fn from(error: MediaError) -> Self {
        Self::Media(error)
    }
}

/// The ring audio track a broadcast carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct AudioTrack {
    track_index: u32,
    sample_rate: u32,
}

/// Per-broadcast conversion state: the configurations the catalog
/// describes and what the next unit needs to be framed.
#[derive(Debug)]
pub(crate) struct MediaConverter {
    video_codec: Option<VideoCodec>,
    audio_track: Option<AudioTrack>,
    video: Option<VideoConfig>,
    audio: Option<AudioConfig>,
    /// The ring configuration generation `video` came from (0: none).
    video_generation: u32,
    /// NAL length width of FLV video units.
    flv_nalu_length_size: usize,
    catalog_version: u64,
}

impl MediaConverter {
    pub(crate) fn new(startup: &MoqPublishStartup) -> Self {
        Self {
            video_codec: startup.video.as_ref().map(|video| video.codec),
            audio_track: startup.audio.as_ref().map(|audio| AudioTrack {
                track_index: audio.track_index,
                sample_rate: audio.config.sample_rate,
            }),
            video: startup
                .video
                .as_ref()
                .and_then(|video| video.config.clone()),
            audio: startup.audio.as_ref().map(|audio| audio.config.clone()),
            video_generation: 0,
            flv_nalu_length_size: startup
                .video
                .as_ref()
                .and_then(|video| video.config.as_ref())
                .and_then(VideoConfig::parameter_sets)
                .map_or(4, |(_, size)| usize::from(size)),
            catalog_version: 0,
        }
    }

    pub(crate) fn has_video(&self) -> bool {
        self.video_codec.is_some()
    }

    pub(crate) fn has_audio(&self) -> bool {
        self.audio_track.is_some()
    }

    /// Bumped by every configuration change the catalog must carry.
    pub(crate) fn catalog_version(&self) -> u64 {
        self.catalog_version
    }

    /// The catalog for the current configurations; `None` while a video
    /// track has no configuration yet (frames wait for it).
    pub(crate) fn catalog(&self) -> Result<Option<hang::Catalog>, MediaError> {
        if self.video_codec.is_some() && self.video.is_none() {
            return Ok(None);
        }
        hang_catalog(self.video.as_ref(), self.audio.as_ref()).map(Some)
    }

    #[cfg(test)]
    pub(crate) fn video_config(&self) -> Option<&VideoConfig> {
        self.video.as_ref()
    }

    #[cfg(test)]
    pub(crate) fn audio_config(&self) -> Option<&AudioConfig> {
        self.audio.as_ref()
    }

    /// Whether a Raw video unit needs its ring configuration looked up.
    pub(crate) fn wants_track_config(&self, packet: &MediaPacket) -> bool {
        self.video_codec.is_some()
            && packet.media_type == MediaType::Video
            && packet.track_index == 0
            && packet.format == PayloadFormat::Raw
    }

    /// Convert one ring packet, pushing zero or more frames into `out`.
    /// `video_config`: the ring configuration recorded for this unit (Raw
    /// video only).
    pub(crate) fn convert(
        &mut self,
        packet: &MediaPacket,
        video_config: Option<&TrackConfig>,
        out: &mut Vec<HangFrame>,
    ) -> Result<(), ConvertError> {
        match packet.media_type {
            MediaType::Video if packet.track_index == 0 => match self.video_codec {
                Some(codec) => match packet.format {
                    PayloadFormat::Raw => self.convert_raw_video(codec, packet, video_config, out),
                    PayloadFormat::Flv => self.convert_flv_video(codec, packet, out),
                },
                None => Ok(()),
            },
            MediaType::Audio => match self.audio_track {
                Some(track) if track.track_index == packet.track_index => {
                    self.convert_audio(track, packet, out)
                }
                _ => Ok(()),
            },
            MediaType::Video => Ok(()),
        }
    }

    fn set_video(&mut self, config: VideoConfig) {
        if self.video.as_ref().map(|video| &video.description) == Some(&config.description) {
            return;
        }
        // Keep what the startup knew when the new record does not say.
        let previous = self.video.take();
        let config = VideoConfig {
            coded_width: config
                .coded_width
                .or(previous.as_ref().and_then(|video| video.coded_width)),
            coded_height: config
                .coded_height
                .or(previous.as_ref().and_then(|video| video.coded_height)),
            framerate: config
                .framerate
                .or(previous.as_ref().and_then(|video| video.framerate)),
            ..config
        };
        self.video = Some(config);
        self.catalog_version += 1;
    }

    fn convert_raw_video(
        &mut self,
        codec: VideoCodec,
        packet: &MediaPacket,
        ring_config: Option<&TrackConfig>,
        out: &mut Vec<HangFrame>,
    ) -> Result<(), ConvertError> {
        match ring_config {
            Some(config) if config.generation != self.video_generation => {
                self.video_generation = config.generation;
                if let Some(video) = VideoConfig::from_parameter_sets(codec, &config.bytes) {
                    self.set_video(video);
                }
            }
            Some(_) => {}
            // No recorded configuration: a keyframe's in-band sets stand in.
            None if packet.is_keyframe => {
                if let Some(video) = annexb_parameter_sets(&packet.payload)
                    .and_then(|sets| VideoConfig::from_parameter_sets(codec, &sets))
                {
                    self.set_video(video);
                }
            }
            None => {}
        }
        let pts_us = pts_us(packet.pts);
        let mut data = frame_buffer(pts_us, packet.payload.len())?;
        let prefix = data.len();
        annexb_to_length_prefixed(codec, &packet.payload, &mut data);
        if data.len() > prefix {
            out.push(HangFrame {
                track: TrackKind::Video,
                keyframe: packet.is_keyframe,
                pts_us,
                data: data.freeze(),
            });
        }
        Ok(())
    }

    fn convert_flv_video(
        &mut self,
        codec: VideoCodec,
        packet: &MediaPacket,
        out: &mut Vec<HangFrame>,
    ) -> Result<(), ConvertError> {
        let payload = &packet.payload;
        let &[tag, packet_type, ..] = payload.as_ref() else {
            return Err(ConvertError::TruncatedFlv);
        };
        let (keyframe, packet_type, body) = if tag & 0x80 != 0 {
            // Enhanced RTMP: [tag][fourcc][CTS for CodedFrames]...
            if codec != VideoCodec::H265 || payload.get(1..5) != Some(b"hvc1".as_slice()) {
                return Err(ConvertError::UnsupportedFlvVideo(tag));
            }
            let packet_type = tag & 0x0F;
            let start = if packet_type == EX_CODED_FRAMES { 8 } else { 5 };
            let body = payload.get(start..).ok_or(ConvertError::TruncatedFlv)?;
            let packet_type = match packet_type {
                EX_SEQUENCE_START => 0,
                EX_CODED_FRAMES | EX_CODED_FRAMES_X => 1,
                _ => 2,
            };
            ((tag >> 4) & 0x07 == 1, packet_type, body)
        } else {
            if codec != VideoCodec::H264 || tag & 0x0F != FLV_CODEC_AVC {
                return Err(ConvertError::UnsupportedFlvVideo(tag));
            }
            let body = payload.get(5..).ok_or(ConvertError::TruncatedFlv)?;
            (tag >> 4 == 1, packet_type, body)
        };
        match packet_type {
            // Sequence header: the decoder configuration record.
            0 => {
                let record = VideoConfig {
                    codec,
                    description: Some(payload.slice_ref(body)),
                    coded_width: None,
                    coded_height: None,
                    framerate: None,
                    jitter: None,
                    container: Default::default(),
                };
                if let Some((sets, size)) = record.parameter_sets() {
                    self.flv_nalu_length_size = usize::from(size);
                    // The parsed config adds the picture size; the record
                    // stays the publisher's own.
                    let parsed = VideoConfig::from_parameter_sets(codec, &sets);
                    let description = record.description.clone();
                    self.set_video(VideoConfig {
                        description,
                        ..parsed.unwrap_or(record)
                    });
                }
                Ok(())
            }
            1 if !body.is_empty() => {
                let pts_us = pts_us(packet.pts);
                let mut data = frame_buffer(pts_us, body.len())?;
                if self.flv_nalu_length_size == 4 {
                    data.put_slice(body);
                } else {
                    let annexb = avcc_to_annexb(body, self.flv_nalu_length_size);
                    annexb_to_length_prefixed(codec, &annexb, &mut data);
                }
                out.push(HangFrame {
                    track: TrackKind::Video,
                    keyframe,
                    pts_us,
                    data: data.freeze(),
                });
                Ok(())
            }
            // End of sequence, or an empty coded-frames tag.
            _ => Ok(()),
        }
    }

    fn convert_audio(
        &mut self,
        track: AudioTrack,
        packet: &MediaPacket,
        out: &mut Vec<HangFrame>,
    ) -> Result<(), ConvertError> {
        let pts_us = pts_us(packet.pts);
        let payload = &packet.payload;
        match packet.format {
            PayloadFormat::Flv => {
                let &[tag, packet_type, ..] = payload.as_ref() else {
                    return Err(ConvertError::TruncatedFlv);
                };
                if tag >> 4 != FLV_SOUND_AAC {
                    return Err(ConvertError::UnsupportedFlvAudio(tag));
                }
                let body = &payload[2..];
                match packet_type {
                    0 if !body.is_empty() => {
                        if let Some(audio) = self.audio.as_mut()
                            && audio.asc.as_deref() != Some(body)
                        {
                            audio.asc = Some(payload.slice(2..));
                            self.catalog_version += 1;
                        }
                    }
                    1 if !body.is_empty() => out.push(audio_frame(pts_us, body)?),
                    _ => {}
                }
                Ok(())
            }
            PayloadFormat::Raw => {
                let mut ranges = adts_frame_ranges(payload).peekable();
                if ranges.peek().is_none() {
                    if !payload.is_empty() {
                        out.push(audio_frame(pts_us, payload)?);
                    }
                    return Ok(());
                }
                for (index, range) in (0u64..).zip(ranges) {
                    let raw = strip_adts(&payload[range]);
                    if raw.is_empty() {
                        continue;
                    }
                    let offset_us =
                        index * AAC_FRAME_SAMPLES * 1_000_000 / u64::from(track.sample_rate.max(1));
                    out.push(audio_frame(pts_us.saturating_add(offset_us), raw)?);
                }
                Ok(())
            }
        }
    }
}

/// A packet PTS (milliseconds; negative clamps to 0) in microseconds.
fn pts_us(pts_ms: i64) -> u64 {
    u64::try_from(pts_ms).unwrap_or(0).saturating_mul(1000)
}

/// A frame buffer holding the timestamp prefix, with room for `payload`.
fn frame_buffer(pts_us: u64, payload: usize) -> Result<BytesMut, MediaError> {
    let mut data = BytesMut::with_capacity(legacy_prefix_len(pts_us) + payload);
    write_legacy_prefix(pts_us, &mut data)?;
    Ok(data)
}

fn audio_frame(pts_us: u64, payload: &[u8]) -> Result<HangFrame, MediaError> {
    let mut data = frame_buffer(pts_us, payload.len())?;
    data.put_slice(payload);
    Ok(HangFrame {
        track: TrackKind::Audio,
        keyframe: true,
        pts_us,
        data: data.freeze(),
    })
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
