use super::mpegts_probe::*;
use super::*;
use proptest::prelude::*;
use std::collections::{BTreeMap, BTreeSet, VecDeque};

// Shared packet builders and fixture probes used by multiple behavior slices.
fn h264_stream_info(pid: u16) -> StreamInfo {
    StreamInfo {
        pid,
        kind: StreamKind::H264,
        track_index: 0,
        language: None,
        title: None,
        continuity: CC_UNSET,
        pes: PesAccumulator::new(),
    }
}

fn aac_adts_stream_info(pid: u16, track_index: u32) -> StreamInfo {
    StreamInfo {
        pid,
        kind: StreamKind::AacAdts,
        track_index,
        language: None,
        title: None,
        continuity: CC_UNSET,
        pes: PesAccumulator::new(),
    }
}

fn ts_header_bytes(pid: u16, pusi: bool, afc: u8, cc: u8) -> [u8; 4] {
    [
        TS_SYNC_BYTE,
        (if pusi { 0x40 } else { 0x00 }) | ((pid >> 8) as u8 & 0x1F),
        (pid & 0xFF) as u8,
        (afc << 4) | (cc & 0x0F),
    ]
}

/// Builds a well-formed TS packet: an adaptation field sized exactly to
/// `af_body.len()` (if `afc` calls for one), followed by `payload` truncated
/// to whatever room remains and 0xFF-stuffed past that.
fn build_ts_packet(
    pid: u16,
    pusi: bool,
    afc: u8,
    cc: u8,
    af_body: &[u8],
    payload: &[u8],
) -> [u8; TS_PACKET_SIZE] {
    let mut pkt = [0xFFu8; TS_PACKET_SIZE];
    pkt[0..4].copy_from_slice(&ts_header_bytes(pid, pusi, afc, cc));
    let mut offset = 4;
    if afc == 0x02 || afc == 0x03 {
        pkt[offset] = af_body.len() as u8;
        offset += 1;
        pkt[offset..offset + af_body.len()].copy_from_slice(af_body);
        offset += af_body.len();
    }
    if afc == 0x01 || afc == 0x03 {
        let n = payload.len().min(TS_PACKET_SIZE - offset);
        pkt[offset..offset + n].copy_from_slice(&payload[..n]);
    }
    pkt
}

fn install_single_h264_stream(demuxer: &mut TsDemuxer, pid: u16) {
    demuxer.streams = vec![h264_stream_info(pid)];
    demuxer.pid_to_stream[pid as usize] = 0;
}

/// A single-TS-packet, PTS-only video PES with an explicit (bounded)
/// `pes_packet_len`, so `es_payload` demuxes exactly regardless of the
/// 0xFF stuffing that fills the rest of the fixed-size 184-byte TS payload
/// region. `payload_unit_start` carries a complete PES header (9-byte
/// mandatory + 5-byte PTS) plus `es_payload`.
fn valid_video_pes_packet(
    pid: u16,
    cc: u8,
    pts_90k: i64,
    es_payload: &[u8],
) -> [u8; TS_PACKET_SIZE] {
    const PES_HEADER_LEN: u8 = 5; // PTS-only optional header
    let pes_packet_len = 3 + PES_HEADER_LEN as u16 + es_payload.len() as u16;
    let mut pes = vec![0x00, 0x00, 0x01, 0xE0];
    pes.extend_from_slice(&pes_packet_len.to_be_bytes());
    pes.push(0x80);
    pes.push(0x80);
    pes.push(PES_HEADER_LEN);
    write_timestamp(&mut pes, pts_90k, 0x02);
    pes.extend_from_slice(es_payload);
    build_ts_packet(pid, true, 0x01, cc, &[], &pes)
}

/// Like [`valid_video_pes_packet`], but leaves `pes_packet_len` at 0
/// (unbounded), the standard MPEG-TS encoding for a video PES whose length
/// isn't known up front — completion then depends solely on the next
/// `payload_unit_start` packet.
fn unbounded_video_pes_start_packet(
    pid: u16,
    cc: u8,
    pts_90k: i64,
    es_payload: &[u8],
) -> [u8; TS_PACKET_SIZE] {
    let mut pes = vec![0x00, 0x00, 0x01, 0xE0, 0x00, 0x00, 0x80, 0x80, 0x05];
    write_timestamp(&mut pes, pts_90k, 0x02);
    pes.extend_from_slice(es_payload);
    build_ts_packet(pid, true, 0x01, cc, &[], &pes)
}

fn first_probe_ready_payloads() -> (Vec<u8>, Vec<u8>) {
    let fixture =
        crate::test_fixtures::canonical_h264_ts_fixture().unwrap_or_else(|e| panic!("{e}"));
    let ts = std::fs::read(&fixture)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", fixture.display()));
    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&ts);
    demuxer.flush();
    let packets = demuxer.drain();
    let video = packets
        .iter()
        .find(|packet| {
            packet.media_type == MediaType::Video
                && video_meta_complete(
                    StreamKind::H264,
                    &probe_video(StreamKind::H264, 0x100, None, None, packet.payload.as_ref()),
                )
        })
        .map(|packet| packet.payload.to_vec())
        .expect("fixture should contain a probe-ready H.264 access unit");
    let audio = packets
        .iter()
        .find(|packet| {
            packet.media_type == MediaType::Audio
                && audio_meta_complete(
                    StreamKind::AacAdts,
                    &probe_audio(
                        StreamKind::AacAdts,
                        0,
                        0x101,
                        None,
                        None,
                        packet.payload.as_ref(),
                    ),
                )
        })
        .map(|packet| packet.payload.to_vec())
        .expect("fixture should contain a probe-ready AAC access unit");
    (video, audio)
}

include!("mpegts_tests/timestamps.rs");
include!("mpegts_tests/demux.rs");
include!("mpegts_tests/mux.rs");
include!("mpegts_tests/round_trip.rs");
include!("mpegts_tests/probe.rs");
include!("mpegts_tests/tables.rs");

#[path = "mpegts_tests/nal_scanning.rs"]
mod nal_scanning;

#[path = "mpegts_tests/tables_and_sync.rs"]
mod tables_and_sync;
