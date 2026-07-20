#[test]
fn parse_timestamp_round_trip() {
    let ts: i64 = 132000; // 90kHz timestamp
    let mut buf = Vec::new();
    write_timestamp(&mut buf, ts, 0x02);
    let parsed = parse_timestamp(&buf);
    assert_eq!(parsed, ts);
}

#[test]
fn parse_timestamp_large_value() {
    let ts: i64 = 8_589_934_591; // max 33-bit value
    let mut buf = Vec::new();
    write_timestamp(&mut buf, ts, 0x03);
    let parsed = parse_timestamp(&buf);
    assert_eq!(parsed, ts);
}

#[test]
fn ts_ms_conversion() {
    assert_eq!(ts_to_ms(90000), 1000);
    assert_eq!(ts_to_ms(0), 0);
    assert_eq!(ms_to_ts(1000), 90000);
    assert_eq!(ms_to_ts(0), 0);
}

#[test]
fn ts_to_ms_no_float_drift() {
    // Verify no floating-point drift at 24-hour scale.
    // At 90 kHz, 24 hours = 24*3600*90000 = 7_776_000_000 ticks.
    // f64 has 53-bit mantissa; at this scale each ULP is ~1024 ticks = ~11 ms.
    // Integer division: ts / 90 must give exact ms with no drift.
    let day_90k: i64 = 24 * 3600 * 90_000;
    let day_ms: i64 = 24 * 3600 * 1000;
    assert_eq!(
        ts_to_ms(day_90k),
        day_ms,
        "ts_to_ms must be exact for 24-hour timestamps (no f64 drift)"
    );
    // Also verify round-trip for a one-hour mark
    let hour_90k: i64 = 3600 * 90_000;
    let hour_ms: i64 = 3600 * 1000;
    assert_eq!(ts_to_ms(hour_90k), hour_ms);
}

#[test]
fn crc32_known_value() {
    // PAT with known CRC
    let data = [
        0x00, 0xB0, 0x0D, 0x00, 0x01, 0xC1, 0x00, 0x00, 0x00, 0x01, 0xE1, 0x00,
    ];
    let crc = crc32_mpeg2(&data);
    assert_ne!(crc, 0); // Just verify it produces a non-trivial value
    // The expected CRC32/MPEG-2 of this PAT payload is 0xE8F95E7D
    assert_eq!(crc, 0xE8F95E7D);
}

#[test]
fn crc32_bit_at_a_time_equivalence() {
    // Local reference implementation of the bit-at-a-time algorithm
    let reference_crc = |data: &[u8]| {
        let mut crc = 0xFFFF_FFFFu32;
        for &byte in data {
            crc ^= (byte as u32) << 24;
            for _ in 0..8 {
                if crc & 0x8000_0000 != 0 {
                    crc = (crc << 1) ^ 0x04C1_1DB7;
                } else {
                    crc <<= 1;
                }
            }
        }
        crc
    };

    // Test with different sizes and randomized inputs
    let mut rng = 12345u32;
    let mut next_random_byte = || {
        // simple LCG generator
        rng = rng.wrapping_mul(1664525).wrapping_add(1013904223);
        (rng >> 24) as u8
    };

    for size in [0, 1, 4, 12, 188, 1024, 4096] {
        for _ in 0..10 {
            let data: Vec<u8> = (0..size).map(|_| next_random_byte()).collect();
            let ref_val = reference_crc(&data);
            let table_val = crc32_mpeg2(&data);
            assert_eq!(
                table_val, ref_val,
                "Failed equivalence test at size {}",
                size
            );
        }
    }
}

