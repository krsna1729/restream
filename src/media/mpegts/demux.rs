//! MPEG-TS demuxer for SRT ingest. Every byte is untrusted publisher input,
//! so the module may not index, slice, unwrap or do unchecked integer
//! arithmetic: whole packets are typed `&[u8; TS_PACKET_SIZE]` (constant
//! offsets are checked at compile time) and every input-derived offset goes
//! through `get`, slice patterns or checked arithmetic.
#![deny(
    clippy::indexing_slicing,
    clippy::arithmetic_side_effects,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic
)]

use std::collections::HashMap;

use bytes::Bytes;
use memchr::memchr;

use super::mpegts_probe::{
    audio_meta_complete, h264_is_keyframe, h265_is_keyframe, probe_audio, probe_video,
    video_meta_complete,
};
use super::wire::{PAT_PID, TS_PACKET_SIZE, TS_SYNC_BYTE, parse_timestamp, ts_to_ms};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::{MediaPacket, MediaType, PayloadFormat};

pub(super) const MAX_PES_BUFFER: usize = 512 * 1024;
const PID_COUNT: usize = 1 << 13;
const NO_STREAM: u16 = u16::MAX;
/// Sentinel meaning "continuity counter not yet observed". Valid CC values are 0–15.
pub(super) const CC_UNSET: u8 = u8::MAX;
/// Sentinel meaning "no PMT version parsed yet". Valid PMT version_number values are 0–31.
pub(super) const PMT_VER_UNSET: u8 = u8::MAX;

const STREAM_TYPE_H264: u8 = 0x1B;
const STREAM_TYPE_H265: u8 = 0x24;
const STREAM_TYPE_AAC_ADTS: u8 = 0x0F;
const STREAM_TYPE_AAC_LATM: u8 = 0x11;

fn pes_payload_len(pes_packet_len: usize, pes_header_len: usize) -> Option<usize> {
    if pes_packet_len == 0 {
        return None;
    }
    pes_packet_len.checked_sub(pes_header_len.checked_add(3)?)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamKind {
    H264,
    H265,
    AacAdts,
    AacLatm,
}

impl StreamKind {
    fn from_stream_type(st: u8) -> Option<Self> {
        match st {
            STREAM_TYPE_H264 => Some(Self::H264),
            STREAM_TYPE_H265 => Some(Self::H265),
            STREAM_TYPE_AAC_ADTS => Some(Self::AacAdts),
            STREAM_TYPE_AAC_LATM => Some(Self::AacLatm),
            _ => None,
        }
    }

    fn media_type(self) -> MediaType {
        match self {
            Self::H264 | Self::H265 => MediaType::Video,
            Self::AacAdts | Self::AacLatm => MediaType::Audio,
        }
    }

    pub(super) fn codec_name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "hevc",
            Self::AacAdts | Self::AacLatm => "aac",
        }
    }
}

#[derive(Debug)]
pub(super) struct PesAccumulator {
    pub(super) buf: Vec<u8>,
    pub(super) expected_payload_len: Option<usize>,
    pub(super) pts: i64,
    pub(super) dts: i64,
    pub(super) has_timestamp: bool,
    pub(super) random_access: bool,
}

impl PesAccumulator {
    pub(super) fn new() -> Self {
        Self {
            buf: Vec::with_capacity(16384),
            expected_payload_len: None,
            pts: 0,
            dts: 0,
            has_timestamp: false,
            random_access: false,
        }
    }

    /// Append PES payload unless that would exceed `MAX_PES_BUFFER` (an
    /// oversized frame is dropped, not grown without bound).
    fn append_bounded(&mut self, data: &[u8]) {
        if self.buf.len().saturating_add(data.len()) <= MAX_PES_BUFFER {
            self.buf.extend_from_slice(data);
        }
    }

    fn reset(&mut self) {
        self.buf.clear();
        self.expected_payload_len = None;
        self.pts = 0;
        self.dts = 0;
        self.has_timestamp = false;
        self.random_access = false;
    }
}

#[derive(Debug)]
pub(super) struct StreamInfo {
    /// The MPEG-TS elementary stream PID for this stream.
    /// Packet dispatch uses the
    /// `pid_to_stream` index table instead of a linear scan.
    pub(super) pid: u16,
    pub(super) kind: StreamKind,
    pub(super) track_index: u32,
    pub(super) language: Option<String>,
    pub(super) title: Option<String>,
    pub(super) continuity: u8,
    pub(super) pes: PesAccumulator,
}

