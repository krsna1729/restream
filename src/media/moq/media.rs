//! The shared MoQ media model and its hang mapping.
//!
//! Frames carry one access unit: H.264/H.265 as 4-byte length-prefixed NAL
//! units with parameter sets out of band (the catalog `description`, an
//! avcC/hvcC record), AAC as one raw access unit. Restream publishes every
//! track as LOC (draft-ietf-moq-loc: a property block with the µs
//! timestamp, then the payload), which both catalogs (hang `catalog.json`,
//! MSF `catalog`) describe; it reads LOC, hang's legacy container (a QUIC
//! varint µs timestamp, then the payload) and CMAF. Video groups are GOPs,
//! audio groups about a second; the publisher picks the grouping, not this
//! module.

#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::fmt;
use std::ops::ControlFlow;
use std::time::Duration;

use bytes::{Buf, BufMut, Bytes, BytesMut};
use hang::catalog::{
    AAC, AudioCodec as HangAudioCodec, AudioConfig as HangAudioConfig, Container, H264, H265,
    VideoCodec as HangVideoCodec, VideoConfig as HangVideoConfig,
};
use moq_net::VarInt;

use super::WireError;
use crate::media::codec::{
    build_avcc_sequence_header, build_hevc_enhanced_rtmp_sequence_header, for_each_annexb_nalu,
    parse_avcc_config, parse_h264_sps, parse_hevc_profile_tier_level, take_u16_prefixed,
};

/// hang's catalog track.
#[cfg(test)]
pub(crate) const CATALOG_TRACK: &str = "catalog.json";
/// Track names Restream publishes.
pub(crate) const VIDEO_TRACK: &str = "video";
pub(crate) const AUDIO_TRACK: &str = "audio";

/// The FLV tag header `build_avcc_sequence_header` and the enhanced-RTMP
/// header `build_hevc_enhanced_rtmp_sequence_header` put before the record.
const SEQUENCE_HEADER_PREFIX: usize = 5;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum VideoCodec {
    H264,
    H265,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TrackKind {
    Video,
    Audio,
}

/// How a track's frames are packaged.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) enum TrackContainer {
    /// QUIC varint µs timestamp, then the codec payload.
    #[default]
    Legacy,
    /// LOC: a property block carrying the timestamp, then the payload.
    Loc,
    /// Each frame a `moof`+`mdat` fragment; `init` is `ftyp`+`moov`.
    Cmaf { init: Bytes },
}

#[derive(Debug, Clone, PartialEq)]
pub(crate) struct VideoConfig {
    pub codec: VideoCodec,
    /// The avcC / hvcC record; `None` means parameter sets travel in band
    /// (avc3 / hev1) and the payload may be Annex-B.
    pub description: Option<Bytes>,
    pub coded_width: Option<u32>,
    pub coded_height: Option<u32>,
    pub framerate: Option<f64>,
    /// The publisher's declared reordering depth (catalog `jitter`).
    pub jitter: Option<Duration>,
    pub container: TrackContainer,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AudioConfig {
    pub sample_rate: u32,
    pub channels: u32,
    /// The AudioSpecificConfig.
    pub asc: Option<Bytes>,
    pub container: TrackContainer,
}

/// What a received catalog offers Restream: at most one video rendition
/// (the best-ranked supported one), every AAC rendition, and the rest with
/// the reason each was refused.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct CatalogTracks {
    pub video: Option<(String, VideoConfig)>,
    pub audio: Vec<(String, AudioConfig)>,
    pub rejected: Vec<(String, String)>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MediaError {
    /// A timestamp at or past 2^62 µs, which the QUIC varint cannot carry.
    TimestampTooLarge,
    /// A video config without a usable avcC/hvcC record.
    MissingDescription,
    /// hang refused the catalog.
    Catalog(String),
}

impl fmt::Display for MediaError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TimestampTooLarge => f.write_str("MoQ timestamp past 2^62 µs"),
            Self::MissingDescription => f.write_str("MoQ video config has no codec record"),
            Self::Catalog(reason) => write!(f, "MoQ catalog: {reason}"),
        }
    }
}

