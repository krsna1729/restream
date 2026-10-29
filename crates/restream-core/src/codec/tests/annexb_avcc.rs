
// -----------------------------------------------------------------------
// annexb_to_avcc_into oracle: streaming implementation must match the
// two-pass split_annexb_nalus reference for all inputs.
// -----------------------------------------------------------------------

/// Reference implementation that uses two intermediate Vecs (the original path).
fn annexb_to_avcc_reference(data: &[u8]) -> Vec<u8> {
    let nalus = split_annexb_nalus(data);
    let mut out = Vec::new();
    for nalu in &nalus {
        if nalu.is_empty() {
            continue;
        }
        let nal_type = nalu[0] & 0x1F;
        if matches!(nal_type, 7..=9) {
            continue;
        }
        out.extend_from_slice(&(nalu.len() as u32).to_be_bytes());
        out.extend_from_slice(nalu);
    }
    out
}

#[test]
fn annexb_to_avcc_into_matches_reference_single_nalu_4byte_sc() {
    let data = [0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB];
    let reference = annexb_to_avcc_reference(&data);
    let mut streaming = Vec::new();
    annexb_to_avcc_into(&data, &mut streaming);
    assert_eq!(streaming, reference);
}

#[test]
fn annexb_to_avcc_into_matches_reference_single_nalu_3byte_sc() {
    let data = [0x00, 0x00, 0x01, 0x41, 0xCC, 0xDD];
    let reference = annexb_to_avcc_reference(&data);
    let mut streaming = Vec::new();
    annexb_to_avcc_into(&data, &mut streaming);
    assert_eq!(streaming, reference);
}

#[test]
fn annexb_to_avcc_into_matches_reference_multiple_nalus_mixed_sc() {
    // 3-byte SC + IDR, 4-byte SC + P-slice, 3-byte SC + P-slice
    let data = [
        0x00, 0x00, 0x01, 0x65, 0x11, 0x22, // IDR (3-byte SC)
        0x00, 0x00, 0x00, 0x01, 0x41, 0x33, 0x44, // P-slice (4-byte SC)
        0x00, 0x00, 0x01, 0x41, 0x55, // P-slice (3-byte SC)
    ];
    let reference = annexb_to_avcc_reference(&data);
    let mut streaming = Vec::new();
    annexb_to_avcc_into(&data, &mut streaming);
    assert_eq!(streaming, reference);
}

#[test]
fn annexb_to_avcc_into_matches_reference_filters_sps_pps_aud() {
    // SPS (7), PPS (8), AUD (9), IDR (5) — only IDR should appear in output
    let data = [
        0x00, 0x00, 0x00, 0x01, 0x67, 0x42, // SPS
        0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, // PPS
        0x00, 0x00, 0x00, 0x01, 0x09, 0xF0, // AUD
        0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80, // IDR
    ];
    let reference = annexb_to_avcc_reference(&data);
    let mut streaming = Vec::new();
    annexb_to_avcc_into(&data, &mut streaming);
    assert_eq!(streaming, reference);
    // Only IDR should remain
    assert_eq!(&streaming[4] & 0x1F, 5);
}

#[test]
fn annexb_to_avcc_into_appends_to_existing_content() {
    let marker = b"MARKER".to_vec();
    let data = [0x00, 0x00, 0x00, 0x01, 0x41, 0xBB];
    let mut out = marker.clone();
    annexb_to_avcc_into(&data, &mut out);
    assert!(out.starts_with(&marker));
    assert!(out.len() > marker.len());
}

#[test]
fn annexb_to_avcc_with_scratch_matches_reference() {
    // Same oracle tests as annexb_to_avcc_into — with_scratch must produce
    // identical output.
    let cases: &[&[u8]] = &[
        &[0x00, 0x00, 0x00, 0x01, 0x65, 0xAA, 0xBB],
        &[0x00, 0x00, 0x01, 0x41, 0xCC, 0xDD],
        &[
            0x00, 0x00, 0x01, 0x65, 0x11, 0x22, 0x00, 0x00, 0x00, 0x01, 0x41, 0x33, 0x44, 0x00,
            0x00, 0x01, 0x41, 0x55,
        ],
        &[
            0x00, 0x00, 0x00, 0x01, 0x67, 0x42, 0x00, 0x00, 0x00, 0x01, 0x68, 0xCE, 0x00, 0x00,
            0x00, 0x01, 0x09, 0xF0, 0x00, 0x00, 0x00, 0x01, 0x65, 0x88, 0x80,
        ],
    ];
    for data in cases {
        let reference = annexb_to_avcc_reference(data);
        let mut scratch_out = Vec::new();
        let mut sc = Vec::new();
        annexb_to_avcc_with_scratch(data, &mut scratch_out, &mut sc);
        assert_eq!(
            scratch_out,
            reference,
            "with_scratch mismatch for input len={}",
            data.len()
        );
    }
}