/// Probe result matching the existing FFmpeg-based DemuxProbe.
#[derive(Debug, Clone)]
pub struct DemuxProbe {
    pub video: Option<VideoMeta>,
    pub video_sequence_header: Option<Bytes>,
    pub video_track_count: usize,
    pub audio_tracks: Vec<AudioMeta>,
}

#[derive(Debug, Clone, Default)]
struct StreamDescriptors {
    language: Option<String>,
    title: Option<String>,
}

fn parse_stream_descriptors(data: &[u8]) -> StreamDescriptors {
    let mut descriptors = StreamDescriptors::default();
    let mut rest = data;

    while let Some((&[tag, len], tail)) = rest.split_first_chunk::<2>() {
        let Some((payload, tail)) = tail.split_at_checked(usize::from(len)) else {
            break;
        };
        if tag == 0x0A
            && let Some(code) = payload.first_chunk::<3>()
            && let Ok(language) = std::str::from_utf8(code)
        {
            let language = language.trim().to_ascii_lowercase();
            if !language.is_empty() {
                descriptors.language = Some(language);
            }
        }
        rest = tail;
    }

    descriptors
}

/// A 12-bit section/info length from two bytes (the top 4 bits are flags).
fn length_12(high: u8, low: u8) -> usize {
    usize::from(u16::from_be_bytes([high & 0x0F, low]))
}

/// A 13-bit PID from two bytes (the top 3 bits are flags).
fn pid_13(high: u8, low: u8) -> u16 {
    u16::from_be_bytes([high & 0x1F, low])
}

fn pmt_stream_loop_bounds(data: &[u8], end: usize) -> Option<(usize, usize)> {
    let stream_loop_end = end.checked_sub(4)?;
    if end > data.len() || stream_loop_end < 12 {
        return None;
    }

    let &[high, low] = data.get(10..12)?.first_chunk::<2>()?;
    let stream_loop_start = 12usize.checked_add(length_12(high, low))?;
    let mut entries = data.get(stream_loop_start..stream_loop_end)?;
    while !entries.is_empty() {
        let (&[_, _, _, info_high, info_low], tail) = entries.split_first_chunk::<5>()?;
        entries = tail.get(length_12(info_high, info_low)..)?;
    }

    Some((stream_loop_start, stream_loop_end))
}

/// Streaming MPEG-TS demuxer. Feed it chunks of TS data and drain packets.
pub struct TsDemuxer {
    pub(super) streams: Vec<StreamInfo>,
    pub(super) pid_to_stream: Box<[u16; PID_COUNT]>,
    pmt_pid: Option<u16>,
    probed: bool,
    probe_result: Option<DemuxProbe>,
    pub(super) remainder: Vec<u8>,
    output: Vec<MediaPacket>,
    audio_track_counter: u32,
    video_track_count: usize,
    probe_payloads: Vec<Option<Vec<u8>>>,
    pmt_buf: Vec<u8>,
    pmt_expected: usize,
    /// Last seen PMT version_number (bits 5–1 of the version/indicator byte).
    /// PMT_VER_UNSET (u8::MAX) means no PMT seen yet; valid values are 0–31.
    pub(super) pmt_version: u8,
}

impl Default for TsDemuxer {
    fn default() -> Self {
        Self::new()
    }
}

impl TsDemuxer {
    pub fn new() -> Self {
        Self {
            streams: Vec::new(),
            pid_to_stream: Box::new([NO_STREAM; PID_COUNT]),
            pmt_pid: None,
            probed: false,
            probe_result: None,
            remainder: Vec::new(),
            output: Vec::with_capacity(16),
            audio_track_counter: 0,
            video_track_count: 0,
            probe_payloads: Vec::new(),
            pmt_buf: Vec::new(),
            pmt_expected: 0,
            pmt_version: PMT_VER_UNSET,
        }
    }