impl std::error::Error for MediaError {}

impl VideoConfig {
    /// A config from Annex-B parameter sets (SPS+PPS, or VPS+SPS+PPS):
    /// the avcC/hvcC record and the picture size from the SPS.
    pub(crate) fn from_parameter_sets(codec: VideoCodec, annexb: &[u8]) -> Option<Self> {
        let (record, size, framerate) = match codec {
            VideoCodec::H264 => {
                let header = build_avcc_sequence_header(annexb)?;
                let sps = find_nalu(annexb, |nalu| nalu.first().is_some_and(|b| b & 0x1F == 7))?;
                let parsed = parse_h264_sps(sps.get(1..)?);
                let size = parsed.as_ref().map(|sps| (sps.width, sps.height));
                let fps = parsed
                    .map(|sps| sps.fps)
                    .filter(|fps| fps.is_finite() && *fps > 0.0);
                (header, size, fps)
            }
            VideoCodec::H265 => {
                let header = build_hevc_enhanced_rtmp_sequence_header(annexb)?;
                let sps = find_nalu(annexb, |nalu| h265_type(nalu) == Some(33))?;
                let size = parse_hevc_profile_tier_level(sps).map(|ptl| (ptl.width, ptl.height));
                (header, size, None)
            }
        };
        Some(Self {
            codec,
            description: Some(record.slice(SEQUENCE_HEADER_PREFIX..)),
            coded_width: size.map(|(width, _)| width).filter(|&w| w > 0),
            coded_height: size.map(|(_, height)| height).filter(|&h| h > 0),
            framerate,
            jitter: None,
            container: TrackContainer::Legacy,
        })
    }

    /// The record's parameter sets as Annex-B and its NAL length size;
    /// `None` without a record or with a malformed one.
    pub(crate) fn parameter_sets(&self) -> Option<(Bytes, u8)> {
        let record = self.description.as_deref()?;
        match self.codec {
            VideoCodec::H264 => {
                let (size, sets) = parse_avcc_config(record);
                (!sets.is_empty()).then(|| (Bytes::from(sets), size as u8))
            }
            VideoCodec::H265 => hvcc_config(record),
        }
    }
}

