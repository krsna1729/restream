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

use std::ops::Range;

use bytes::Bytes;
use shiguredo_mp4::Decode;
use shiguredo_mp4::boxes::{HdlrBox, MoovBox, SampleEntry};
use shiguredo_mp4::demux::Fmp4SegmentDemuxer;

use super::{MediaRejection, SegmentDemux};
use restream_core::codec::AAC_SAMPLE_RATES;
use restream_core::codec::build_adts_header_for;
use restream_core::codec::build_avcc_sequence_header;
use restream_core::codec::for_each_length_prefixed;
use restream_core::codec::level_name;
use restream_core::codec::parse_h264_sps;
use restream_core::codec::profile_name;
use restream_core::metadata::{AudioMeta, VideoMeta};
use restream_core::{MediaPacket, MediaType, PayloadFormat};
use restream_mpegts::DemuxProbe;

const START_CODE: [u8; 4] = [0, 0, 0, 1];

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

    /// The 7-byte ADTS header for a raw frame of `frame_len` bytes; `None`
    /// above [`restream_core::codec::MAX_ADTS_PAYLOAD`].
    pub(crate) fn adts_header(self, frame_len: usize) -> Option<[u8; 7]> {
        build_adts_header_for(
            frame_len,
            self.object_type,
            self.freq_index,
            self.channel_config,
        )
    }

    pub(crate) fn sample_rate(self) -> u32 {
        AAC_SAMPLE_RATES
            .get(usize::from(self.freq_index))
            .copied()
            .unwrap_or_default()
    }

    pub(crate) fn channels(self) -> u32 {
        match self.channel_config {
            7 => 8,
            channels => u32::from(channels),
        }
    }

    pub(crate) fn profile(self) -> &'static str {
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
    /// H.264 or HEVC: length-prefixed NAL units, published as Annex B with
    /// the configuration record's parameter sets ahead of a keyframe that
    /// lacks them.
    Video {
        nal: VideoNal,
        nalu_length_size: usize,
        /// SPS and PPS from `avcC`, or VPS, SPS and PPS from `hvcC`, as
        /// Annex B.
        parameter_sets: Bytes,
    },
    Aac(AdtsConfig),
}

/// Which NAL syntax a video track's samples use.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum VideoNal {
    H264,
    H265,
}

impl VideoNal {
    /// Whether a NAL unit, by its first header byte, is a sequence
    /// parameter set: type 7 in H.264, type 33 in HEVC.
    fn is_sps(self, header: u8) -> bool {
        match self {
            Self::H264 => header & 0x1F == 7,
            Self::H265 => (header >> 1) & 0x3F == 33,
        }
    }
}

/// HEVC parameter-set NAL unit types: VPS, SPS and PPS.
const HEVC_PARAMETER_SETS: std::ops::RangeInclusive<u8> = 32..=34;

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
    /// The fragment being released. FFmpeg's fMP4 HLS writes one fragment
    /// per segment, so its samples are converted as the chunks that pace the
    /// segment release them, not all in the poll that first reaches it.
    releasing: Option<Releasing>,
}

/// A fragment's sample plans, in release order, and how many are released.
struct Releasing {
    range: Range<usize>,
    plans: Vec<SamplePlan>,
    next: usize,
}

/// One sample of a fragment, owned (the demuxer's own sample type borrows
/// the fragment slice) and without its payload: where it lies, how to
/// convert it, and the object byte position whose arrival releases it.
struct SamplePlan {
    track: usize,
    timescale: u32,
    timestamp: u64,
    dts_ms: i64,
    composition_offset: i64,
    keyframe: bool,
    offset: usize,
    size: usize,
    release_at: usize,
}

/// fMP4 demux for one HLS push session.
#[derive(Default)]
pub struct Fmp4Demux {
    init: Option<Init>,
    object: Option<Object>,
    fragments: Vec<Range<usize>>,
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

