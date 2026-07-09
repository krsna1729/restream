use super::*;

fn make_pat_ts_pkt() -> Vec<u8> {
    let mut pkt = vec![0xFFu8; 188];
    pkt[0] = 0x47;
    pkt[1] = 0x40; // PUSI, PID=0x0000
    pkt[2] = 0x00;
    pkt[3] = 0x10; // payload-only, CC=0
    pkt[4] = 0x00; // pointer_field
    // PAT section
    pkt[5] = 0x00; // table_id = PAT
    pkt[6] = 0xB0;
    pkt[7] = 13; // section_length
    pkt[8] = 0x00;
    pkt[9] = 0x01; // TSID = 1
    pkt[10] = 0xC1; // version=0, current_next=1
    pkt[11] = 0x00; // section_number
    pkt[12] = 0x00; // last_section_number
    // Program 1 -> PMT PID 0x1000
    pkt[13] = 0x00;
    pkt[14] = 0x01;
    pkt[15] = 0xF0; // 0xE0 | (0x1000 >> 8) = 0xF0
    pkt[16] = 0x00; // 0x1000 & 0xFF
    let crc = crc32_mpeg2(&pkt[5..17]);
    pkt[17] = (crc >> 24) as u8;
    pkt[18] = (crc >> 16) as u8;
    pkt[19] = (crc >> 8) as u8;
    pkt[20] = crc as u8;
    pkt
}

/// Build a 188-byte TS PMT packet at PID 0x1000 with the given version and
/// stream list. Each stream is `(stream_type, elementary_pid)`.
fn make_pmt_ts_pkt(version: u8, streams: &[(u8, u16)]) -> Vec<u8> {
    let mut pkt = vec![0xFFu8; 188];
    pkt[0] = 0x47;
    pkt[1] = 0x50; // PUSI, PID high (0x1000)
    pkt[2] = 0x00; // PID low
    pkt[3] = 0x10; // payload-only, CC=0
    pkt[4] = 0x00; // pointer_field
    // PMT section
    pkt[5] = 0x02; // table_id = PMT
    let section_len = 9 + (5 * streams.len()) + 4;
    pkt[6] = 0xB0;
    pkt[7] = section_len as u8;
    pkt[8] = 0x00; // program_number high
    pkt[9] = 0x01; // program_number low
    // version_number in bits 5..1, current_next_indicator in bit 0
    pkt[10] = 0xC0 | ((version & 0x1F) << 1) | 0x01;
    pkt[11] = 0x00; // section_number
    pkt[12] = 0x00; // last_section_number
    pkt[13] = 0xE1; // PCR_PID = 0x100
    pkt[14] = 0x00;
    pkt[15] = 0xF0; // program_info_length high
    pkt[16] = 0x00; // program_info_length low (= 0)
    let mut pos = 17usize;
    for &(stream_type, pid) in streams {
        pkt[pos] = stream_type;
        pkt[pos + 1] = 0xE0 | ((pid >> 8) as u8 & 0x1F);
        pkt[pos + 2] = (pid & 0xFF) as u8;
        pkt[pos + 3] = 0xF0; // ES_info_length = 0
        pkt[pos + 4] = 0x00;
        pos += 5;
    }
    let crc = crc32_mpeg2(&pkt[5..pos]);
    pkt[pos] = (crc >> 24) as u8;
    pkt[pos + 1] = (crc >> 16) as u8;
    pkt[pos + 2] = (crc >> 8) as u8;
    pkt[pos + 3] = crc as u8;
    pkt
}

// --- Regression: issue #3 — PMT version tracking ---

#[test]
fn pmt_retransmission_same_version_is_idempotent() {
    // Regression: the old guard `if !self.streams.is_empty() && pmt_expected == 0 { return }`
    // was replaced by explicit version tracking. A retransmission of the same
    // PMT version must NOT rebuild the stream map (no phantom duplicates).
    let mut data = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    let streams = [(0x1B, 0x100u16), (0x0F, 0x101u16)]; // H.264 + AAC
    data.extend_from_slice(&make_pmt_ts_pkt(0, &streams));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&data);

    assert_eq!(
        demuxer.streams.len(),
        2,
        "initial PMT must produce 2 streams"
    );
    assert_eq!(demuxer.pmt_version, 0);

    // Feed the same PMT version again (broadcaster retransmits every ~100ms)
    let retransmit = make_pmt_ts_pkt(0, &streams);
    demuxer.feed(&retransmit);

    assert_eq!(
        demuxer.streams.len(),
        2,
        "retransmitting the same PMT version must not rebuild the stream map"
    );
    assert_eq!(demuxer.pmt_version, 0);
}