#[test]
fn annexb_to_avcc_with_scratch_reuses_sc_buffer() {
    let data = [0x00, 0x00, 0x00, 0x01, 0x41, 0xBB];
    let mut out = Vec::new();
    let mut sc: Vec<(usize, usize)> = Vec::new();
    // First call: sc grows
    annexb_to_avcc_with_scratch(&data, &mut out, &mut sc);
    let cap_after_first = sc.capacity();
    out.clear();
    // Second call: sc should reuse its allocation (capacity unchanged or larger)
    annexb_to_avcc_with_scratch(&data, &mut out, &mut sc);
    assert!(
        sc.capacity() >= cap_after_first,
        "sc scratch should not shrink"
    );
}

// --- Edge-case tests ---

#[test]
fn parse_avcc_config_truncated_input() {
    assert_eq!(parse_avcc_config(&[]), (4, Vec::new()));
    assert_eq!(parse_avcc_config(&[1, 66, 0, 30]), (4, Vec::new()));
    // Below the 8-byte header floor entirely; body loop never runs.
    assert_eq!(
        parse_avcc_config(&[1, 66, 0, 30, 0xFF, 0xE1]),
        (4, Vec::new())
    );
}

#[test]
fn parse_avcc_config_zero_sps_pps() {
    // 8 bytes: past the header floor, so this exercises the real
    // num_sps/num_pps == 0 loop bodies rather than the length<8 gate.
    let config = [1, 66, 0, 30, 0xFF, 0x00, 0x00, 0x00];
    let (nls, annexb) = parse_avcc_config(&config);
    assert_eq!(nls, 4);
    assert!(annexb.is_empty());
}

#[test]
fn parse_avcc_config_sps_ok_but_missing_pps_count_byte_yields_no_partial_state() {
    // num_sps = 1, one valid 4-byte SPS, then the buffer ends before the
    // mandatory numPPS byte. The old implementation returned the SPS
    // alone; a decoder handed SPS with no PPS cannot decode either, so
    // the parser must fail closed and cache nothing.
    let config = [
        1, 66, 0, 30, 0xFF, // header (nalu_len_size = 4)
        0xE1, // num_sps = 1
        0x00, 0x04, // SPS length = 4
        0x67, 0x42, 0x00, 0x1E, // SPS body
    ];
    assert_eq!(parse_avcc_config(&config), (4, Vec::new()));
}

#[test]
fn parse_avcc_config_sps_ok_but_pps_length_truncated_yields_no_partial_state() {
    // SPS parses cleanly, numPPS = 1, but the PPS length/body never
    // arrives. Must not leak the SPS-only prefix.
    let config = [
        1, 66, 0, 30, 0xFF, // header
        0xE1, // num_sps = 1
        0x00, 0x04, // SPS length = 4
        0x67, 0x42, 0x00, 0x1E, // SPS body
        0x01, // num_pps = 1, then buffer ends
    ];
    assert_eq!(parse_avcc_config(&config), (4, Vec::new()));
}

#[test]
fn parse_avcc_config_max_declared_length_with_tiny_buffer_rejected() {
    // SPS declares a length of 0xFFFF (max u16) but only 2 bytes of body
    // actually follow; must reject without allocating for the declared
    // length or panicking on the out-of-bounds slice.
    let config = [
        1, 66, 0, 30, 0xFF, // header
        0xE1, // num_sps = 1
        0xFF, 0xFF, // SPS length = 65535
        0xAA, 0xBB, // only 2 bytes actually present
    ];
    assert_eq!(parse_avcc_config(&config), (4, Vec::new()));
}

proptest! {
    #[test]
    fn parse_avcc_config_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
        let _ = parse_avcc_config(&bytes);
    }

    #[test]
    fn parse_avcc_config_truncation_always_fails_closed(
        header in any::<u8>(),
        sps_bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..16), 0..3),
        pps_bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..16), 0..3),
    ) {
        let mut config = vec![1u8, 66, 0, 30, header];
        config.push(0xE0 | (sps_bodies.len() as u8 & 0x1F));
        for sps in &sps_bodies {
            config.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            config.extend_from_slice(sps);
        }
        config.push(pps_bodies.len() as u8);
        for pps in &pps_bodies {
            config.extend_from_slice(&(pps.len() as u16).to_be_bytes());
            config.extend_from_slice(pps);
        }

        let mut expected = Vec::new();
        for sps in &sps_bodies {
            expected.extend_from_slice(&[0, 0, 0, 1]);
            expected.extend_from_slice(sps);
        }
        for pps in &pps_bodies {
            expected.extend_from_slice(&[0, 0, 0, 1]);
            expected.extend_from_slice(pps);
        }
        let (_, full_annexb) = parse_avcc_config(&config);
        prop_assert_eq!(full_annexb, expected);

        // Any strict prefix of a well-formed buffer must fail closed
        // (empty annexb), never a partial SPS/PPS prefix.
        for cut in 0..config.len() {
            let (_, partial) = parse_avcc_config(&config[..cut]);
            prop_assert!(partial.is_empty(), "truncated at {cut} produced non-empty output");
        }
    }
}
