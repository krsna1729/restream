//! Low-level MPEG-TS muxing, demuxing, and metadata extraction shared by
//! ingest, HLS, recording, and transcoding paths.

use bytes::Bytes;
use tracing::error;

use crate::media::engine::{AudioMeta, VideoMeta};
use crate::media::ring_buffer::{MediaPacket, MediaType, PayloadFormat};
use memchr::memchr;

#[path = "mpegts_probe.rs"]
mod mpegts_probe;
use mpegts_probe::{
    audio_meta_complete, h264_is_keyframe, h265_is_keyframe, probe_audio, probe_video,
    video_meta_complete,
};

const TS_PACKET_SIZE: usize = 188;
const TS_SYNC_BYTE: u8 = 0x47;
const PAT_PID: u16 = 0x0000;
const SDT_PID: u16 = 0x0011;
const PES_START_CODE: [u8; 3] = [0x00, 0x00, 0x01];
const MAX_PES_BUFFER: usize = 512 * 1024;
const PID_COUNT: usize = 1 << 13;
const NO_STREAM: u16 = u16::MAX;
/// Sentinel meaning "continuity counter not yet observed". Valid CC values are 0–15.
const CC_UNSET: u8 = u8::MAX;
/// Sentinel meaning "no PMT version parsed yet". Valid PMT version_number values are 0–31.
const PMT_VER_UNSET: u8 = u8::MAX;

// MPEG-TS stream type constants
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
enum StreamKind {
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

    fn codec_name(self) -> &'static str {
        match self {
            Self::H264 => "h264",
            Self::H265 => "hevc",
            Self::AacAdts | Self::AacLatm => "aac",
        }
    }
}

#[derive(Debug)]
struct PesAccumulator {
    buf: Vec<u8>,
    expected_payload_len: Option<usize>,
    pts: i64,
    dts: i64,
    has_timestamp: bool,
    random_access: bool,
}

