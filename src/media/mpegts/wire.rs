pub(super) const TS_PACKET_SIZE: usize = 188;
pub(super) const TS_SYNC_BYTE: u8 = 0x47;
pub(super) const PAT_PID: u16 = 0x0000;
pub(super) const SDT_PID: u16 = 0x0011;
pub(super) const PES_START_CODE: [u8; 3] = [0x00, 0x00, 0x01];

pub(super) fn parse_timestamp(data: &[u8]) -> i64 {
    let b0 = data[0] as i64;
    let b1 = data[1] as i64;
    let b2 = data[2] as i64;
    let b3 = data[3] as i64;
    let b4 = data[4] as i64;

    ((b0 >> 1) & 0x07) << 30 | (b1 << 22) | ((b2 >> 1) << 15) | (b3 << 7) | (b4 >> 1)
}

#[cfg(test)]
pub(super) fn write_timestamp(buf: &mut Vec<u8>, ts: i64, marker: u8) {
    buf.push((marker << 4) | (((ts >> 30) as u8) & 0x07) << 1 | 0x01);
    buf.push(((ts >> 22) & 0xFF) as u8);
    buf.push((((ts >> 15) & 0x7F) as u8) << 1 | 0x01);
    buf.push(((ts >> 7) & 0xFF) as u8);
    buf.push((((ts) & 0x7F) as u8) << 1 | 0x01);
}

pub(super) fn write_timestamp_buf(buf: &mut [u8], ts: i64, marker: u8) {
    buf[0] = (marker << 4) | (((ts >> 30) as u8) & 0x07) << 1 | 0x01;
    buf[1] = ((ts >> 22) & 0xFF) as u8;
    buf[2] = (((ts >> 15) & 0x7F) as u8) << 1 | 0x01;
    buf[3] = ((ts >> 7) & 0xFF) as u8;
    buf[4] = (((ts) & 0x7F) as u8) << 1 | 0x01;
}

pub(super) fn copy_pes_slices(
    dst: &mut [u8],
    hdr: &[u8],
    payload: &[u8],
    offset: usize,
    len: usize,
) {
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

pub(super) fn write_pcr(buf: &mut [u8], ts_90k: i64) {
    let pcr_base = ts_90k.max(0) as u64;
    let pcr_ext: u16 = 0;
    buf[0] = (pcr_base >> 25) as u8;
    buf[1] = (pcr_base >> 17) as u8;
    buf[2] = (pcr_base >> 9) as u8;
    buf[3] = (pcr_base >> 1) as u8;
    buf[4] = ((pcr_base & 1) << 7) as u8 | 0x7E | ((pcr_ext >> 8) as u8 & 0x01);
    buf[5] = pcr_ext as u8;
}

pub(super) fn ts_to_ms(ts_90k: i64) -> i64 {
    // Exact integer arithmetic: 90kHz → ms is ts / 90 (= ts * 1000 / 90000).
    // Using f64 would introduce up to ~45 ms of accumulated drift over a 24-hour
    // stream because f64 has only 53-bit mantissa precision and ts_90k grows to
    // ~7.8e12 for a day-long stream, losing sub-90-tick resolution.
    ts_90k / 90
}

pub(super) fn ms_to_ts(ms: i64) -> i64 {
    ms * 90
}

// Benchmarked crc-fast (PCLMULQDQ) vs table-driven: at our workload sizes
// (12-22 bytes, once per ~100ms), crc-fast is 2.5× slower due to SIMD dispatch
// overhead. Table-driven is zero-dependency, faster at these sizes, and more than
// sufficient for production. See benches/simd_alternatives.rs.
//
// All operations used in the table computation (`<<`, `^`, conditionals) are
// valid in `const fn`, so the table is computed at compile time and placed in
// the binary's read-only data segment — no OnceLock, no runtime init, no atomic.
pub(super) const CRC32_TABLE: [u32; 256] = {
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

pub(super) fn crc32_mpeg2(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;
    for &byte in data {
        let idx = (((crc >> 24) ^ (byte as u32)) & 0xFF) as usize;
        crc = (crc << 8) ^ CRC32_TABLE[idx];
    }
    crc
}