#[test]
fn pmt_version_change_rebuilds_stream_map() {
    // Regression: the old code returned early on non-empty streams, silently
    // dropping genuine PMT version changes (e.g., broadcaster adds audio mid-stream).
    let mut data = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    // Version 0: video only
    data.extend_from_slice(&make_pmt_ts_pkt(0, &[(0x1B, 0x100u16)]));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&data);

    assert_eq!(demuxer.streams.len(), 1, "PMT v0 must have 1 stream");
    assert_eq!(demuxer.pmt_version, 0);

    // Version 1: broadcaster added an audio track
    let v1_pkt = make_pmt_ts_pkt(1, &[(0x1B, 0x100u16), (0x0F, 0x101u16)]);
    demuxer.feed(&v1_pkt);

    assert_eq!(
        demuxer.streams.len(),
        2,
        "PMT version change must rebuild stream map so new audio PID is parsed"
    );
    assert_eq!(demuxer.pmt_version, 1);
    assert_eq!(demuxer.streams[1].kind, StreamKind::AacAdts);
}

// --- Regression: issue #12 — PCR negative guard ---

#[test]
fn write_pcr_clamps_negative_ts_to_zero() {
    // Regression: before the .max(0) fix, a negative DTS reaching write_pcr
    // would silently cast to a large u64, producing a nonsensical PCR value
    // that makes decoders stall or seek unexpectedly.
    let mut buf_neg = [0u8; 6];
    let mut buf_zero = [0u8; 6];
    write_pcr(&mut buf_neg, -1_000_000);
    write_pcr(&mut buf_zero, 0);
    assert_eq!(
        buf_neg, buf_zero,
        "negative ts_90k must clamp to 0, not wrap to a huge u64 PCR"
    );

    // Also verify an extreme negative value does not panic.
    let mut buf_min = [0u8; 6];
    write_pcr(&mut buf_min, i64::MIN);
    assert_eq!(buf_min, buf_zero);
}

// --- Regression: issue #6 (Round 3) — TsDemuxer remainder length cap ---
// Before the MAX_REMAINDER guard, feeding a stream of single-byte 0x47
// chunks would cause remainder to grow by 1 byte on every call — O(n)
// memory growth per byte of input, i.e. O(n²) overall processing cost
// for a corrupt / adversarial stream.  After the fix the remainder must
// never exceed TS_PACKET_SIZE - 1 = 187 bytes.
#[test]
fn feed_remainder_capped_on_corrupt_stream() {
    let mut dem = TsDemuxer::new();
    // Feed 500 isolated 0x47 bytes (each looks like a TS sync byte but is
    // never followed by 187 more bytes, so no packet can complete).
    for _ in 0..500 {
        dem.feed(&[0x47]);
    }
    assert!(
        dem.remainder.len() < TS_PACKET_SIZE,
        "remainder must be capped at TS_PACKET_SIZE-1 ({}) but was {}",
        TS_PACKET_SIZE - 1,
        dem.remainder.len()
    );
}

