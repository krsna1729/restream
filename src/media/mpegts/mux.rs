use tracing::error;

use super::wire::{
    PES_START_CODE, SDT_PID, TS_PACKET_SIZE, TS_SYNC_BYTE, copy_pes_slices, crc32_mpeg2, ms_to_ts,
    write_pcr, write_timestamp_buf,
};
use crate::media::metadata::{AudioMeta, VideoMeta};
use crate::media::packet::MediaType;

const STREAM_TYPE_H264: u8 = 0x1B;
const STREAM_TYPE_H265: u8 = 0x24;
const STREAM_TYPE_AAC_ADTS: u8 = 0x0F;

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
    pub(super) last_dts_90k: Vec<i64>,
    service_metadata: TsServiceMetadata,
    output: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TsSegmentView {
    Video,
    Audio(u32),
}

/// Per-packet timing/keyframe metadata for the `mux_packet*` family, bundled
/// so those functions stay under clippy's argument-count lint. `Copy` and
/// stack-sized (three scalars) so passing it costs nothing extra on the
/// mux hot path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PacketMeta {
    pub pts_ms: i64,
    pub dts_ms: i64,
    pub is_keyframe: bool,
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
        // Borrow the internal scratch buffer out so `mux_packet_at` can append
        // into it (a `&mut self` method cannot also hold `&mut self.output`).
        // `mem::take` is O(1) — it swaps in an empty Vec and keeps the
        // allocation on restore, so the buffer is reused across calls.
        let mut out = std::mem::take(&mut self.output);
        out.clear();
        if let Some(stream_idx) = self.stream_index(media_type, track_index) {
            self.mux_packet_at(
                stream_idx,
                media_type,
                PacketMeta {
                    pts_ms,
                    dts_ms,
                    is_keyframe,
                },
                payload,
                &mut out,
            );
        }
        self.output = out;
        &self.output
    }

    /// Mux a MediaPacket directly into a caller-owned accumulator, appending
    /// the produced TS packets to `out` with no intermediate copy.
    ///
    /// This is the zero-copy hot-path entry point: burst feeders accumulate a
    /// whole `pull_burst` into one `out` buffer and freeze it once, instead of
    /// muxing into the muxer's internal scratch and copying each packet's bytes
    /// into the accumulator (the `mux_packet` + `extend_from_slice` shape). The
    /// number of TS bytes appended is `out.len()` after the call minus before.
    pub fn mux_packet_into(
        &mut self,
        media_type: MediaType,
        track_index: u32,
        meta: PacketMeta,
        payload: &[u8],
        out: &mut Vec<u8>,
    ) {
        if let Some(stream_idx) = self.stream_index(media_type, track_index) {
            self.mux_packet_at(stream_idx, media_type, meta, payload, out);
        }
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
        let mut out = std::mem::take(&mut self.output);
        out.clear();
        if self.streams.get(stream_idx).map(|s| s.media_type) == Some(media_type) {
            self.mux_packet_at(
                stream_idx,
                media_type,
                PacketMeta {
                    pts_ms,
                    dts_ms,
                    is_keyframe,
                },
                payload,
                &mut out,
            );
        }
        self.output = out;
        &self.output
    }

    /// Like [`mux_packet_into`](Self::mux_packet_into), but takes an
    /// already-resolved stream index (see
    /// [`mux_packet_by_stream_idx`](Self::mux_packet_by_stream_idx)).
    pub fn mux_packet_by_stream_idx_into(
        &mut self,
        stream_idx: usize,
        media_type: MediaType,
        meta: PacketMeta,
        payload: &[u8],
        out: &mut Vec<u8>,
    ) {
        if self.streams.get(stream_idx).map(|s| s.media_type) == Some(media_type) {
            self.mux_packet_at(stream_idx, media_type, meta, payload, out);
        }
    }

    fn mux_packet_at(
        &mut self,
        stream_idx: usize,
        media_type: MediaType,
        meta: PacketMeta,
        payload: &[u8],
        out: &mut Vec<u8>,
    ) {
        let PacketMeta {
            pts_ms,
            dts_ms,
            is_keyframe,
        } = meta;
        if payload.is_empty() {
            return;
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

        let should_insert_tables = match self.last_pat_pmt_dts {
            None => true,
            Some(last) => is_keyframe || (dts_ms - last).abs() >= 500,
        };

        if should_insert_tables {
            self.write_pat(out);
            self.write_pmt(out);
            if !self.service_metadata.provider_name.is_empty()
                || !self.service_metadata.service_name.is_empty()
            {
                self.write_sdt(out);
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
        let ts_count = total_pes.div_ceil(184);
        out.reserve(ts_count * TS_PACKET_SIZE + 2 * TS_PACKET_SIZE);

        let mut pes_offset = 0usize;
        let mut first = true;

        while pes_offset < total_pes {
            let base = out.len();
            out.resize(base + TS_PACKET_SIZE, 0xFF);
            let ts = &mut out[base..base + TS_PACKET_SIZE];

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
    }

    fn write_pat(&mut self, out: &mut Vec<u8>) {
        let mut ts = [0xFFu8; TS_PACKET_SIZE];
        ts[0] = TS_SYNC_BYTE;
        ts[1] = 0x40;
        ts[2] = 0x00;
        ts[3] = 0x10 | (self.pat_cc & 0x0F);
        self.pat_cc = (self.pat_cc + 1) & 0x0F;
        ts[4] = 0x00;

        let pat = &mut ts[5..];
        pat[0] = 0x00;
        pat[1] = 0xB0;
        pat[2] = 13;
        pat[3] = 0x00;
        pat[4] = 0x01;
        pat[5] = 0xC1;
        pat[6] = 0x00;
        pat[7] = 0x00;
        pat[8] = 0x00;
        pat[9] = 0x01;
        pat[10] = 0xE0 | ((self.pmt_pid >> 8) as u8 & 0x1F);
        pat[11] = self.pmt_pid as u8;

        let crc = crc32_mpeg2(&ts[5..5 + 12]);
        ts[17] = (crc >> 24) as u8;
        ts[18] = (crc >> 16) as u8;
        ts[19] = (crc >> 8) as u8;
        ts[20] = crc as u8;
        out.extend_from_slice(&ts);
    }

    fn write_pmt(&mut self, out: &mut Vec<u8>) {
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
        let section_len = 9 + entry_size + 4;
        if section_len > 1021 {
            error!(
                "[mpegts] PMT too large: section_len={} streams={}",
                section_len,
                self.streams.len()
            );
            return;
        }

        let mut section = Vec::with_capacity(3 + section_len);
        section.push(0x02);
        section.push(0xB0 | ((section_len >> 8) as u8 & 0x0F));
        section.push(section_len as u8);
        section.push(0x00);
        section.push(0x01);
        section.push(0xC1);
        section.push(0x00);
        section.push(0x00);
        section.push(0xE0 | ((self.pcr_pid >> 8) as u8 & 0x1F));
        section.push(self.pcr_pid as u8);
        section.push(0xF0);
        section.push(0x00);

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
                ts[4] = 0x00;
                5
            } else {
                4
            };
            let capacity = TS_PACKET_SIZE - payload_start;
            let n = capacity.min(section.len() - offset);
            ts[payload_start..payload_start + n].copy_from_slice(&section[offset..offset + n]);
            offset += n;
            first = false;
            out.extend_from_slice(&ts);
        }
    }

    fn write_sdt(&mut self, out: &mut Vec<u8>) {
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
        ts[4] = 0x00;

        let sdt = &mut ts[5..];
        sdt[0] = 0x42;
        sdt[1] = 0xF0 | ((section_len >> 8) as u8 & 0x0F);
        sdt[2] = section_len as u8;
        sdt[3] = 0x00;
        sdt[4] = 0x01;
        sdt[5] = 0xC1;
        sdt[6] = 0x00;
        sdt[7] = 0x00;
        sdt[8] = 0x00;
        sdt[9] = 0x01;
        sdt[10] = 0xFF;

        let mut pos = 11;
        sdt[pos] = 0x00;
        sdt[pos + 1] = 0x01;
        sdt[pos + 2] = 0xFC;
        sdt[pos + 3] = 0x80 | ((descriptor_len >> 8) as u8 & 0x0F);
        sdt[pos + 4] = descriptor_len as u8;
        pos += 5;

        sdt[pos] = 0x48;
        sdt[pos + 1] = descriptor_len as u8;
        sdt[pos + 2] = 0x01;
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
        out.extend_from_slice(&ts);
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
