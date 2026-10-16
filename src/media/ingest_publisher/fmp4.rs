//! Fragmented MP4 (CMAF) HLS objects to ring packets: H.264 video as Annex B
//! and AAC audio as ADTS, the payloads an MPEG-TS input produces, so every
//! downstream stage sees an fMP4 input exactly as it sees a TS one.
//!
//! The init segment (`EXT-X-MAP`) builds the track table; each media segment
//! is walked by [`boxes`] and demuxed one `moof` + `mdat` fragment at a time
//! by `shiguredo_mp4`. Timestamps come from each fragment's `tfdt`, so a
//! skipped segment or a new epoch cannot drift. Untrusted bytes: checked
//! arithmetic, no indexing, no panics.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

pub mod boxes;

use std::collections::VecDeque;
use std::ops::Range;

use bytes::Bytes;
use shiguredo_mp4::Decode;
use shiguredo_mp4::boxes::{HdlrBox, MoovBox, SampleEntry};
use shiguredo_mp4::demux::Fmp4SegmentDemuxer;

use super::{MediaRejection, SegmentDemux};
use crate::media::codec::{
    avcc_to_annexb_into, build_adts_header_for, build_avcc_sequence_header, level_name,
    parse_h264_sps, profile_name,
};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::mpegts::DemuxProbe;
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};

/// The reason an HEVC init segment is refused.
pub const HEVC_NOT_IMPLEMENTED: &str = "HEVC fMP4 ingest is not implemented";

const START_CODE: [u8; 4] = [0, 0, 0, 1];
const AAC_SAMPLE_RATES: [u32; 13] = [
    96_000, 88_200, 64_000, 48_000, 44_100, 32_000, 24_000, 22_050, 16_000, 12_000, 11_025, 8_000,
    7_350,
];

fn malformed(message: impl Into<String>) -> MediaRejection {
    MediaRejection::Malformed(message.into())
}

fn unsupported(message: impl Into<String>) -> MediaRejection {
    MediaRejection::Unsupported(message.into())
}

/// ADTS header fields taken from an `esds` AudioSpecificConfig.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdtsConfig {
    object_type: u8,
    freq_index: u8,
    channel_config: u8,
}

impl AdtsConfig {
    /// Read the AudioSpecificConfig fields ADTS carries; refuse what ADTS
    /// cannot express rather than publish audio a decoder misreads.
    pub fn from_audio_specific_config(asc: &[u8]) -> Result<Self, MediaRejection> {
        let &[b0, b1] = asc
            .first_chunk::<2>()
            .ok_or_else(|| malformed("AudioSpecificConfig is shorter than 2 bytes"))?;
        let object_type = b0 >> 3;
        let freq_index = ((b0 & 0x07) << 1) | (b1 >> 7);
        let channel_config = (b1 >> 3) & 0x0F;
        if !(1..=4).contains(&object_type) {
            return Err(unsupported(format!(
                "AAC audio object type {object_type} cannot be carried as ADTS"
            )));
        }
        if freq_index == 0x0F {
            return Err(unsupported(
                "AAC with an explicit sampling frequency cannot be carried as ADTS",
            ));
        }
        if usize::from(freq_index) >= AAC_SAMPLE_RATES.len() {
            return Err(malformed(format!(
                "AAC sampling frequency index {freq_index} is reserved"
            )));
        }
        if !(1..=7).contains(&channel_config) {
            return Err(unsupported(format!(
                "AAC channel configuration {channel_config} is not supported"
            )));
        }
        Ok(Self {
            object_type,
            freq_index,
            channel_config,
        })
    }

    fn sample_rate(self) -> u32 {
        AAC_SAMPLE_RATES
            .get(usize::from(self.freq_index))
            .copied()
            .unwrap_or_default()
    }

    fn channels(self) -> u32 {
        match self.channel_config {
            7 => 8,
            channels => u32::from(channels),
        }
    }