    /// Feed raw bytes (potentially multiple TS packets or partial ones).
    pub fn feed(&mut self, data: &[u8]) {
        if self.remainder.is_empty() {
            let leftover = self.feed_slice(data);
            self.remainder
                .extend_from_slice(data.get(leftover..).unwrap_or_default());
        } else {
            self.remainder.extend_from_slice(data);
            let buf = std::mem::take(&mut self.remainder);
            let leftover = self.feed_slice(&buf);
            self.remainder
                .extend_from_slice(buf.get(leftover..).unwrap_or_default());
        }
        // Safety cap: remainder must never exceed TS_PACKET_SIZE-1 bytes.
        // feed_slice guarantees the unprocessed tail is < TS_PACKET_SIZE under
        // normal operation (it processes every complete 188-byte block it can).
        // This explicit cap prevents accumulation in edge cases — e.g. when
        // find_ts_sync optimistically accepts a 0x47 byte near the end of a
        // short chunk but the next chunk also starts with 0x47, causing the
        // buffer to grow one byte per call before the 188-byte threshold is
        // reached and the block is processed or discarded.
        const MAX_REMAINDER: usize = TS_PACKET_SIZE - 1;
        if let Some(excess) = self.remainder.len().checked_sub(MAX_REMAINDER) {
            self.remainder.drain(..excess);
        }
    }

    /// Process every whole packet in `buf`; returns where the unprocessed
    /// tail starts.
    fn feed_slice(&mut self, buf: &[u8]) -> usize {
        let mut offset = find_ts_sync(buf);

        while let Some(packet) = buf
            .get(offset..)
            .and_then(<[u8]>::first_chunk::<TS_PACKET_SIZE>)
        {
            if packet[0] != TS_SYNC_BYTE {
                // Resync after this byte: `offset + 1 + next` never passes
                // `buf.len()`, so the additions cannot overflow.
                let next = find_ts_sync(buf.get(offset.saturating_add(1)..).unwrap_or_default());
                offset = offset.saturating_add(1).saturating_add(next);
                continue;
            }
            self.process_ts_packet(packet);
            offset = offset.saturating_add(TS_PACKET_SIZE);
        }

        offset
    }

    /// Drain completed media packets.
    pub fn drain(&mut self) -> Vec<MediaPacket> {
        std::mem::take(&mut self.output)
    }

    /// Move completed packets into a caller-owned reusable batch.
    ///
    /// Unlike `drain()`, this keeps the demuxer's output allocation available
    /// for subsequent receives. Callers should consume `output.drain(..)` to
    /// retain their batch allocation too.
    pub fn drain_into(&mut self, output: &mut Vec<MediaPacket>) -> usize {
        let moved = self.output.len();
        output.append(&mut self.output);
        moved
    }

    /// Take the probe result (available after the first PMT + PES headers are parsed).
    pub fn take_probe(&mut self) -> Option<DemuxProbe> {
        self.probe_result.take()
    }

    /// Whether PMT has been parsed and streams are known.
    pub fn has_streams(&self) -> bool {
        !self.streams.is_empty()
    }