    /// Plan each fragment the arrived bytes reach, and convert the samples
    /// whose share of their fragment has arrived. Conversion follows release,
    /// so a call converts what its chunk released: about one chunk of payload.
    fn advance(&mut self) -> Result<(), MediaRejection> {
        let Self {
            init,
            object,
            fragments,
            output,
            ..
        } = self;
        let Some(object) = object else {
            return Ok(());
        };
        let init = init
            .as_mut()
            .ok_or_else(|| malformed("fMP4 media segment before its init segment"))?;
        loop {
            if object.releasing.is_none() {
                let Some(range) = fragments.get(object.next_fragment).cloned() else {
                    break;
                };
                if range.start >= object.arrived {
                    break;
                }
                let fragment = object
                    .bytes
                    .get(range.clone())
                    .ok_or_else(|| malformed("fMP4 fragment runs past the object"))?;
                let samples = init
                    .demuxer
                    .handle_media_segment(fragment)
                    .map_err(|error| {
                        malformed(format!("fMP4 fragment at byte {}: {error}", range.start))
                    })?;
                let plans = plan_fragment(&init.tracks, &samples, &range)?;
                object.releasing = Some(Releasing {
                    range,
                    plans,
                    next: 0,
                });
            }
            let Some(releasing) = object.releasing.as_mut() else {
                break;
            };
            let fragment = object
                .bytes
                .get(releasing.range.clone())
                .ok_or_else(|| malformed("fMP4 fragment runs past the object"))?;
            while let Some(plan) = releasing.plans.get(releasing.next)
                && plan.release_at <= object.arrived
            {
                let track = init
                    .tracks
                    .get(plan.track)
                    .ok_or_else(|| malformed("fMP4 sample of an unknown track"))?;
                let data = plan
                    .offset
                    .checked_add(plan.size)
                    .map(|end| plan.offset..end)
                    .and_then(|span| fragment.get(span))
                    .ok_or_else(|| malformed("fMP4 sample runs past its fragment"))?;
                output.push(convert_sample(
                    track,
                    plan.timescale,
                    plan.timestamp,
                    plan.composition_offset,
                    plan.keyframe,
                    data,
                )?);
                releasing.next = releasing.next.saturating_add(1);
            }
            if releasing.next < releasing.plans.len() {
                break;
            }
            object.next_fragment = object.next_fragment.saturating_add(1);
            object.releasing = None;
        }
        Ok(())
    }
}

