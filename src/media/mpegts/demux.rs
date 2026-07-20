use std::collections::HashMap;

use bytes::Bytes;
use memchr::memchr;

use super::mpegts_probe::{
    audio_meta_complete, h264_is_keyframe, h265_is_keyframe, probe_audio, probe_video,
    video_meta_complete,
};
use super::wire::{
    PAT_PID, PES_START_CODE, TS_PACKET_SIZE, TS_SYNC_BYTE, parse_timestamp, ts_to_ms,
};
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
    pes_packet_len.checked_sub(3 + pes_header_len)
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
    let mut pos = 0usize;

    while pos + 2 <= data.len() {
        let tag = data[pos];
        let len = data[pos + 1] as usize;
        let start = pos + 2;
        let end = start.saturating_add(len);
        if end > data.len() {
            break;
        }

        let payload = &data[start..end];
        if tag == 0x0A
            && payload.len() >= 3
            && let Ok(language) = std::str::from_utf8(&payload[..3])
        {
            let language = language.trim().to_ascii_lowercase();
            if !language.is_empty() {
                descriptors.language = Some(language);
            }
        }

        pos = end;
    }

    descriptors
}

fn pmt_stream_loop_bounds(data: &[u8], end: usize) -> Option<(usize, usize)> {
    let stream_loop_end = end.checked_sub(4)?;
    if end > data.len() || stream_loop_end < 12 {
        return None;
    }

    let program_info_len = ((data[10] as usize & 0x0F) << 8) | data[11] as usize;
    let stream_loop_start = 12usize.checked_add(program_info_len)?;
    if stream_loop_start > stream_loop_end {
        return None;
    }

    let mut pos = stream_loop_start;
    while pos < stream_loop_end {
        let descriptor_start = pos.checked_add(5)?;
        if descriptor_start > stream_loop_end {
            return None;
        }
        let es_info_len = ((data[pos + 3] as usize & 0x0F) << 8) | data[pos + 4] as usize;
        pos = descriptor_start.checked_add(es_info_len)?;
        if pos > stream_loop_end {
            return None;
        }
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
            if leftover < data.len() {
                self.remainder.extend_from_slice(&data[leftover..]);
            }
        } else {
            self.remainder.extend_from_slice(data);
            let buf = std::mem::take(&mut self.remainder);
            let leftover = self.feed_slice(&buf);
            if leftover < buf.len() {
                self.remainder.extend_from_slice(&buf[leftover..]);
            }
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
        if self.remainder.len() > MAX_REMAINDER {
            let excess = self.remainder.len() - MAX_REMAINDER;
            self.remainder.drain(..excess);
        }
    }

    fn feed_slice(&mut self, buf: &[u8]) -> usize {
        let mut offset = find_ts_sync(buf);

        while offset + TS_PACKET_SIZE <= buf.len() {
            if buf[offset] != TS_SYNC_BYTE {
                let next = find_ts_sync(&buf[offset + 1..]);
                offset += 1 + next;
                continue;
            }
            self.process_ts_packet(&buf[offset..offset + TS_PACKET_SIZE]);
            offset += TS_PACKET_SIZE;
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
        let start_len = output.len();
        output.append(&mut self.output);
        output.len() - start_len
    }

    /// Take the probe result (available after the first PMT + PES headers are parsed).
    pub fn take_probe(&mut self) -> Option<DemuxProbe> {
        self.probe_result.take()
    }

    /// Whether PMT has been parsed and streams are known.
    pub fn has_streams(&self) -> bool {
        !self.streams.is_empty()
    }

    pub(super) fn process_ts_packet(&mut self, pkt: &[u8]) {
        let pid = (((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16;
        let payload_unit_start = pkt[1] & 0x40 != 0;
        let adaptation_field_control = (pkt[3] >> 4) & 0x03;
        let continuity_counter = pkt[3] & 0x0F;

        let mut payload_offset = 4;
        let mut random_access = false;

        if (adaptation_field_control == 0x02 || adaptation_field_control == 0x03)
            && payload_offset < TS_PACKET_SIZE
        {
            let af_len = pkt[payload_offset] as usize;
            payload_offset += 1;
            if af_len > 0 && payload_offset < TS_PACKET_SIZE {
                let af_flags = pkt[payload_offset];
                random_access = af_flags & 0x40 != 0;
            }
            payload_offset += af_len;
        }

        if adaptation_field_control == 0x00 || adaptation_field_control == 0x02 {
            return;
        }

        if payload_offset >= TS_PACKET_SIZE {
            return;
        }

        let payload = &pkt[payload_offset..TS_PACKET_SIZE];

        if pid == PAT_PID {
            self.parse_pat(payload, payload_unit_start);
            return;
        }

        if Some(pid) == self.pmt_pid {
            self.parse_pmt(payload, payload_unit_start);
            return;
        }

        let stream_idx = self.pid_to_stream[pid as usize];
        if stream_idx == NO_STREAM {
            return;
        }
        let stream_idx = stream_idx as usize;
        self.streams[stream_idx].continuity = continuity_counter;

        if payload_unit_start {
            self.flush_pes(stream_idx);

            if payload.len() >= 9 && payload[0..3] == PES_START_CODE {
                let pes_packet_len = u16::from_be_bytes([payload[4], payload[5]]) as usize;
                let pes_header_len = payload[8] as usize;
                let flags = payload[7];
                let has_pts = flags & 0x80 != 0;
                let has_dts = flags & 0x40 != 0;

                let stream = &mut self.streams[stream_idx];
                stream.pes.random_access = random_access;
                stream.pes.expected_payload_len = pes_payload_len(pes_packet_len, pes_header_len);

                if has_pts && payload.len() >= 14 {
                    stream.pes.pts = parse_timestamp(&payload[9..14]);
                    stream.pes.has_timestamp = true;
                }
                if has_dts && payload.len() >= 19 {
                    stream.pes.dts = parse_timestamp(&payload[14..19]);
                } else if has_pts {
                    stream.pes.dts = stream.pes.pts;
                }

                let data_start = 9 + pes_header_len;
                if data_start < payload.len() {
                    let pes_data = &payload[data_start..];
                    if stream.pes.buf.len() + pes_data.len() <= MAX_PES_BUFFER {
                        stream.pes.buf.extend_from_slice(pes_data);
                    }
                }
                self.flush_completed_pes(stream_idx);
            }
        } else {
            let stream = &mut self.streams[stream_idx];
            if stream.pes.buf.len() + payload.len() <= MAX_PES_BUFFER {
                stream.pes.buf.extend_from_slice(payload);
            }
            self.flush_completed_pes(stream_idx);
        }
    }

    fn flush_completed_pes(&mut self, stream_idx: usize) {
        let Some(expected) = self.streams[stream_idx].pes.expected_payload_len else {
            return;
        };
        if self.streams[stream_idx].pes.buf.len() >= expected {
            self.streams[stream_idx].pes.buf.truncate(expected);
            self.flush_pes(stream_idx);
        }
    }

    fn flush_pes(&mut self, stream_idx: usize) {
        let stream = &mut self.streams[stream_idx];
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
        self.streams[stream_idx].pes.reset();

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
        let data = if pusi && !payload.is_empty() {
            let pointer = payload[0] as usize;
            if 1 + pointer >= payload.len() {
                return;
            }
            &payload[1 + pointer..]
        } else {
            payload
        };

        if data.len() < 8 || data[0] != 0x00 {
            return;
        }

        let section_length = ((data[1] as usize & 0x0F) << 8) | data[2] as usize;
        let end = (3 + section_length).min(data.len());
        let mut pos = 8;
        while pos + 4 <= end.saturating_sub(4) {
            let program_num = ((data[pos] as u16) << 8) | data[pos + 1] as u16;
            let pid = ((data[pos + 2] as u16 & 0x1F) << 8) | data[pos + 3] as u16;
            if program_num != 0 {
                self.pmt_pid = Some(pid);
                break;
            }
            pos += 4;
        }
    }

    fn parse_pmt(&mut self, payload: &[u8], pusi: bool) {
        if pusi {
            let data = if !payload.is_empty() {
                let pointer = payload[0] as usize;
                if 1 + pointer >= payload.len() {
                    return;
                }
                &payload[1 + pointer..]
            } else {
                return;
            };

            if data.len() < 3 || data[0] != 0x02 {
                return;
            }
            let section_length = ((data[1] as usize & 0x0F) << 8) | data[2] as usize;
            self.pmt_expected = 3 + section_length;
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

        let Some((mut pos, stream_loop_end)) = pmt_stream_loop_bounds(data, end) else {
            self.pmt_buf.clear();
            self.pmt_expected = 0;
            return;
        };

        let incoming_version = (data[5] >> 1) & 0x1F;
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
        while pos < stream_loop_end {
            let stream_type = data[pos];
            let es_pid = ((data[pos + 1] as u16 & 0x1F) << 8) | data[pos + 2] as u16;
            let es_info_len = ((data[pos + 3] as usize & 0x0F) << 8) | data[pos + 4] as usize;
            let desc_start = pos + 5;
            let desc_end = desc_start + es_info_len;
            let descriptors = parse_stream_descriptors(&data[desc_start..desc_end]);
            pos = desc_end;

            if let Some(kind) = StreamKind::from_stream_type(stream_type) {
                let track_index = match kind.media_type() {
                    MediaType::Video => {
                        self.video_track_count += 1;
                        if has_video {
                            continue;
                        }
                        has_video = true;
                        0
                    }
                    MediaType::Audio => {
                        let idx = self.audio_track_counter;
                        self.audio_track_counter += 1;
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
                self.pid_to_stream[es_pid as usize] = stream_idx as u16;
            }
        }

        self.pmt_buf.clear();
        self.pmt_expected = 0;
    }

    fn probe_payload_complete(&self, stream_idx: usize, payload: &[u8]) -> bool {
        let stream = &self.streams[stream_idx];
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
        let replace = match self.probe_payloads[stream_idx].as_deref() {
            None => true,
            Some(existing) => !self.probe_payload_complete(stream_idx, existing),
        };
        if replace {
            self.probe_payloads[stream_idx] = Some(payload.to_vec());
        }

        if self.probe_payloads.iter().any(|p| p.is_none()) {
            return;
        }

        let mut video_meta = None;
        let mut audio_tracks = Vec::new();
        let mut video_sequence_header = None;
        let mut probe_complete = true;

        for (idx, stream) in self.streams.iter().enumerate() {
            let data = self.probe_payloads[idx].as_deref().unwrap();
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
    while search_offset < data.len() {
        let Some(relative) = memchr(TS_SYNC_BYTE, &data[search_offset..]) else {
            return data.len();
        };
        let candidate = search_offset + relative;
        if ts_sync_candidate_is_valid(data, candidate) {
            return candidate;
        }
        search_offset = candidate + 1;
    }
    data.len()
}

pub(super) fn ts_sync_candidate_is_valid(data: &[u8], candidate: usize) -> bool {
    if data.get(candidate) != Some(&TS_SYNC_BYTE) {
        return false;
    }

    let remaining = data.len() - candidate;
    if remaining <= TS_PACKET_SIZE {
        return true;
    }
    if data.get(candidate + TS_PACKET_SIZE) != Some(&TS_SYNC_BYTE) {
        return false;
    }
    remaining <= TS_PACKET_SIZE * 2
        || data.get(candidate + TS_PACKET_SIZE * 2) == Some(&TS_SYNC_BYTE)
}