    pub(super) fn process_ts_packet(&mut self, pkt: &[u8; TS_PACKET_SIZE]) {
        let [_, flags, pid_low, control, ref after_header @ ..] = *pkt;
        let pid = pid_13(flags, pid_low);
        let payload_unit_start = flags & 0x40 != 0;
        let adaptation_field_control = (control >> 4) & 0x03;
        let continuity_counter = control & 0x0F;

        // 0b00 reserved and 0b10 adaptation field only: no payload.
        if adaptation_field_control == 0x00 || adaptation_field_control == 0x02 {
            return;
        }
        let mut random_access = false;
        let payload: &[u8] = if adaptation_field_control == 0x03 {
            let Some((&af_len, rest)) = after_header.split_first() else {
                return;
            };
            if af_len > 0 {
                random_access = rest.first().is_some_and(|af_flags| af_flags & 0x40 != 0);
            }
            rest.get(usize::from(af_len)..).unwrap_or_default()
        } else {
            after_header
        };
        if payload.is_empty() {
            return;
        }

        if pid == PAT_PID {
            self.parse_pat(payload, payload_unit_start);
            return;
        }

        if Some(pid) == self.pmt_pid {
            self.parse_pmt(payload, payload_unit_start);
            return;
        }

        let Some(&stream_idx) = self.pid_to_stream.get(usize::from(pid)) else {
            return;
        };
        if stream_idx == NO_STREAM {
            return;
        }
        let stream_idx = usize::from(stream_idx);
        let Some(stream) = self.streams.get_mut(stream_idx) else {
            return;
        };
        stream.continuity = continuity_counter;

        if payload_unit_start {
            self.flush_pes(stream_idx);

            if let Some(
                &[
                    0x00,
                    0x00,
                    0x01,
                    _,
                    len_high,
                    len_low,
                    _,
                    pes_flags,
                    header_len,
                ],
            ) = payload.first_chunk::<9>()
                && let Some(stream) = self.streams.get_mut(stream_idx)
            {
                let pes_packet_len = usize::from(u16::from_be_bytes([len_high, len_low]));
                let pes_header_len = usize::from(header_len);
                let has_pts = pes_flags & 0x80 != 0;
                let has_dts = pes_flags & 0x40 != 0;

                stream.pes.random_access = random_access;
                stream.pes.expected_payload_len = pes_payload_len(pes_packet_len, pes_header_len);

                let header = payload.get(9..).unwrap_or_default();
                if has_pts && let Some(pts) = header.first_chunk::<5>() {
                    stream.pes.pts = parse_timestamp(pts);
                    stream.pes.has_timestamp = true;
                }
                if has_dts && let Some(dts) = header.get(5..).and_then(<[u8]>::first_chunk::<5>) {
                    stream.pes.dts = parse_timestamp(dts);
                } else if has_pts {
                    stream.pes.dts = stream.pes.pts;
                }

                if let Some(pes_data) = header.get(pes_header_len..)
                    && !pes_data.is_empty()
                {
                    stream.pes.append_bounded(pes_data);
                }
                self.flush_completed_pes(stream_idx);
            }
        } else {
            stream.pes.append_bounded(payload);
            self.flush_completed_pes(stream_idx);
        }
    }

    fn flush_completed_pes(&mut self, stream_idx: usize) {
        let Some(stream) = self.streams.get_mut(stream_idx) else {
            return;
        };
        let Some(expected) = stream.pes.expected_payload_len else {
            return;
        };
        if stream.pes.buf.len() >= expected {
            stream.pes.buf.truncate(expected);
            self.flush_pes(stream_idx);
        }
    }

    fn flush_pes(&mut self, stream_idx: usize) {
        let Some(stream) = self.streams.get_mut(stream_idx) else {
            return;
        };
        if stream.pes.buf.is_empty() || !stream.pes.has_timestamp {
            stream.pes.reset();
            return;
        }

        let kind = stream.kind;
        let track_index = stream.track_index;
        let pts_90k = stream.pes.pts;
        let dts_90k = stream.pes.dts;
        let random_access = stream.pes.random_access;

        // Copy payload to a fresh Bytes, then reset the PES buffer keeping its
        // heap capacity for the next frame.  Using std::mem::take() would strip
        // the Vec capacity (leaving a 0-capacity Vec), forcing 3–8 reallocs on
        // the next PES reassembly. copy_from_slice costs one allocation of exactly
        // the frame size but keeps the PES buf warm — net saving for typical streams.
        let payload = Bytes::copy_from_slice(&stream.pes.buf);
        stream.pes.reset();

        let pts_ms = ts_to_ms(pts_90k);
        let dts_ms = ts_to_ms(dts_90k);

        let is_keyframe = match kind {
            StreamKind::H264 => random_access || h264_is_keyframe(&payload),
            StreamKind::H265 => h265_is_keyframe(&payload),
            _ => false,
        };

        if !self.probed {
            self.try_build_probe(stream_idx, &payload);
        }

        self.output.push(MediaPacket {
            media_type: kind.media_type(),
            track_index,
            pts: pts_ms,
            dts: dts_ms,
            is_keyframe,
            format: PayloadFormat::Raw,
            payload,
        });
    }