fn find_nalu(annexb: &[u8], wanted: impl Fn(&[u8]) -> bool) -> Option<&[u8]> {
    let mut found = None;
    let _ = for_each_annexb_nalu(annexb, |nalu| {
        if wanted(nalu) {
            found = Some(nalu);
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    found
}

fn h265_type(nalu: &[u8]) -> Option<u8> {
    match nalu {
        [first, _, ..] => Some((first >> 1) & 0x3F),
        _ => None,
    }
}

/// An hvcC record's (ISO/IEC 14496-15 8.3.3.1) NAL length size and its VPS,
/// SPS and PPS as Annex-B. Truncation, or a record without an SPS, is `None`.
fn hvcc_config(record: &[u8]) -> Option<(Bytes, u8)> {
    let (header, mut rest) = record.split_first_chunk::<22>()?;
    let size = (header[21] & 0x03).saturating_add(1);
    let (&arrays, tail) = rest.split_first()?;
    rest = tail;
    let mut sets = Vec::new();
    let mut has_sps = false;
    for _ in 0..arrays {
        let (&nal_type, tail) = rest.split_first()?;
        let (count, tail) = tail.split_first_chunk::<2>()?;
        rest = tail;
        let nal_type = nal_type & 0x3F;
        for _ in 0..u16::from_be_bytes(*count) {
            let nalu = take_u16_prefixed(&mut rest)?;
            if (32..=34).contains(&nal_type) && !nalu.is_empty() {
                has_sps |= nal_type == 33;
                sets.extend_from_slice(&[0, 0, 0, 1]);
                sets.extend_from_slice(nalu);
            }
        }
    }
    has_sps.then(|| (Bytes::from(sets), size))
}

/// Append Annex-B access unit `au` with 4-byte NAL lengths, without access
/// unit delimiters and parameter sets (those travel in the catalog).
fn annexb_to_length_prefixed(codec: VideoCodec, au: &[u8], out: &mut BytesMut) {
    out.reserve(au.len());
    let _ = for_each_annexb_nalu(au, |nalu| {
        let out_of_band = match codec {
            // SPS, PPS, AUD.
            VideoCodec::H264 => nalu.first().is_some_and(|b| matches!(b & 0x1F, 7..=9)),
            // VPS, SPS, PPS, AUD.
            VideoCodec::H265 => h265_type(nalu).is_some_and(|t| matches!(t, 32..=35)),
        };
        if !out_of_band && let Ok(len) = u32::try_from(nalu.len()) {
            out.put_u32(len);
            out.put_slice(nalu);
        }
        ControlFlow::Continue(())
    });
}

/// The hang codec of a config: avc1/hvc1 from its record when it has one
/// (parameter sets out of band), else unknown.
fn hang_video_codec(config: &VideoConfig) -> Result<HangVideoCodec, MediaError> {
    let record = config
        .description
        .as_deref()
        .ok_or(MediaError::MissingDescription)?;
    match config.codec {
        VideoCodec::H264 => {
            let &[_version, profile, constraints, level] = record
                .first_chunk::<4>()
                .ok_or(MediaError::MissingDescription)?;
            Ok(HangVideoCodec::H264(H264 {
                inline: false,
                profile,
                constraints,
                level,
            }))
        }
        VideoCodec::H265 => {
            let header = record
                .first_chunk::<13>()
                .ok_or(MediaError::MissingDescription)?;
            let mut compatibility = [0; 4];
            compatibility.copy_from_slice(&header[2..6]);
            let mut constraints = [0; 6];
            constraints.copy_from_slice(&header[6..12]);
            Ok(HangVideoCodec::H265(H265 {
                in_band: false,
                profile_space: header[1] >> 6,
                profile_idc: header[1] & 0x1F,
                profile_compatibility_flags: compatibility,
                tier_flag: header[1] & 0x20 != 0,
                level_idc: header[12],
                constraint_flags: constraints,
            }))
        }
    }
}

/// The catalog Restream publishes: [`VIDEO_TRACK`] and [`AUDIO_TRACK`],
/// both LOC (whatever container the configurations were read from).
pub(crate) fn hang_catalog(
    video: Option<&VideoConfig>,
    audio: Option<&AudioConfig>,
) -> Result<hang::Catalog, MediaError> {
    let catalog_error = |error: hang::Error| MediaError::Catalog(error.to_string());
    let mut catalog = hang::Catalog::default();
    if let Some(video) = video {
        let mut config = HangVideoConfig::new(hang_video_codec(video)?);
        config.description = video.description.clone();
        config.coded_width = video.coded_width;
        config.coded_height = video.coded_height;
        config.framerate = video.framerate;
        config.jitter = video.jitter;
        config.container = Container::Loc;
        catalog
            .video
            .insert(VIDEO_TRACK, config)
            .map_err(catalog_error)?;
    }
    if let Some(audio) = audio {
        // The audio object type is the ASC's first five bits; AAC-LC (2)
        // without one.
        let profile = audio
            .asc
            .as_deref()
            .and_then(|asc| asc.first())
            .map_or(2, |b| b >> 3);
        let mut config = HangAudioConfig::new(AAC { profile }, audio.sample_rate, audio.channels);
        config.description = audio.asc.clone();
        config.container = Container::Loc;
        catalog
            .audio
            .insert(AUDIO_TRACK, config)
            .map_err(catalog_error)?;
    }
    Ok(catalog)
}

fn track_container(container: &Container) -> Result<TrackContainer, String> {
    match container {
        Container::Legacy => Ok(TrackContainer::Legacy),
        Container::Cmaf { init } => Ok(TrackContainer::Cmaf { init: init.clone() }),
        Container::Loc => Ok(TrackContainer::Loc),
        Container::Unknown(unknown) => Err(format!(
            "unknown container {:?}",
            unknown.kind().unwrap_or_default()
        )),
    }
}

/// Map a received hang catalog onto the tracks Restream can ingest.
pub(crate) fn from_hang_catalog(catalog: &hang::Catalog) -> CatalogTracks {
    let mut tracks = CatalogTracks::default();
    for (name, config) in catalog.video.ranked() {
        let codec = match &config.codec {
            HangVideoCodec::H264(_) => VideoCodec::H264,
            HangVideoCodec::H265(_) => VideoCodec::H265,
            other => {
                tracks
                    .rejected
                    .push((name.clone(), format!("unsupported video codec {other}")));
                continue;
            }
        };
        let container = match track_container(&config.container) {
            Ok(container) => container,
            Err(reason) => {
                tracks.rejected.push((name.clone(), reason));
                continue;
            }
        };
        if tracks.video.is_some() {
            tracks.rejected.push((
                name.clone(),
                "a better video rendition was chosen".to_string(),
            ));
            continue;
        }
        tracks.video = Some((
            name.clone(),
            VideoConfig {
                codec,
                description: config.description.clone(),
                coded_width: config.coded_width,
                coded_height: config.coded_height,
                framerate: config.framerate,
                jitter: config.jitter,
                container,
            },
        ));
    }
    for (name, config) in &catalog.audio.renditions {
        if !matches!(config.codec, HangAudioCodec::AAC(_)) {
            let codec = &config.codec;
            tracks
                .rejected
                .push((name.clone(), format!("unsupported audio codec {codec}")));
            continue;
        }
        match track_container(&config.container) {
            Ok(container) => tracks.audio.push((
                name.clone(),
                AudioConfig {
                    sample_rate: config.sample_rate,
                    channels: config.channel_count,
                    asc: config.description.clone(),
                    container,
                },
            )),
            Err(reason) => tracks.rejected.push((name.clone(), reason)),
        }
    }
    tracks
}

/// LOC-04 §2.3.1.1–2: an explicit Timescale denotes application media
/// time; omitting it would advertise Unix wall time, not ring timestamps.
const LOC_TIMESCALE: u64 = 0x08;
const LOC_TIMESTAMP: u64 = 0x10;
const LOC_MEDIA_SCALE: u32 = 1_000_000;
/// `LOC_TIMESTAMP - LOC_TIMESCALE` as the one-byte property-id delta.
const LOC_TIMESTAMP_DELTA: u8 = LOC_TIMESTAMP.wrapping_sub(LOC_TIMESCALE) as u8;

/// One-byte property-block length, Timescale id/value, Timestamp id delta,
/// and the largest timestamp. The block stays below 64 bytes, so its length
/// and both property ids each fit in one byte (15 bytes total).
pub(crate) const LOC_HEADROOM: usize =
    1 + 1 + varint_len(LOC_MEDIA_SCALE as u64) + 1 + varint_len(VarInt::MAX.into_inner());

/// The encoded size of a QUIC varint.
const fn varint_len(value: u64) -> usize {
    match value {
        0..0x40 => 1,
        0x40..0x4000 => 2,
        0x4000..0x4000_0000 => 4,
        _ => 8,
    }
}

/// A frame being built: [`LOC_HEADROOM`] spare bytes, then the payload,
/// so the header can be written in front of it once the timestamp is
/// final ([`Self::into_frame`]) and the frame stays one buffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocBuffer(BytesMut);

impl LocBuffer {
    pub(crate) fn with_capacity(payload: usize) -> Self {
        // The headroom is a small constant; a saturating sum only bounds the
        // reservation and never changes the bytes written.
        let mut buffer = BytesMut::with_capacity(LOC_HEADROOM.saturating_add(payload));
        buffer.put_bytes(0, LOC_HEADROOM);
        Self(buffer)
    }

    /// A buffer holding `payload` (one copy).
    pub(crate) fn from_payload(payload: &[u8]) -> Self {
        let mut buffer = Self::with_capacity(payload.len());
        buffer.0.put_slice(payload);
        buffer
    }

    /// Append Annex-B access unit `au` to the payload with 4-byte NAL
    /// lengths ([`annexb_to_length_prefixed`]).
    pub(crate) fn put_length_prefixed(&mut self, codec: VideoCodec, au: &[u8]) {
        annexb_to_length_prefixed(codec, au, &mut self.0);
    }

    pub(crate) fn payload(&self) -> &[u8] {
        self.0.get(LOC_HEADROOM..).unwrap_or_default()
    }

    /// A LOC media-time frame, with an explicit µs Timescale, framed in the
    /// reserved headroom without moving or copying the payload.
    pub(crate) fn into_frame(mut self, timestamp_us: u64) -> Result<Bytes, MediaError> {
        let value = VarInt::from_u64(timestamp_us).ok_or(MediaError::TimestampTooLarge)?;
        let block = 1usize
            .checked_add(varint_len(u64::from(LOC_MEDIA_SCALE)))
            .and_then(|size| size.checked_add(1))
            .and_then(|size| size.checked_add(varint_len(timestamp_us)))
            .ok_or(MediaError::TimestampTooLarge)?;
        let start = LOC_HEADROOM
            .checked_sub(1)
            .and_then(|headroom| headroom.checked_sub(block))
            .ok_or(MediaError::TimestampTooLarge)?;
        {
            let Some(slice) = self.0.get_mut(start..LOC_HEADROOM) else {
                return Err(MediaError::TimestampTooLarge);
            };
            let mut header = slice;
            // The block and property-id deltas are all one-byte varints.
            header.put_u8(u8::try_from(block).map_err(|_| MediaError::TimestampTooLarge)?);
            header.put_u8(LOC_TIMESCALE as u8);
            VarInt::from_u32(LOC_MEDIA_SCALE)
                .encode_quic(&mut header)
                .map_err(|_| MediaError::TimestampTooLarge)?;
            // Property-id delta: Timestamp id (0x10) minus Timescale id (0x08).
            header.put_u8(LOC_TIMESTAMP_DELTA);
            value
                .encode_quic(&mut header)
                .map_err(|_| MediaError::TimestampTooLarge)?;
        }
        self.0.advance(start);
        Ok(self.0.freeze())
    }
}

/// Split a legacy frame into its µs timestamp and payload (no copy).
pub(crate) fn parse_legacy_frame(mut frame: Bytes) -> Result<(u64, Bytes), WireError> {
    let timestamp = VarInt::decode_quic(&mut frame)
        .map_err(|_| WireError::Invalid("truncated legacy frame timestamp"))?;
    Ok((timestamp.into_inner(), frame))
}

/// Split a LOC frame into its µs timestamp (a per-frame timescale
/// honoured) and payload (no copy).
pub(crate) fn parse_loc_frame(frame: Bytes) -> Result<(u64, Bytes), WireError> {
    let frame = moq_loc::decode(frame).map_err(|error| match error {
        moq_loc::Error::MissingTimestamp => WireError::Invalid("LOC frame without a timestamp"),
        _ => WireError::Invalid("malformed LOC properties"),
    })?;
    let us = match frame.timescale {
        None | Some(1_000_000) => frame.timestamp,
        Some(scale) => {
            let scaled = u128::from(frame.timestamp)
                .checked_mul(1_000_000)
                .and_then(|micros| micros.checked_div(u128::from(scale)))
                .ok_or(WireError::Invalid("LOC timestamp out of range"))?;
            u64::try_from(scaled).map_err(|_| WireError::Invalid("LOC timestamp out of range"))?
        }
    };
    Ok((us, frame.payload))
}

#[cfg(test)]
#[path = "media_tests.rs"]
mod tests;