    fn profile(self) -> &'static str {
        match self.object_type {
            1 => "Main",
            2 => "LC",
            3 => "SSR",
            _ => "LTP/Reserved",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Codec {
    H264 {
        nalu_length_size: usize,
        /// SPS and PPS from `avcC`, as Annex B.
        parameter_sets: Bytes,
    },
    Aac(AdtsConfig),
}

/// One published track of the init segment.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Track {
    track_id: u32,
    media_type: MediaType,
    track_index: u32,
    codec: Codec,
}

struct Init {
    demuxer: Fmp4SegmentDemuxer,
    tracks: Vec<Track>,
}

/// The media segment being published: its bytes (shared, not copied) and
/// how far the session's chunks have reached into it.
struct Object {
    bytes: Bytes,
    next_fragment: usize,
    arrived: usize,
}

/// A demuxed packet and the object byte position whose arrival releases it.
struct Pending {
    release_at: usize,
    packet: MediaPacket,
}

/// fMP4 demux for one HLS push session.
#[derive(Default)]
pub struct Fmp4Demux {
    init: Option<Init>,
    object: Option<Object>,
    fragments: Vec<Range<usize>>,
    /// One fragment's packets, sorted into decode order before release.
    fragment_packets: Vec<(usize, MediaPacket)>,
    pending: VecDeque<Pending>,
    output: Vec<MediaPacket>,
    probe: Option<DemuxProbe>,
    unknown_logged: bool,
}

impl Fmp4Demux {
    /// Validate an init segment without applying it: the bound, the box
    /// layout, and the codec scope (H.264 and AAC).
    pub fn check_init(bytes: &[u8], max_bytes: usize) -> Result<(), MediaRejection> {
        parse_init(bytes, max_bytes).map(|_| ())
    }

    /// Demux every fragment the arrived bytes reach and release the packets
    /// whose share of the fragment has arrived.
    fn advance(&mut self) -> Result<(), MediaRejection> {
        let Self {
            init,
            object,
            fragments,
            fragment_packets,
            pending,
            output,
            ..
        } = self;
        let Some(object) = object else {
            return Ok(());
        };
        let init = init
            .as_mut()
            .ok_or_else(|| malformed("fMP4 media segment before its init segment"))?;
        while let Some(range) = fragments.get(object.next_fragment)
            && range.start < object.arrived
        {
            object.next_fragment = object.next_fragment.saturating_add(1);
            demux_fragment(init, &object.bytes, range.clone(), fragment_packets)?;
            // Decode order across tracks, released in proportion to the
            // fragment's bytes, like the interleaved TS stream it replaces.
            fragment_packets.sort_by_key(|(_, packet)| packet.dts);
            let mut release_at = range.start;
            for (size, packet) in fragment_packets.drain(..) {
                release_at = release_at.saturating_add(size).min(range.end);
                pending.push_back(Pending { release_at, packet });
            }
        }
        while pending
            .front()
            .is_some_and(|next| next.release_at <= object.arrived)
        {
            if let Some(next) = pending.pop_front() {
                output.push(next.packet);
            }
        }
        Ok(())
    }
}

impl SegmentDemux for Fmp4Demux {
    /// An fMP4 media segment is demuxed from the whole object; the chunks
    /// that follow only pace how much of it is released.
    fn begin_object(&mut self, object: &Bytes) -> Result<(), MediaRejection> {
        if self.init.is_none() {
            return Err(malformed(
                "fMP4 media segment before its EXT-X-MAP init segment",
            ));
        }
        let report = boxes::media_fragments(object, &mut self.fragments)
            .map_err(|error| malformed(error.to_string()))?;
        if let Some(kind) = report.first_unknown
            && !std::mem::replace(&mut self.unknown_logged, true)
        {
            tracing::info!(
                box_type = %boxes::name(&kind),
                "[hls] fMP4 segment carries an unknown top-level box; skipped (logged once per session)"
            );
        }
        self.object = Some(Object {
            bytes: object.clone(),
            next_fragment: 0,
            arrived: 0,
        });
        Ok(())
    }