    fn parse_pat(&mut self, payload: &[u8], pusi: bool) {
        let data = if pusi {
            match section_after_pointer(payload) {
                Some(section) => section,
                None => return,
            }
        } else {
            payload
        };
        let Some(&[0x00, length_high, length_low, ..]) = data.first_chunk::<8>() else {
            return;
        };

        // Programs run from byte 8 to the CRC32 that ends the section.
        let end = length_12(length_high, length_low)
            .saturating_add(3)
            .min(data.len())
            .saturating_sub(4);
        let Some(programs) = data.get(8..end) else {
            return;
        };
        for program in programs.chunks_exact(4) {
            if let &[number_high, number_low, pid_high, pid_low] = program
                && u16::from_be_bytes([number_high, number_low]) != 0
            {
                self.pmt_pid = Some(pid_13(pid_high, pid_low));
                break;
            }
        }
    }

    fn parse_pmt(&mut self, payload: &[u8], pusi: bool) {
        if pusi {
            let Some(data) = section_after_pointer(payload) else {
                return;
            };
            let Some(&[0x02, length_high, length_low]) = data.first_chunk::<3>() else {
                return;
            };
            self.pmt_expected = length_12(length_high, length_low).saturating_add(3);
            self.pmt_buf.clear();
            self.pmt_buf.extend_from_slice(data);
        } else if self.pmt_expected > 0 {
            self.pmt_buf.extend_from_slice(payload);
        } else {
            return;
        }

        if self.pmt_buf.len() < self.pmt_expected {
            return;
        }

        let data = &self.pmt_buf;
        let end = self.pmt_expected.min(data.len());

        let (Some((loop_start, loop_end)), Some(&version_byte)) =
            (pmt_stream_loop_bounds(data, end), data.get(5))
        else {
            self.pmt_buf.clear();
            self.pmt_expected = 0;
            return;
        };

        let incoming_version = (version_byte >> 1) & 0x1F;
        if self.pmt_version == incoming_version {
            self.pmt_buf.clear();
            self.pmt_expected = 0;
            return;
        }
        self.pmt_version = incoming_version;

        // Preserve in-flight PES for PIDs retained by a PMT version update.
        let mut old_pes: HashMap<u16, PesAccumulator> =
            self.streams.drain(..).map(|s| (s.pid, s.pes)).collect();
        self.pid_to_stream.fill(NO_STREAM);
        self.audio_track_counter = 0;
        self.video_track_count = 0;
        self.probe_payloads.clear();

        let mut has_video = false;
        // `pmt_stream_loop_bounds` validated that these entries tile the loop.
        let mut entries = data.get(loop_start..loop_end).unwrap_or_default();
        while let Some((&[stream_type, pid_high, pid_low, info_high, info_low], tail)) =
            entries.split_first_chunk::<5>()
        {
            let es_pid = pid_13(pid_high, pid_low);
            let Some((descriptor_bytes, tail)) =
                tail.split_at_checked(length_12(info_high, info_low))
            else {
                break;
            };
            entries = tail;
            let descriptors = parse_stream_descriptors(descriptor_bytes);

            if let Some(kind) = StreamKind::from_stream_type(stream_type) {
                let track_index = match kind.media_type() {
                    MediaType::Video => {
                        self.video_track_count = self.video_track_count.saturating_add(1);
                        if has_video {
                            continue;
                        }
                        has_video = true;
                        0
                    }
                    MediaType::Audio => {
                        let idx = self.audio_track_counter;
                        self.audio_track_counter = self.audio_track_counter.saturating_add(1);
                        idx
                    }
                };

                let stream_idx = self.streams.len();
                let pes = old_pes.remove(&es_pid).unwrap_or_else(PesAccumulator::new);
                self.streams.push(StreamInfo {
                    pid: es_pid,
                    kind,
                    track_index,
                    language: descriptors.language.clone(),
                    title: descriptors.title.clone(),
                    continuity: CC_UNSET,
                    pes,
                });
                if let (Some(slot), Ok(index)) = (
                    self.pid_to_stream.get_mut(usize::from(es_pid)),
                    u16::try_from(stream_idx),
                ) {
                    *slot = index;
                }
            }
        }

        self.pmt_buf.clear();
        self.pmt_expected = 0;
    }

    fn probe_payload_complete(&self, stream_idx: usize, payload: &[u8]) -> bool {
        let Some(stream) = self.streams.get(stream_idx) else {
            return false;
        };
        match stream.kind.media_type() {
            MediaType::Video => video_meta_complete(
                stream.kind,
                &probe_video(stream.kind, stream.pid, None, None, payload),
            ),
            MediaType::Audio => audio_meta_complete(
                stream.kind,
                &probe_audio(
                    stream.kind,
                    stream.track_index,
                    stream.pid,
                    None,
                    None,
                    payload,
                ),
            ),
        }
    }