impl PesAccumulator {
    fn new() -> Self {
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
struct StreamInfo {
    /// The MPEG-TS elementary stream PID for this stream.
    /// Packet dispatch uses the
    /// `pid_to_stream` index table instead of a linear scan.
    pid: u16,
    kind: StreamKind,
    track_index: u32,
    language: Option<String>,
    title: Option<String>,
    continuity: u8, // CC_UNSET (u8::MAX) = not yet seen; valid CC values are 0–15
    pes: PesAccumulator,
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
    let stream_loop_end = end.checked_sub(4)?; // trailing CRC32
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
    streams: Vec<StreamInfo>,
    pid_to_stream: Box<[u16; PID_COUNT]>,
    pmt_pid: Option<u16>,
    probed: bool,
    probe_result: Option<DemuxProbe>,
    remainder: Vec<u8>,
    output: Vec<MediaPacket>,
    audio_track_counter: u32,
    video_track_count: usize,
    probe_payloads: Vec<Option<Vec<u8>>>,
    pmt_buf: Vec<u8>,
    pmt_expected: usize,
    /// Last seen PMT version_number (bits 5–1 of the version/indicator byte).
    /// PMT_VER_UNSET (u8::MAX) means no PMT seen yet; valid values are 0–31.
    pmt_version: u8,
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

    /// Process complete TS packets from a slice. Returns the offset of unconsumed bytes.
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

    fn process_ts_packet(&mut self, pkt: &[u8]) {
        let pid = (((pkt[1] & 0x1F) as u16) << 8) | pkt[2] as u16;
        let payload_unit_start = pkt[1] & 0x40 != 0;
        let adaptation_field_control = (pkt[3] >> 4) & 0x03;
        let continuity_counter = pkt[3] & 0x0F;

        let mut payload_offset = 4;
        let mut random_access = false;

        // Adaptation field
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

        // No payload
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

        // MPEG-TS PIDs are 13-bit values, so direct dispatch avoids a linear
        // stream scan for every 188-byte packet.
        let stream_idx = self.pid_to_stream[pid as usize];
        if stream_idx == NO_STREAM {
            return;
        }
        let stream_idx = stream_idx as usize;

        // Continuity check (just track it, don't drop packets)
        self.streams[stream_idx].continuity = continuity_counter;

        if payload_unit_start {
            // Flush previous PES before starting new one
            self.flush_pes(stream_idx);

            // Parse PES header
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
            // Continuation of PES
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
        self.streams[stream_idx].pes.reset(); // clears buf, preserves capacity

        // Convert 90kHz to milliseconds
        let pts_ms = ts_to_ms(pts_90k);
        let dts_ms = ts_to_ms(dts_90k);

        let is_keyframe = match kind {
            StreamKind::H264 => random_access || h264_is_keyframe(&payload),
            StreamKind::H265 => h265_is_keyframe(&payload),
            _ => false,
        };

        // Build probe on first video/audio PES
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

        // PAT: table_id(1) + flags(2) + transport_stream_id(2) + version(1) + section(1) + last_section(1) + entries(4 each)
        if data.len() < 8 {
            return;
        }
        if data[0] != 0x00 {
            return; // table_id must be 0 for PAT
        }

        let section_length = ((data[1] as usize & 0x0F) << 8) | data[2] as usize;
        let end = (3 + section_length).min(data.len());

        // Skip 5 bytes (tsid + version + section numbers), then 4 bytes per entry, minus 4 byte CRC
        let mut pos = 8;
        while pos + 4 <= end.saturating_sub(4) {
            let program_num = ((data[pos] as u16) << 8) | data[pos + 1] as u16;
            let pid = ((data[pos + 2] as u16 & 0x1F) << 8) | data[pos + 3] as u16;
            if program_num != 0 {
                self.pmt_pid = Some(pid);
                break; // Single-program assumption
            }
            pos += 4;
        }
    }

    fn parse_pmt(&mut self, payload: &[u8], pusi: bool) {
        // No early-return on non-empty streams: we must process new PUSI packets
        // to detect PMT version changes (e.g., broadcaster adds an audio track).
        // Duplicate retransmissions of the same version are filtered below after
        // the full section is assembled.

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
            return; // Need more continuation packets
        }

        let data = &self.pmt_buf;
        let end = self.pmt_expected.min(data.len());

        let Some((mut pos, stream_loop_end)) = pmt_stream_loop_bounds(data, end) else {
            self.pmt_buf.clear();
            self.pmt_expected = 0;
            return;
        };

        // Check PMT version_number (ISO 13818-1 table syntax: byte 5 bits 5–1).
        // Skip retransmissions of the same version; reset stream state on change.
        let incoming_version = (data[5] >> 1) & 0x1F;
        if self.pmt_version == incoming_version {
            self.pmt_buf.clear();
            self.pmt_expected = 0;
            return; // Duplicate retransmission — nothing changed
        }
        self.pmt_version = incoming_version;

        // Version changed (or first parse) — rebuild the stream map from the new PMT.
        //
        // For PIDs that survive unchanged into the new PMT we preserve their
        // in-flight PesAccumulator so that partially-assembled frames are not
        // lost mid-decode, which would cause a video glitch/audio pop until the
        // next IDR.  PIDs that disappear are simply dropped.  New PIDs get a
        // fresh accumulator.
        let mut old_pes: std::collections::HashMap<u16, PesAccumulator> =
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
                            continue; // Single video program
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
                // Preserve in-flight PES accumulator for this PID if it existed
                // in the previous PMT — avoids dropping partially-assembled frames
                // when the broadcaster sends a PMT update that only changes metadata
                // (e.g. language descriptor) while keeping the same PIDs.
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

    fn try_build_probe(&mut self, stream_idx: usize, payload: &[u8]) {
        // Stash payload for this stream's probe data
        if self.probe_payloads.len() < self.streams.len() {
            self.probe_payloads.resize(self.streams.len(), None);
        }
        // Keep the first payload that yields complete metadata for this stream.
        // Later non-IDR frames may lack SPS/PPS and would otherwise clobber the
        // probe-ready GOP-start packet while other streams are still pending.
        let replace = match self.probe_payloads[stream_idx].as_deref() {
            None => true,
            Some(existing) => !self.probe_payload_complete(stream_idx, existing),
        };
        if replace {
            self.probe_payloads[stream_idx] = Some(payload.to_vec());
        }

        // Wait until all streams have contributed at least one PES
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
fn find_ts_sync(data: &[u8]) -> usize {
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

fn ts_sync_candidate_is_valid(data: &[u8], candidate: usize) -> bool {
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

fn iso639_language_descriptor(language: Option<&str>) -> Vec<u8> {
    let Some(language) = language else {
        return Vec::new();
    };
    let mut code = [0u8; 3];
    let mut count = 0usize;
    for byte in language.bytes().filter(|b| b.is_ascii_alphabetic()).take(3) {
        code[count] = byte.to_ascii_lowercase();
        count += 1;
    }
    if count != 3 {
        return Vec::new();
    }
    vec![0x0A, 0x04, code[0], code[1], code[2], 0x00]
}

// --- MPEG-TS muxer ---

/// MPEG-TS stream configuration for the muxer.
#[derive(Debug, Clone)]
pub struct MuxStreamConfig {
    pub stream_type: u8,
    pub pid: u16,
    pub media_type: MediaType,
    pub track_index: u32,
    pub sample_rate: u32,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsServiceMetadata {
    pub provider_name: String,
    pub service_name: String,
}

/// Streaming MPEG-TS muxer. Accepts MediaPackets and produces TS bytes.
pub struct TsMuxer {
    streams: Vec<MuxStreamConfig>,
    continuity: Vec<u8>,
    pat_cc: u8,
    pmt_cc: u8,
    sdt_cc: u8,
    pmt_pid: u16,
    pcr_pid: u16,
    last_pat_pmt_dts: Option<i64>,
    last_dts_90k: Vec<i64>,
    service_metadata: TsServiceMetadata,
    output: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsSegmentView {
    Video,
    Audio(u32),
}

impl TsMuxer {
    /// Create a new muxer from stream metadata.
    ///
    /// `flv_payloads`: if true, payloads have FLV wrappers that need stripping.
    pub fn new(video: Option<&VideoMeta>, audio_tracks: &[AudioMeta]) -> Self {
        Self::new_with_metadata(video, audio_tracks, TsServiceMetadata::disabled())
    }

    pub fn new_with_metadata(
        video: Option<&VideoMeta>,
        audio_tracks: &[AudioMeta],
        service_metadata: TsServiceMetadata,
    ) -> Self {
        let mut streams = Vec::new();
        let mut pid = 0x100u16;

        if let Some(v) = video {
            let stream_type = match v.codec.as_str() {
                "h264" => STREAM_TYPE_H264,
                "hevc" => STREAM_TYPE_H265,
                _ => STREAM_TYPE_H264,
            };
            streams.push(MuxStreamConfig {
                stream_type,
                pid,
                media_type: MediaType::Video,
                track_index: 0,
                sample_rate: 0,
                language: None,
            });
            pid += 1;
        }

        for a in audio_tracks {
            let stream_type = match a.codec.as_str() {
                "aac" => STREAM_TYPE_AAC_ADTS,
                _ => STREAM_TYPE_AAC_ADTS,
            };
            streams.push(MuxStreamConfig {
                stream_type,
                pid,
                media_type: MediaType::Audio,
                track_index: a.track_index,
                sample_rate: a.sample_rate,
                language: a.language.clone(),
            });
            pid += 1;
        }

        let stream_count = streams.len();
        let pcr_pid = streams.first().map_or(0x100, |s| s.pid);
        let continuity = vec![0u8; stream_count];

        Self {
            streams,
            continuity,
            pat_cc: 0,
            pmt_cc: 0,
            sdt_cc: 0,
            pmt_pid: 0x1000,
            pcr_pid,
            last_pat_pmt_dts: None,
            last_dts_90k: vec![i64::MIN; stream_count],
            service_metadata,
            output: Vec::with_capacity(TS_PACKET_SIZE * 8),
        }
    }

    /// Resolve a packet's `(media_type, track_index)` into this muxer's stream
    /// index once, so a hot loop can call
    /// [`mux_packet_by_stream_idx`](Self::mux_packet_by_stream_idx) on every
    /// subsequent packet from that track instead of re-scanning `streams`.
    pub fn stream_index(&self, media_type: MediaType, track_index: u32) -> Option<usize> {
        self.streams
            .iter()
            .position(|s| s.media_type == media_type && s.track_index == track_index)
    }

    /// Mux a MediaPacket into MPEG-TS bytes. Returns the produced bytes.
    ///
    /// `payload` should be the raw codec payload (FLV headers already stripped if needed).
    pub fn mux_packet(
        &mut self,
        media_type: MediaType,
        track_index: u32,
        pts_ms: i64,
        dts_ms: i64,
        is_keyframe: bool,
        payload: &[u8],
    ) -> &[u8] {
        let Some(stream_idx) = self.stream_index(media_type, track_index) else {
            self.output.clear();
            return &self.output;
        };
        self.mux_packet_at(stream_idx, media_type, pts_ms, dts_ms, is_keyframe, payload)
    }

    /// Like [`mux_packet`](Self::mux_packet), but takes an already-resolved
    /// stream index instead of re-scanning `streams` by `(media_type,
    /// track_index)` on every call. Callers that mux many packets per track
    /// (recording/HLS/transcoder stdin feeders) should resolve the index once
    /// via [`stream_index`](Self::stream_index) and reuse it here.
    ///
    /// `stream_idx` must identify a stream whose media type matches
    /// `media_type`; a stale or out-of-range index returns an empty slice,
    /// same as a lookup miss in [`mux_packet`](Self::mux_packet).
    pub fn mux_packet_by_stream_idx(
        &mut self,
        stream_idx: usize,
        media_type: MediaType,
        pts_ms: i64,
        dts_ms: i64,
        is_keyframe: bool,
        payload: &[u8],
    ) -> &[u8] {
        if self.streams.get(stream_idx).map(|s| s.media_type) != Some(media_type) {
            self.output.clear();
            return &self.output;
        }
        self.mux_packet_at(stream_idx, media_type, pts_ms, dts_ms, is_keyframe, payload)
    }

    fn mux_packet_at(
        &mut self,
        stream_idx: usize,
        media_type: MediaType,
        pts_ms: i64,
        dts_ms: i64,
        is_keyframe: bool,
        payload: &[u8],
    ) -> &[u8] {
        self.output.clear();

        if payload.is_empty() {
            return &self.output;
        }

        let pid = self.streams[stream_idx].pid;
        let mut pts_90k = ms_to_ts(pts_ms);
        let mut dts_90k = ms_to_ts(dts_ms);
        let pts_offset_90k = (pts_90k - dts_90k).max(0);
        if let Some(prev) = self.last_dts_90k.get(stream_idx).copied()
            && dts_90k <= prev
        {
            dts_90k = prev + 1;
            pts_90k = dts_90k + pts_offset_90k;
        }
        if pts_90k < dts_90k {
            pts_90k = dts_90k;
        }
        let packet_span_end_90k = self.packet_span_end_90k(stream_idx, dts_90k, payload);
        if let Some(slot) = self.last_dts_90k.get_mut(stream_idx) {
            *slot = packet_span_end_90k;
        }

        // Insert PAT/PMT before keyframes or every ~500ms
        let should_insert_tables = match self.last_pat_pmt_dts {
            None => true,
            Some(last) => is_keyframe || (dts_ms - last).abs() >= 500,
        };

        if should_insert_tables {
            self.write_pat();
            self.write_pmt();
            if !self.service_metadata.provider_name.is_empty()
                || !self.service_metadata.service_name.is_empty()
            {
                self.write_sdt();
            }
            self.last_pat_pmt_dts = Some(dts_ms);
        }

        // Build PES header on the stack — no allocation, no payload copy.
        // The logical PES is pes_hdr[..hdr_len] ++ payload.
        let pts_differs = pts_90k != dts_90k;
        let mut pes_hdr = [0u8; 19];
        pes_hdr[0..3].copy_from_slice(&PES_START_CODE);
        pes_hdr[3] = match media_type {
            MediaType::Video => 0xE0,
            MediaType::Audio => 0xC0,
        };
        let hdr_len: usize = if pts_differs { 19 } else { 14 };
        let pes_data_len = hdr_len - 6 + payload.len();
        if media_type == MediaType::Audio && pes_data_len <= 0xFFFF {
            pes_hdr[4] = (pes_data_len >> 8) as u8;
            pes_hdr[5] = pes_data_len as u8;
        }
        pes_hdr[6] = 0x80;
        pes_hdr[7] = if pts_differs { 0xC0 } else { 0x80 };
        pes_hdr[8] = if pts_differs { 10 } else { 5 };
        write_timestamp_buf(
            &mut pes_hdr[9..14],
            pts_90k,
            if pts_differs { 0x03 } else { 0x02 },
        );
        if pts_differs {
            write_timestamp_buf(&mut pes_hdr[14..19], dts_90k, 0x01);
        }

        let total_pes = hdr_len + payload.len();
        let ts_count = total_pes.div_ceil(184); // upper bound
        self.output
            .reserve(ts_count * TS_PACKET_SIZE + 2 * TS_PACKET_SIZE);

        // Packetize: walk two logical slices (pes_hdr, payload) without copying
        // them into a contiguous PES buffer.
        let mut pes_offset = 0usize;
        let mut first = true;

        while pes_offset < total_pes {
            let base = self.output.len();
            self.output.resize(base + TS_PACKET_SIZE, 0xFF);
            let ts = &mut self.output[base..base + TS_PACKET_SIZE];

            ts[0] = TS_SYNC_BYTE;
            let pusi_bit: u8 = if first { 0x40 } else { 0x00 };
            ts[1] = pusi_bit | ((pid >> 8) as u8 & 0x1F);
            ts[2] = pid as u8;

            let cc = self.continuity[stream_idx];
            self.continuity[stream_idx] = (cc + 1) & 0x0F;

            let remaining_pes = total_pes - pes_offset;

            let header_end = if first && (is_keyframe || pid == self.pcr_pid) {
                let pcr_bytes = if pid == self.pcr_pid { 6 } else { 0 };
                let af_flags: u8 =
                    if is_keyframe { 0x40 } else { 0x00 } | if pcr_bytes > 0 { 0x10 } else { 0x00 };

                let min_af_len = 1 + pcr_bytes;
                let available = TS_PACKET_SIZE - 4 - 1 - min_af_len;
                let payload_in_packet = remaining_pes.min(available);
                let stuff = available - payload_in_packet;
                let af_len = min_af_len + stuff;

                ts[3] = 0x30 | cc;
                ts[4] = af_len as u8;
                ts[5] = af_flags;

                if pcr_bytes > 0 {
                    write_pcr(&mut ts[6..], dts_90k);
                }

                5 + af_len
            } else {
                let available = TS_PACKET_SIZE - 4;
                let payload_in_packet = remaining_pes.min(available);

                if payload_in_packet < available {
                    let stuff = available - payload_in_packet;
                    if stuff == 1 {
                        ts[3] = 0x30 | cc;
                        ts[4] = 0x00;
                        5
                    } else {
                        ts[3] = 0x30 | cc;
                        ts[4] = (stuff - 1) as u8;
                        if stuff >= 2 {
                            ts[5] = 0x00;
                        }
                        4 + stuff
                    }
                } else {
                    ts[3] = 0x10 | cc;
                    4
                }
            };

            // Copy from the two logical PES slices into this TS packet's
            // payload region, without ever building a contiguous PES buffer.
            let payload_space = TS_PACKET_SIZE - header_end;
            let copy_len = remaining_pes.min(payload_space);
            copy_pes_slices(
                &mut ts[header_end..header_end + copy_len],
                &pes_hdr[..hdr_len],
                payload,
                pes_offset,
                copy_len,
            );
            pes_offset += copy_len;

            first = false;
        }

        &self.output
    }

    fn write_pat(&mut self) {
        let mut ts = [0xFFu8; TS_PACKET_SIZE];
        ts[0] = TS_SYNC_BYTE;
        ts[1] = 0x40; // PUSI, PID=0
        ts[2] = 0x00;
        ts[3] = 0x10 | (self.pat_cc & 0x0F);
        self.pat_cc = (self.pat_cc + 1) & 0x0F;

        ts[4] = 0x00; // pointer field

        // PAT section
        let pat = &mut ts[5..];
        pat[0] = 0x00; // table_id
        // section_length = 9 (5 header + 4 program entry) — no CRC for simplicity
        // Actually, PAT with CRC: section_length includes from tsid to CRC
        // tsid(2) + version(1) + section(1) + last_section(1) + program(4) + crc(4) = 13
        pat[1] = 0xB0;
        pat[2] = 13;
        pat[3] = 0x00; // transport_stream_id
        pat[4] = 0x01;
        pat[5] = 0xC1; // version=0, current
        pat[6] = 0x00; // section_number
        pat[7] = 0x00; // last_section_number
        // Program 1 → PMT PID
        pat[8] = 0x00;
        pat[9] = 0x01; // program_number = 1
        pat[10] = 0xE0 | ((self.pmt_pid >> 8) as u8 & 0x1F);
        pat[11] = self.pmt_pid as u8;

        let crc = crc32_mpeg2(&ts[5..5 + 12]);
        ts[17] = (crc >> 24) as u8;
        ts[18] = (crc >> 16) as u8;
        ts[19] = (crc >> 8) as u8;
        ts[20] = crc as u8;

        self.output.extend_from_slice(&ts);
    }

    fn write_pmt(&mut self) {
        let stream_descriptors: Vec<Vec<u8>> = self
            .streams
            .iter()
            .map(|s| match s.media_type {
                MediaType::Audio => iso639_language_descriptor(s.language.as_deref()),
                MediaType::Video => Vec::new(),
            })
            .collect();
        let entry_size: usize = self
            .streams
            .iter()
            .zip(stream_descriptors.iter())
            .map(|(_, descriptors)| 5 + descriptors.len())
            .sum();
        let section_len = 9 + entry_size + 4; // 9 fixed + entries + CRC
        if section_len > 1021 {
            error!(
                "[mpegts] PMT too large: section_len={} streams={}",
                section_len,
                self.streams.len()
            );
            return;
        }

        let mut section = Vec::with_capacity(3 + section_len);
        section.push(0x02); // table_id
        section.push(0xB0 | ((section_len >> 8) as u8 & 0x0F));
        section.push(section_len as u8);
        section.push(0x00);
        section.push(0x01); // program_number = 1
        section.push(0xC1); // version=0, current
        section.push(0x00);
        section.push(0x00);
        section.push(0xE0 | ((self.pcr_pid >> 8) as u8 & 0x1F));
        section.push(self.pcr_pid as u8);
        section.push(0xF0);
        section.push(0x00); // program_info_length = 0

        for (s, descriptors) in self.streams.iter().zip(stream_descriptors.iter()) {
            section.push(s.stream_type);
            section.push(0xE0 | ((s.pid >> 8) as u8 & 0x1F));
            section.push(s.pid as u8);
            section.push(0xF0 | ((descriptors.len() >> 8) as u8 & 0x0F));
            section.push(descriptors.len() as u8);
            section.extend_from_slice(descriptors);
        }

        let crc = crc32_mpeg2(&section);
        section.push((crc >> 24) as u8);
        section.push((crc >> 16) as u8);
        section.push((crc >> 8) as u8);
        section.push(crc as u8);

        let mut offset = 0usize;
        let mut first = true;
        while offset < section.len() {
            let mut ts = [0xFFu8; TS_PACKET_SIZE];
            ts[0] = TS_SYNC_BYTE;
            ts[1] = ((self.pmt_pid >> 8) as u8 & 0x1F) | if first { 0x40 } else { 0x00 };
            ts[2] = self.pmt_pid as u8;
            ts[3] = 0x10 | (self.pmt_cc & 0x0F);
            self.pmt_cc = (self.pmt_cc + 1) & 0x0F;

            let payload_start = if first {
                ts[4] = 0x00; // pointer field
                5
            } else {
                4
            };
            let capacity = TS_PACKET_SIZE - payload_start;
            let n = capacity.min(section.len() - offset);
            ts[payload_start..payload_start + n].copy_from_slice(&section[offset..offset + n]);
            offset += n;
            first = false;

            self.output.extend_from_slice(&ts);
        }
    }

    fn write_sdt(&mut self) {
        let provider = truncate_utf8_bytes(&self.service_metadata.provider_name, 48);
        let service = truncate_utf8_bytes(&self.service_metadata.service_name, 110);
        let descriptor_len = 3 + provider.len() + service.len();
        let service_loop_len = 5 + 2 + descriptor_len;
        let section_len = 12 + service_loop_len;

        let mut ts = [0xFFu8; TS_PACKET_SIZE];
        ts[0] = TS_SYNC_BYTE;
        ts[1] = 0x40 | ((SDT_PID >> 8) as u8 & 0x1F);
        ts[2] = SDT_PID as u8;
        ts[3] = 0x10 | (self.sdt_cc & 0x0F);
        self.sdt_cc = (self.sdt_cc + 1) & 0x0F;
        ts[4] = 0x00; // pointer field

        let sdt = &mut ts[5..];
        sdt[0] = 0x42; // service_description_section - actual TS
        sdt[1] = 0xF0 | ((section_len >> 8) as u8 & 0x0F);
        sdt[2] = section_len as u8;
        sdt[3] = 0x00; // transport_stream_id
        sdt[4] = 0x01;
        sdt[5] = 0xC1; // version=0, current_next_indicator=1
        sdt[6] = 0x00; // section_number
        sdt[7] = 0x00; // last_section_number
        sdt[8] = 0x00; // original_network_id
        sdt[9] = 0x01;
        sdt[10] = 0xFF; // reserved_future_use

        let mut pos = 11;
        sdt[pos] = 0x00;
        sdt[pos + 1] = 0x01; // service_id = program 1
        sdt[pos + 2] = 0xFC; // reserved + EIT flags = false
        sdt[pos + 3] = 0x80 | ((descriptor_len >> 8) as u8 & 0x0F); // running=4, free_ca=false
        sdt[pos + 4] = descriptor_len as u8;
        pos += 5;

        sdt[pos] = 0x48; // service_descriptor
        sdt[pos + 1] = descriptor_len as u8;
        sdt[pos + 2] = 0x01; // digital television service
        sdt[pos + 3] = provider.len() as u8;
        pos += 4;
        sdt[pos..pos + provider.len()].copy_from_slice(provider.as_bytes());
        pos += provider.len();
        sdt[pos] = service.len() as u8;
        pos += 1;
        sdt[pos..pos + service.len()].copy_from_slice(service.as_bytes());

        let crc_start = 5;
        let crc_end = 5 + 3 + section_len - 4;
        let crc = crc32_mpeg2(&ts[crc_start..crc_end]);
        ts[crc_end] = (crc >> 24) as u8;
        ts[crc_end + 1] = (crc >> 16) as u8;
        ts[crc_end + 2] = (crc >> 8) as u8;
        ts[crc_end + 3] = crc as u8;

        self.output.extend_from_slice(&ts);
    }

    fn packet_span_end_90k(&self, stream_idx: usize, dts_90k: i64, payload: &[u8]) -> i64 {
        let Some(stream) = self.streams.get(stream_idx) else {
            return dts_90k;
        };
        if stream.media_type != MediaType::Audio || stream.sample_rate == 0 {
            return dts_90k;
        }
        let frame_count = crate::media::codec::adts_frame_count(payload);
        if frame_count <= 1 {
            return dts_90k;
        }
        let frame_ticks = (90_000_i64 * 1024_i64) / stream.sample_rate as i64;
        dts_90k + frame_ticks * (frame_count as i64 - 1)
    }
}

pub fn remux_segment_view(
    segment: &[u8],
    video: Option<&VideoMeta>,
    audio_tracks: &[AudioMeta],
    view: TsSegmentView,
) -> Option<Bytes> {
    let (mux_video, mux_audio) = match view {
        TsSegmentView::Video => (video, Vec::new()),
        TsSegmentView::Audio(track_index) => {
            let audio = audio_tracks
                .iter()
                .find(|track| track.track_index == track_index)
                .cloned()?;
            (None, vec![audio])
        }
    };

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(segment);
    demuxer.flush();

    let mut muxer = TsMuxer::new(mux_video, &mux_audio);
    let mut output = Vec::with_capacity(segment.len().min(256 * 1024));
    let mut wrote_media = false;

    for packet in demuxer.drain() {
        let include = match view {
            TsSegmentView::Video => packet.media_type == MediaType::Video,
            TsSegmentView::Audio(track_index) => {
                packet.media_type == MediaType::Audio && packet.track_index == track_index
            }
        };
        if !include {
            continue;
        }

        let data = muxer.mux_packet(
            packet.media_type,
            packet.track_index,
            packet.pts,
            packet.dts,
            packet.is_keyframe,
            &packet.payload,
        );
        if !data.is_empty() {
            wrote_media = true;
            output.extend_from_slice(data);
        }
    }

    wrote_media.then(|| Bytes::from(output))
}

impl Default for TsServiceMetadata {
    fn default() -> Self {
        Self {
            provider_name: "Restream".to_string(),
            service_name: "Restream Recording".to_string(),
        }
    }
}

impl TsServiceMetadata {
    pub fn disabled() -> Self {
        Self {
            provider_name: String::new(),
            service_name: String::new(),
        }
    }
}

fn truncate_utf8_bytes(value: &str, max_bytes: usize) -> String {
    if value.len() <= max_bytes {
        return value.to_string();
    }
    let mut end = max_bytes;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

// --- Timestamp helpers ---

fn parse_timestamp(data: &[u8]) -> i64 {
    let b0 = data[0] as i64;
    let b1 = data[1] as i64;
    let b2 = data[2] as i64;
    let b3 = data[3] as i64;
    let b4 = data[4] as i64;

    ((b0 >> 1) & 0x07) << 30 | (b1 << 22) | ((b2 >> 1) << 15) | (b3 << 7) | (b4 >> 1)
}

#[cfg(test)]
fn write_timestamp(buf: &mut Vec<u8>, ts: i64, marker: u8) {
    buf.push((marker << 4) | (((ts >> 30) as u8) & 0x07) << 1 | 0x01);
    buf.push(((ts >> 22) & 0xFF) as u8);
    buf.push((((ts >> 15) & 0x7F) as u8) << 1 | 0x01);
    buf.push(((ts >> 7) & 0xFF) as u8);
    buf.push((((ts) & 0x7F) as u8) << 1 | 0x01);
}

fn write_timestamp_buf(buf: &mut [u8], ts: i64, marker: u8) {
    buf[0] = (marker << 4) | (((ts >> 30) as u8) & 0x07) << 1 | 0x01;
    buf[1] = ((ts >> 22) & 0xFF) as u8;
    buf[2] = (((ts >> 15) & 0x7F) as u8) << 1 | 0x01;
    buf[3] = ((ts >> 7) & 0xFF) as u8;
    buf[4] = (((ts) & 0x7F) as u8) << 1 | 0x01;
}

fn copy_pes_slices(dst: &mut [u8], hdr: &[u8], payload: &[u8], offset: usize, len: usize) {
    let hdr_len = hdr.len();
    let mut written = 0;
    if offset < hdr_len {
        let from_hdr = (hdr_len - offset).min(len);
        dst[..from_hdr].copy_from_slice(&hdr[offset..offset + from_hdr]);
        written = from_hdr;
    }
    if written < len {
        let payload_offset = offset.saturating_sub(hdr_len);
        let remaining = len - written;
        dst[written..written + remaining]
            .copy_from_slice(&payload[payload_offset..payload_offset + remaining]);
    }
}

fn write_pcr(buf: &mut [u8], ts_90k: i64) {
    let pcr_base = ts_90k.max(0) as u64;
    let pcr_ext: u16 = 0;
    buf[0] = (pcr_base >> 25) as u8;
    buf[1] = (pcr_base >> 17) as u8;
    buf[2] = (pcr_base >> 9) as u8;
    buf[3] = (pcr_base >> 1) as u8;
    buf[4] = ((pcr_base & 1) << 7) as u8 | 0x7E | ((pcr_ext >> 8) as u8 & 0x01);
    buf[5] = pcr_ext as u8;
}

fn ts_to_ms(ts_90k: i64) -> i64 {
    // Exact integer arithmetic: 90kHz → ms is ts / 90 (= ts * 1000 / 90000).
    // Using f64 would introduce up to ~45 ms of accumulated drift over a 24-hour
    // stream because f64 has only 53-bit mantissa precision and ts_90k grows to
    // ~7.8e12 for a day-long stream, losing sub-90-tick resolution.
    ts_90k / 90
}

fn ms_to_ts(ms: i64) -> i64 {
    ms * 90
}

// --- CRC-32/MPEG-2 ---

// Benchmarked crc-fast (PCLMULQDQ) vs table-driven: at our workload sizes
// (12-22 bytes, once per ~100ms), crc-fast is 2.5× slower due to SIMD dispatch
// overhead. Table-driven is zero-dependency, faster at these sizes, and more than
// sufficient for production. See benches/simd_alternatives.rs.
//
// All operations used in the table computation (`<<`, `^`, conditionals) are
// valid in `const fn`, so the table is computed at compile time and placed in
// the binary's read-only data segment — no OnceLock, no runtime init, no atomic.
const CRC32_TABLE: [u32; 256] = {
    let mut table = [0u32; 256];
    let mut i = 0usize;
    while i < 256 {
        let mut crc = (i as u32) << 24;
        let mut j = 0u32;
        while j < 8 {
            if crc & 0x8000_0000 != 0 {
                crc = (crc << 1) ^ 0x04C1_1DB7;
            } else {
                crc <<= 1;
            }
            j += 1;
        }
        table[i] = crc;
        i += 1;
    }
    table
};

fn crc32_mpeg2(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        let idx = (((crc >> 24) ^ (byte as u32)) & 0xFF) as usize;
        crc = (crc << 8) ^ CRC32_TABLE[idx];
    }
    crc
}

#[cfg(test)]
#[path = "mpegts_tests.rs"]
mod tests;