    fn feed(&mut self, chunk: &[u8]) -> Result<(), MediaRejection> {
        let object = self
            .object
            .as_mut()
            .ok_or_else(|| malformed("fMP4 bytes arrived outside an object"))?;
        object.arrived = object
            .arrived
            .saturating_add(chunk.len())
            .min(object.bytes.len());
        self.advance()
    }

    fn drain_into(&mut self, out: &mut Vec<MediaPacket>) -> usize {
        let moved = self.output.len();
        out.append(&mut self.output);
        moved
    }

    fn end_object(&mut self) -> Result<(), MediaRejection> {
        if let Some(object) = self.object.as_mut() {
            object.arrived = object.bytes.len();
        }
        let advanced = self.advance();
        self.flush();
        advanced
    }

    fn flush(&mut self) {
        self.output
            .extend(self.pending.drain(..).map(|pending| pending.packet));
        self.object = None;
    }

    /// Fragments are anchored by their own `tfdt`, so the init stays; a new
    /// init arrives through [`SegmentDemux::apply_init`].
    fn discontinuity(&mut self) {
        self.pending.clear();
        self.object = None;
    }

    fn apply_init(&mut self, bytes: &[u8], max_bytes: usize) -> Result<(), MediaRejection> {
        let (init, probe) = parse_init(bytes, max_bytes)?;
        self.init = Some(init);
        self.probe = Some(probe);
        Ok(())
    }