    pub(super) fn try_build_probe(&mut self, stream_idx: usize, payload: &[u8]) {
        if self.probe_payloads.len() < self.streams.len() {
            self.probe_payloads.resize(self.streams.len(), None);
        }
        let replace = match self.probe_payloads.get(stream_idx) {
            None => return,
            Some(None) => true,
            Some(Some(existing)) => !self.probe_payload_complete(stream_idx, existing),
        };
        if replace && let Some(slot) = self.probe_payloads.get_mut(stream_idx) {
            *slot = Some(payload.to_vec());
        }

        if self.probe_payloads.iter().any(|p| p.is_none()) {
            return;
        }

        let mut video_meta = None;
        let mut audio_tracks = Vec::new();
        let mut video_sequence_header = None;
        let mut probe_complete = true;

        for (stream, data) in self.streams.iter().zip(&self.probe_payloads) {
            let Some(data) = data.as_deref() else {
                return;
            };
            match stream.kind.media_type() {
                MediaType::Video => {
                    if video_meta.is_none() {
                        let meta = probe_video(
                            stream.kind,
                            stream.pid,
                            stream.language.clone(),
                            stream.title.clone(),
                            data,
                        );
                        if stream.kind == StreamKind::H264 {
                            video_sequence_header =
                                crate::media::codec::build_avcc_sequence_header(data);
                        }
                        probe_complete &= video_meta_complete(stream.kind, &meta);
                        video_meta = Some(meta);
                    }
                }
                MediaType::Audio => {
                    let meta = probe_audio(
                        stream.kind,
                        stream.track_index,
                        stream.pid,
                        stream.language.clone(),
                        stream.title.clone(),
                        data,
                    );
                    probe_complete &= audio_meta_complete(stream.kind, &meta);
                    audio_tracks.push(meta);
                }
            }
        }

        if !probe_complete {
            return;
        }

        let has_video_meta = video_meta.is_some();
        self.probed = true;
        self.probe_payloads.clear();
        self.probe_result = Some(DemuxProbe {
            video: video_meta,
            video_sequence_header,
            video_track_count: self.video_track_count.max(usize::from(has_video_meta)),
            audio_tracks,
        });
    }

    /// Flush any remaining PES data for all streams (call at end of input).
    pub fn flush(&mut self) {
        for idx in 0..self.streams.len() {
            self.flush_pes(idx);
        }
    }
}

#[inline]
pub(super) fn find_ts_sync(data: &[u8]) -> usize {
    if ts_sync_candidate_is_valid(data, 0) {
        return 0;
    }

    let mut search_offset = 0usize;
    while let Some(rest) = data.get(search_offset..)
        && let Some(relative) = memchr(TS_SYNC_BYTE, rest)
    {
        // `candidate < data.len()`, so neither addition can overflow.
        let candidate = search_offset.saturating_add(relative);
        if ts_sync_candidate_is_valid(data, candidate) {
            return candidate;
        }
        search_offset = candidate.saturating_add(1);
    }
    data.len()
}

/// A PSI section after its `pointer_field`, if the pointer stays inside the
/// payload (and leaves at least one byte).
fn section_after_pointer(payload: &[u8]) -> Option<&[u8]> {
    let (&pointer, rest) = payload.split_first()?;
    rest.get(usize::from(pointer)..)
        .filter(|section| !section.is_empty())
}

pub(super) fn ts_sync_candidate_is_valid(data: &[u8], candidate: usize) -> bool {
    if data.get(candidate) != Some(&TS_SYNC_BYTE) {
        return false;
    }

    let remaining = data.len().saturating_sub(candidate);
    if remaining <= TS_PACKET_SIZE {
        return true;
    }
    let second = candidate.saturating_add(TS_PACKET_SIZE);
    if data.get(second) != Some(&TS_SYNC_BYTE) {
        return false;
    }
    remaining <= 2 * TS_PACKET_SIZE
        || data.get(second.saturating_add(TS_PACKET_SIZE)) == Some(&TS_SYNC_BYTE)
}