// --- Regression: issue #5 (Round 4) — PMT version rebuild preserves in-flight PES ---
// Before the fix, a PMT version change discarded ALL StreamInfo (including
// PesAccumulator buffers).  A partially-assembled video frame would be lost,
// producing a glitch until the next IDR.  After the fix, PES buffers for PIDs
// that survive into the new PMT are carried over.
#[test]
fn pmt_version_change_preserves_pes_for_unchanged_pid() {
    // Build a minimal TS PES packet for PID 0x100 that starts a new PES unit
    // (PUSI=1) but does NOT complete it (no second packet with the next PES
    // start, so the frame stays in the accumulator).
    fn make_pes_ts_pkt(pid: u16, pusi: bool, payload: &[u8]) -> Vec<u8> {
        let mut pkt = vec![0xFFu8; 188];
        pkt[0] = 0x47;
        pkt[1] = if pusi { 0x40 } else { 0x00 } | ((pid >> 8) as u8 & 0x1F);
        pkt[2] = (pid & 0xFF) as u8;
        pkt[3] = 0x10; // payload_unit_start = pusi flag handled above; CC=0
        // When PUSI, prepend a minimal PES header: start code + stream_id +
        // length(0=unbounded) + flags + header_data_length(0)
        let pes_header: &[u8] = if pusi {
            &[0x00, 0x00, 0x01, 0xE0, 0x00, 0x00, 0x80, 0x00, 0x00]
        } else {
            &[]
        };
        let data_offset = 4usize;
        let total = pes_header.len() + payload.len();
        pkt[data_offset..data_offset + total.min(184)]
            .iter_mut()
            .zip(pes_header.iter().chain(payload.iter()))
            .for_each(|(d, &s)| *d = s);
        pkt
    }

    let mut data: Vec<u8> = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    // PMT v0: H.264 video at PID 0x100
    data.extend_from_slice(&make_pmt_ts_pkt(0, &[(0x1B, 0x100u16)]));
    // PES start for PID 0x100 — frame not yet complete
    data.extend_from_slice(&make_pes_ts_pkt(0x100, true, &[0xDE, 0xAD]));

    let mut demuxer = TsDemuxer::new();
    demuxer.feed(&data);

    // Verify the PES buffer has data
    assert_eq!(demuxer.streams.len(), 1);
    let buf_before = demuxer.streams[0].pes.buf.clone();
    assert!(
        !buf_before.is_empty(),
        "PES buf must have partial frame data"
    );

    // Now the broadcaster sends a PMT version change for the same PID
    // (e.g., only the language descriptor changed).
    let v1_pkt = make_pmt_ts_pkt(1, &[(0x1B, 0x100u16)]);
    demuxer.feed(&v1_pkt);

    assert_eq!(
        demuxer.streams.len(),
        1,
        "stream map should still have 1 stream"
    );
    assert_eq!(demuxer.pmt_version, 1);
    assert_eq!(
        demuxer.streams[0].pes.buf, buf_before,
        "in-flight PES buffer must be preserved after PMT version change for same PID"
    );
}

#[test]
fn crc32_empty_data() {
    assert_eq!(crc32_mpeg2(&[]), 0xFFFF_FFFF);
}

#[test]
fn crc32_known_vector() {
    // CRC-32/MPEG-2 of "123456789" (classic check value)
    let data = b"123456789";
    assert_eq!(crc32_mpeg2(data), 0x0376_E6E7);
}

#[test]
fn crc32_idempotent_across_calls() {
    let data = b"hello world";
    let a = crc32_mpeg2(data);
    let b = crc32_mpeg2(data);
    assert_eq!(a, b);
}

#[test]
fn write_pcr_zero_ts() {
    let mut buf = [0xFFu8; 6];
    write_pcr(&mut buf, 0);
    // PCR at zero: base=0, extension=0, with the PCR marker bits set
    assert_eq!(buf[0], 0x00);
    assert_eq!(buf[1], 0x00);
    assert_eq!(buf[2], 0x00);
    assert_eq!(buf[3], 0x00);
}

// --- const CRC32_TABLE correctness ---

#[test]
fn crc32_table_is_compile_time_const() {
    // The const table and a freshly-computed runtime table must agree for all 256 entries.
    let runtime_table: [u32; 256] = {
        let mut t = [0u32; 256];
        for (i, entry) in t.iter_mut().enumerate() {
            let mut crc = (i as u32) << 24;
            for _ in 0..8 {
                if crc & 0x8000_0000 != 0 {
                    crc = (crc << 1) ^ 0x04C1_1DB7;
                } else {
                    crc <<= 1;
                }
            }
            *entry = crc;
        }
        t
    };
    assert_eq!(
        CRC32_TABLE, runtime_table,
        "compile-time CRC32_TABLE must match a freshly-computed runtime table"
    );
}

// --- Sentinel u8 for continuity counter and PMT version ---

#[test]
#[allow(clippy::assertions_on_constants)]
fn continuity_sentinel_is_not_a_valid_cc_value() {
    // Valid continuity counter values are 0–15 (4-bit field in TS header).
    assert_eq!(CC_UNSET, u8::MAX, "CC_UNSET must be u8::MAX");
    assert!(
        CC_UNSET > 15,
        "CC_UNSET must be out of the valid 0–15 range"
    );
}