    fn take_probe(&mut self) -> Option<DemuxProbe> {
        self.probe.take()
    }
}

/// Parse an init segment into the demuxer, the published tracks and the
/// probe the ingest's metadata comes from.
fn parse_init(bytes: &[u8], max_bytes: usize) -> Result<(Init, DemuxProbe), MediaRejection> {
    if bytes.len() > max_bytes {
        return Err(MediaRejection::TooLarge(format!(
            "fMP4 init segment of {} bytes is over its {max_bytes}-byte bound",
            bytes.len()
        )));
    }
    let moov = boxes::moov_range(bytes)
        .ok()
        .and_then(|range| bytes.get(range))
        .ok_or_else(|| malformed("fMP4 init segment has no moov"))?;
    let (moov, _) =
        MoovBox::decode(moov).map_err(|error| malformed(format!("fMP4 init moov: {error}")))?;

    let mut tracks = Vec::new();
    let mut video = None;
    let mut video_sequence_header = None;
    let mut video_track_count = 0usize;
    let mut audio_tracks = Vec::new();
    for trak in &moov.trak_boxes {
        let track_id = trak.tkhd_box.track_id;
        let media_type = match trak.mdia_box.hdlr_box.handler_type {
            HdlrBox::HANDLER_TYPE_VIDE => MediaType::Video,
            HdlrBox::HANDLER_TYPE_SOUN => MediaType::Audio,
            _ => continue,
        };
        let entries = &trak.mdia_box.minf_box.stbl_box.stsd_box.entries;
        let [entry] = entries.as_slice() else {
            return Err(unsupported(format!(
                "fMP4 track {track_id} has {} sample descriptions; one is supported",
                entries.len()
            )));
        };
        match (media_type, entry) {
            (MediaType::Video, SampleEntry::Avc1(avc1)) => {
                video_track_count = video_track_count.saturating_add(1);
                if video.is_some() {
                    // Like TS ingest, the first video track is published.
                    continue;
                }
                let avcc = &avc1.avcc_box;
                let mut parameter_sets = Vec::new();
                for nalu in avcc.sps_list.iter().chain(&avcc.pps_list) {
                    parameter_sets.extend_from_slice(&START_CODE);
                    parameter_sets.extend_from_slice(nalu);
                }
                let sps = avcc
                    .sps_list
                    .first()
                    .ok_or_else(|| malformed(format!("fMP4 track {track_id}: avcC has no SPS")))?;
                video = Some(video_meta(sps, avc1.visual.width, avc1.visual.height));
                video_sequence_header = build_avcc_sequence_header(&parameter_sets);
                let parameter_sets = Bytes::from(parameter_sets);
                tracks.push(Track {
                    track_id,
                    media_type,
                    track_index: 0,
                    codec: Codec::H264 {
                        nalu_length_size: usize::from(avcc.length_size_minus_one.get())
                            .saturating_add(1),
                        parameter_sets,
                    },
                });
            }
            (MediaType::Video, SampleEntry::Hev1(_) | SampleEntry::Hvc1(_)) => {
                return Err(unsupported(HEVC_NOT_IMPLEMENTED));
            }
            (MediaType::Audio, SampleEntry::Mp4a(mp4a)) => {
                let asc = mp4a
                    .esds_box
                    .es
                    .dec_config_descr
                    .dec_specific_info
                    .as_ref()
                    .map(|info| info.payload.as_slice())
                    .ok_or_else(|| {
                        malformed(format!(
                            "fMP4 track {track_id}: esds has no AudioSpecificConfig"
                        ))
                    })?;
                let adts = AdtsConfig::from_audio_specific_config(asc)?;
                let track_index = u32::try_from(audio_tracks.len()).unwrap_or(u32::MAX);
                audio_tracks.push(AudioMeta {
                    codec: "aac".to_string(),
                    sample_rate: adts.sample_rate(),
                    channels: adts.channels(),
                    track_index,
                    profile: Some(adts.profile().to_string()),
                    ..AudioMeta::default()
                });
                tracks.push(Track {
                    track_id,
                    media_type,
                    track_index,
                    codec: Codec::Aac(adts),
                });
            }
            (MediaType::Video, other) => {
                return Err(unsupported(format!(
                    "fMP4 video codec {} is not supported; H.264 only",
                    shiguredo_mp4::BaseBox::box_type(other)
                )));
            }
            (MediaType::Audio, other) => {
                return Err(unsupported(format!(
                    "fMP4 audio codec {} is not supported; AAC only",
                    shiguredo_mp4::BaseBox::box_type(other)
                )));
            }
        }
    }
    if tracks.is_empty() {
        return Err(unsupported("fMP4 init segment has no H.264 or AAC track"));
    }

    let mut demuxer = Fmp4SegmentDemuxer::new();
    demuxer
        .handle_init_segment(bytes)
        .map_err(|error| malformed(format!("fMP4 init segment: {error}")))?;
    let probe = DemuxProbe {
        video,
        video_sequence_header,
        video_track_count,
        audio_tracks,
    };
    Ok((Init { demuxer, tracks }, probe))
}

fn video_meta(sps: &[u8], width: u16, height: u16) -> VideoMeta {
    let mut meta = VideoMeta {
        codec: "h264".to_string(),
        width: u32::from(width),
        height: u32::from(height),
        fps: 0.0,
        bw: None,
        pid: None,
        language: None,
        title: None,
        profile: None,
        level: None,
        pixel_format: None,
    };
    if let Some(parsed) = sps.get(1..).and_then(parse_h264_sps) {
        meta.profile = Some(profile_name(parsed.profile_idc).to_string());
        meta.level = Some(level_name(parsed.level_idc));
        meta.width = parsed.width;
        meta.height = parsed.height;
        if parsed.fps > 0.0 {
            meta.fps = parsed.fps;
        }
    }
    meta
}

/// Demux one `moof` + `mdat` fragment of `object` into `out` as
/// `(sample size, packet)` pairs.
fn demux_fragment(
    init: &mut Init,
    object: &[u8],
    range: Range<usize>,
    out: &mut Vec<(usize, MediaPacket)>,
) -> Result<(), MediaRejection> {
    let fragment = object
        .get(range.clone())
        .ok_or_else(|| malformed("fMP4 fragment runs past the object"))?;
    let samples = init
        .demuxer
        .handle_media_segment(fragment)
        .map_err(|error| malformed(format!("fMP4 fragment at byte {}: {error}", range.start)))?;
    for sample in &samples {
        let Some(track) = init
            .tracks
            .iter()
            .find(|track| track.track_id == sample.track.track_id)
        else {
            continue;
        };
        let data = usize::try_from(sample.data_offset)
            .ok()
            .and_then(|start| Some(start..start.checked_add(sample.data_size)?))
            .and_then(|span| fragment.get(span))
            .ok_or_else(|| malformed("fMP4 sample runs past its fragment"))?;
        let packet = convert_sample(
            track,
            sample.track.timescale.get(),
            sample.timestamp,
            sample.composition_time_offset.unwrap_or(0),
            sample.keyframe,
            data,
        )?;
        out.push((data.len(), packet));
    }
    Ok(())
}

/// Ticks of `timescale` to milliseconds, rounded to nearest: at most 0.5 ms
/// off, and never accumulated, since each sample is converted on its own.
fn ticks_to_ms(ticks: i128, timescale: u32) -> Option<i64> {
    let timescale = i128::from(timescale);
    if timescale == 0 {
        return None;
    }
    let scaled = ticks.checked_mul(1000)?.checked_add(timescale / 2)?;
    i64::try_from(scaled.div_euclid(timescale)).ok()
}

/// One sample as the packet a TS input would carry: DTS and PTS in
/// milliseconds, H.264 as Annex B (with the `avcC` SPS/PPS ahead of a
/// keyframe that lacks them), AAC behind an ADTS header.
fn convert_sample(
    track: &Track,
    timescale: u32,
    timestamp: u64,
    composition_offset: i64,
    keyframe: bool,
    data: &[u8],
) -> Result<MediaPacket, MediaRejection> {
    let decode = i128::from(timestamp);
    let dts = ticks_to_ms(decode, timescale);
    let pts = decode
        .checked_add(i128::from(composition_offset))
        .and_then(|ticks| ticks_to_ms(ticks, timescale));
    let (Some(dts), Some(pts)) = (dts, pts) else {
        return Err(malformed("fMP4 sample timestamp is out of range"));
    };
    let payload = match &track.codec {
        Codec::H264 {
            nalu_length_size,
            parameter_sets,
        } => {
            let inject = keyframe && !avcc_carries_sps(data, *nalu_length_size);
            let mut payload = Vec::with_capacity(data.len().saturating_add(if inject {
                parameter_sets.len()
            } else {
                0
            }));
            if inject {
                payload.extend_from_slice(parameter_sets);
            }
            avcc_to_annexb_into(data, *nalu_length_size, &mut payload);
            payload
        }
        Codec::Aac(adts) => {
            let header = build_adts_header_for(
                data.len(),
                adts.object_type,
                adts.freq_index,
                adts.channel_config,
            )
            .ok_or_else(|| {
                malformed(format!(
                    "AAC frame of {} bytes does not fit ADTS",
                    data.len()
                ))
            })?;
            let mut payload = Vec::with_capacity(data.len().saturating_add(header.len()));
            payload.extend_from_slice(&header);
            payload.extend_from_slice(data);
            payload
        }
    };
    Ok(MediaPacket {
        media_type: track.media_type,
        format: PayloadFormat::Raw,
        is_keyframe: track.media_type == MediaType::Video && keyframe,
        track_index: track.track_index,
        pts,
        dts,
        payload: Bytes::from(payload),
    })
}

/// Whether a length-prefixed H.264 sample carries an SPS (NAL type 7).
fn avcc_carries_sps(mut data: &[u8], nalu_length_size: usize) -> bool {
    while let Some((length, rest)) = data.split_at_checked(nalu_length_size) {
        let length = length
            .iter()
            .fold(0usize, |len, &byte| (len << 8) | usize::from(byte));
        let Some((nalu, rest)) = rest.split_at_checked(length) else {
            return false;
        };
        if nalu.first().is_some_and(|header| header & 0x1F == 7) {
            return true;
        }
        if length == 0 {
            return false;
        }
        data = rest;
    }
    false
}

#[cfg(test)]
#[path = "fmp4_tests.rs"]
mod tests;