/// A fragment's samples as plans in decode order across tracks, each
/// released once the fragment's bytes up to its cumulative share have
/// arrived, like the interleaved TS stream it replaces. Samples of a track
/// the init does not publish are skipped.
fn plan_fragment(
    tracks: &[Track],
    samples: &[shiguredo_mp4::demux::Sample<'_>],
    range: &Range<usize>,
) -> Result<Vec<SamplePlan>, MediaRejection> {
    let mut plans = Vec::with_capacity(samples.len());
    for sample in samples {
        let Some(track) = tracks
            .iter()
            .position(|track| track.track_id == sample.track.track_id)
        else {
            continue;
        };
        let timescale = sample.track.timescale.get();
        let dts_ms = ticks_to_ms(i128::from(sample.timestamp), timescale)
            .ok_or_else(|| malformed("fMP4 sample timestamp is out of range"))?;
        let offset = usize::try_from(sample.data_offset)
            .map_err(|_| malformed("fMP4 sample runs past its fragment"))?;
        plans.push(SamplePlan {
            track,
            timescale,
            timestamp: sample.timestamp,
            dts_ms,
            composition_offset: sample.composition_time_offset.unwrap_or(0),
            keyframe: sample.keyframe,
            offset,
            size: sample.data_size,
            release_at: range.end,
        });
    }
    plans.sort_by_key(|plan| plan.dts_ms);
    let mut release_at = range.start;
    for plan in &mut plans {
        release_at = release_at.saturating_add(plan.size).min(range.end);
        plan.release_at = release_at;
    }
    Ok(plans)
}

impl SegmentDemux for Fmp4Demux {
    /// An fMP4 media segment is walked from the whole object; the chunks
    /// that follow pace how much of it is converted and released.
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
            releasing: None,
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

    /// The whole object has arrived: release the rest of it. A sample that
    /// fails here fails the segment like one that fails mid-object.
    fn end_object(&mut self) -> Result<(), MediaRejection> {
        if let Some(object) = self.object.as_mut() {
            object.arrived = object.bytes.len();
        }
        let released = self.advance();
        self.object = None;
        released
    }

    /// End of the stream: release the rest of the fragment in progress; the
    /// object's later fragments never arrived. Nothing follows to fail, so a
    /// sample that cannot be converted is logged and the rest dropped.
    fn flush(&mut self) {
        if let Some(object) = self.object.as_mut()
            && let Some(releasing) = &object.releasing
        {
            object.arrived = object.arrived.max(releasing.range.end);
        }
        if let Err(error) = self.advance() {
            tracing::warn!(
                %error,
                "[hls] fMP4 fragment tail dropped at end of stream"
            );
        }
        self.object = None;
    }

    /// An object that could not be demuxed is dropped: what it had demuxed
    /// but not yet released is discarded. Packets from an earlier fragment
    /// that were already released stay in the ring.
    fn discard_object(&mut self) {
        self.output.clear();
        self.object = None;
    }

    /// Fragments are anchored by their own `tfdt`, so the init stays; a new
    /// init arrives through [`SegmentDemux::apply_init`].
    fn discontinuity(&mut self) {
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
                    codec: Codec::Video {
                        nal: VideoNal::H264,
                        nalu_length_size: usize::from(avcc.length_size_minus_one.get())
                            .saturating_add(1),
                        parameter_sets,
                    },
                });
            }
            (MediaType::Video, SampleEntry::Hev1(hevc)) => {
                video_track_count = video_track_count.saturating_add(1);
                if video.is_none() {
                    let (meta, track) = hevc_track(track_id, &hevc.hvcc_box, &hevc.visual)?;
                    video = Some(meta);
                    tracks.push(track);
                }
            }
            (MediaType::Video, SampleEntry::Hvc1(hevc)) => {
                video_track_count = video_track_count.saturating_add(1);
                if video.is_none() {
                    let (meta, track) = hevc_track(track_id, &hevc.hvcc_box, &hevc.visual)?;
                    video = Some(meta);
                    tracks.push(track);
                }
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
                    "fMP4 video codec {} is not supported; H.264 or HEVC only",
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
        return Err(unsupported(
            "fMP4 init segment has no H.264, HEVC or AAC track",
        ));
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

/// An HEVC track from its `hvcC`: the VPS, SPS and PPS as Annex B, and the
/// probe a TS input carrying the same stream would report. HEVC has no
/// sequence header here, as in TS ingest; consumers that need one build it
/// from the parameter sets at the keyframe.
fn hevc_track(
    track_id: u32,
    hvcc: &shiguredo_mp4::boxes::HvccBox,
    visual: &shiguredo_mp4::boxes::VisualSampleEntryFields,
) -> Result<(VideoMeta, Track), MediaRejection> {
    let mut parameter_sets = Vec::new();
    for array in &hvcc.nalu_arrays {
        if !HEVC_PARAMETER_SETS.contains(&array.nal_unit_type.get()) {
            continue;
        }
        for nalu in &array.nalus {
            parameter_sets.extend_from_slice(&START_CODE);
            parameter_sets.extend_from_slice(nalu);
        }
    }
    let has_sps = hvcc
        .nalu_arrays
        .iter()
        .any(|array| array.nal_unit_type.get() == 33 && !array.nalus.is_empty());
    if !has_sps {
        return Err(malformed(format!("fMP4 track {track_id}: hvcC has no SPS")));
    }
    let mut meta = restream_mpegts::hevc_video_meta(&parameter_sets);
    if meta.width == 0 || meta.height == 0 {
        meta.width = u32::from(visual.width);
        meta.height = u32::from(visual.height);
    }
    let track = Track {
        track_id,
        media_type: MediaType::Video,
        track_index: 0,
        codec: Codec::Video {
            nal: VideoNal::H265,
            nalu_length_size: usize::from(hvcc.length_size_minus_one.get()).saturating_add(1),
            parameter_sets: Bytes::from(parameter_sets),
        },
    };
    Ok((meta, track))
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
    // An HEVC sample is a keyframe only if it also carries an IRAP picture
    // (NAL types 16-23): TS ingest requires the same of a random-access
    // flag, and a sync flag on a non-IRAP picture would inject parameter
    // sets, promote the gate and cut a segment at an undecodable picture.
    let mut keyframe = keyframe;
    let payload = match &track.codec {
        Codec::Video {
            nal,
            nalu_length_size,
            parameter_sets,
        } => {
            // One pass validates the framing (a truncated access unit is
            // refused, not published in part) and reads what the second
            // needs; neither allocates per NAL unit.
            let (mut irap, mut has_sps, mut annexb_len) = (false, false, 0usize);
            let framed = for_each_length_prefixed(*nalu_length_size, data, |nalu| {
                let header = nalu.first().copied().unwrap_or_default();
                irap |= (16..=23).contains(&((header >> 1) & 0x3F));
                has_sps |= nal.is_sps(header);
                annexb_len = annexb_len.saturating_add(nalu.len().saturating_add(START_CODE.len()));
            });
            if !framed {
                return Err(malformed(
                    "video sample ends inside a NALU length field or is empty",
                ));
            }
            if *nal == VideoNal::H265 {
                keyframe &= irap;
            }
            let inject = keyframe && !has_sps;
            let mut payload = Vec::with_capacity(annexb_len.saturating_add(if inject {
                parameter_sets.len()
            } else {
                0
            }));
            if inject {
                payload.extend_from_slice(parameter_sets);
            }
            for_each_length_prefixed(*nalu_length_size, data, |nalu| {
                payload.extend_from_slice(&START_CODE);
                payload.extend_from_slice(nalu);
            });
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

#[cfg(test)]
#[path = "fmp4_tests.rs"]
mod tests;