#[test]
#[allow(clippy::assertions_on_constants)]
fn pmt_version_sentinel_is_not_a_valid_version() {
    // Valid PMT version_number values are 0–31 (5-bit field in PMT header).
    assert_eq!(PMT_VER_UNSET, u8::MAX, "PMT_VER_UNSET must be u8::MAX");
    assert!(
        PMT_VER_UNSET > 31,
        "PMT_VER_UNSET must be out of the valid 0–31 range"
    );
}

#[test]
fn new_demuxer_pmt_version_is_unset() {
    let dem = TsDemuxer::new();
    assert_eq!(
        dem.pmt_version, PMT_VER_UNSET,
        "freshly constructed TsDemuxer must report PMT_VER_UNSET before any PMT is seen"
    );
}

#[test]
fn new_stream_continuity_is_unset() {
    let mut data = Vec::new();
    data.extend_from_slice(&make_pat_ts_pkt());
    data.extend_from_slice(&make_pmt_ts_pkt(0, &[(0x1B, 0x100u16)]));

    let mut dem = TsDemuxer::new();
    dem.feed(&data);

    assert_eq!(dem.streams.len(), 1, "PMT must add one video stream");
    assert_eq!(
        dem.streams[0].continuity, CC_UNSET,
        "new stream continuity must be CC_UNSET before first TS packet arrives"
    );
}

// --- find_ts_sync / ts_sync_candidate_is_valid direct tests ---

#[test]
fn find_ts_sync_empty_returns_zero_length() {
    assert_eq!(find_ts_sync(&[]), 0);
}

#[test]
fn find_ts_sync_no_sync_byte_returns_len() {
    let data = vec![0x00u8; 200];
    assert_eq!(find_ts_sync(&data), data.len());
}

#[test]
fn find_ts_sync_sync_at_offset_zero_accepted() {
    // One packet followed by non-0x47 — optimistic accept at offset 0
    let mut data = vec![0x00u8; TS_PACKET_SIZE * 2];
    data[0] = TS_SYNC_BYTE;
    data[TS_PACKET_SIZE] = TS_SYNC_BYTE; // confirm at +188
    assert_eq!(find_ts_sync(&data), 0);
}

#[test]
fn find_ts_sync_sync_at_offset_zero_short_buffer_optimistic() {
    // Buffer has only one packet (≤ TS_PACKET_SIZE bytes after candidate) → optimistic accept
    let mut data = vec![0x00u8; TS_PACKET_SIZE];
    data[0] = TS_SYNC_BYTE;
    assert_eq!(
        find_ts_sync(&data),
        0,
        "single-packet buffer must accept offset 0"
    );
}

#[test]
fn find_ts_sync_false_sync_skipped_real_sync_found() {
    // byte[5] = 0x47 but byte[5+188] ≠ 0x47 → rejected.
    // byte[10] = 0x47, +188 and +376 both confirmed → accepted.
    // Buffer is 4 packets so the triple-confirmation path is exercised.
    let mut data = vec![0x00u8; TS_PACKET_SIZE * 4];
    data[5] = TS_SYNC_BYTE; // false candidate — no +188 confirm
    data[10] = TS_SYNC_BYTE;
    data[10 + TS_PACKET_SIZE] = TS_SYNC_BYTE;
    data[10 + TS_PACKET_SIZE * 2] = TS_SYNC_BYTE;
    assert_eq!(find_ts_sync(&data), 10);
}

#[test]
fn find_ts_sync_double_confirmed_sync() {
    // Two consecutive confirming sync bytes (triple-confirmation path)
    let mut data = vec![0x00u8; TS_PACKET_SIZE * 4];
    data[3] = TS_SYNC_BYTE;
    data[3 + TS_PACKET_SIZE] = TS_SYNC_BYTE;
    data[3 + TS_PACKET_SIZE * 2] = TS_SYNC_BYTE;
    assert_eq!(find_ts_sync(&data), 3);
}

#[test]
fn ts_sync_candidate_not_sync_byte_returns_false() {
    let data = [0x00u8; 188];
    assert!(!ts_sync_candidate_is_valid(&data, 0));
}

#[test]
fn ts_sync_candidate_out_of_bounds_returns_false() {
    let data = [0x47u8; 10];
    assert!(!ts_sync_candidate_is_valid(&data, 20)); // candidate > data.len()
}
