// -----------------------------------------------------------------------
// Round trips between the AVCC/hvcC builders and the parsers that read
// their output back. The builders run on untrusted Annex-B from SRT ingest;
// the parsers run on untrusted FLV from RTMP ingest.
// -----------------------------------------------------------------------

/// A NALU body with no zero bytes, so it can never contain a start code or
/// an emulation-prevention sequence and survives Annex-B framing unchanged.
fn nalu_body(len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<u8>> {
    prop::collection::vec(1u8..=255, len)
}

fn h264_nalu(header: u8, len: std::ops::Range<usize>) -> impl Strategy<Value = Vec<u8>> {
    nalu_body(len).prop_map(move |mut body| {
        body.insert(0, header);
        body
    })
}

fn annexb(nalus: &[Vec<u8>]) -> Vec<u8> {
    let mut out = Vec::new();
    for nalu in nalus {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nalu);
    }
    out
}

/// Reads one hvcC NALU array (`array_completeness|type`, `numNalus = 1`,
/// `nalUnitLength`, NALU) at `offset`; returns `(type, nalu, next_offset)`.
fn read_hvcc_array(record: &[u8], offset: usize) -> (u8, &[u8], usize) {
    let nal_type = record[offset] & 0x3F;
    assert_eq!(
        u16::from_be_bytes([record[offset + 1], record[offset + 2]]),
        1
    );
    let len = usize::from(u16::from_be_bytes([record[offset + 3], record[offset + 4]]));
    let start = offset + 5;
    (nal_type, &record[start..start + len], start + len)
}

/// SPS/PPS lists the AVCDecoderConfigurationRecord cannot express (more
/// than 31 SPS, more than 255 PPS, or a NALU longer than 65,535 bytes) are
/// refused. They used to be written with wrapped counts or lengths, so the
/// record sent to every RTMP destination no longer matched its own bytes.
#[test]
fn avcc_sequence_header_refuses_parameter_sets_it_cannot_encode() {
    let sps = [0x67, 0x64, 0x00, 0x28, 0xAC];
    let pps = [0x68, 0xEE, 0x3C, 0x80];

    let mut long_sps = vec![0x67, 0x64, 0x00, 0x28];
    long_sps.resize(usize::from(u16::MAX) + 1, 0xAC);
    assert!(build_avcc_sequence_header(&annexb(&[long_sps, pps.to_vec()])).is_none());

    let mut many_sps = vec![sps.to_vec(); 32];
    many_sps.push(pps.to_vec());
    assert!(build_avcc_sequence_header(&annexb(&many_sps)).is_none());

    let mut many_pps = vec![sps.to_vec()];
    many_pps.extend(std::iter::repeat_n(pps.to_vec(), 256));
    assert!(build_avcc_sequence_header(&annexb(&many_pps)).is_none());

    // The largest expressible record still builds and reads back.
    let mut max = vec![sps.to_vec(); 31];
    max.extend(std::iter::repeat_n(pps.to_vec(), 255));
    let header = build_avcc_sequence_header(&annexb(&max)).expect("31 SPS, 255 PPS");
    assert_eq!(parse_avcc_config(&header[5..]).1, annexb(&max));
}

/// A NALU length width outside 1..=4 converts nothing. Width 0 used to read
/// a four-byte length from a shorter body and panic on the index.
#[test]
fn avcc_to_annexb_ignores_an_impossible_length_width() {
    assert!(avcc_to_annexb(&[0, 1], 0).is_empty());
    assert!(avcc_to_annexb(&[0, 0, 0, 0, 0, 0, 0, 1, 0xAA], 8).is_empty());
    assert_eq!(avcc_to_annexb(&[1, 0xAA], 1), [0, 0, 0, 1, 0xAA]);
}

proptest! {
    /// `avcc_record` reads back exactly the lists a well-formed record
    /// carries, and every strict prefix of the record is `None`: never a
    /// partial SPS/PPS list. RTMP ingest, FLV → TS and the fMP4 sample entry
    /// all read the record through it.
    #[test]
    fn avcc_record_reads_whole_records_and_rejects_every_prefix(
        header in any::<[u8; 5]>(),
        sps_bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..16), 0..3),
        pps_bodies in prop::collection::vec(prop::collection::vec(any::<u8>(), 0..16), 0..3),
    ) {
        let mut data = header.to_vec();
        data.push(0xE0 | sps_bodies.len() as u8);
        for sps in &sps_bodies {
            data.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            data.extend_from_slice(sps);
        }
        data.push(pps_bodies.len() as u8);
        for pps in &pps_bodies {
            data.extend_from_slice(&(pps.len() as u16).to_be_bytes());
            data.extend_from_slice(pps);
        }

        let record = avcc_record(&data).expect("well-formed record");
        prop_assert_eq!(record.length_size_minus_one, header[4] & 0x03);
        prop_assert_eq!(&record.sps, &sps_bodies);
        prop_assert_eq!(&record.pps, &pps_bodies);
        for cut in 0..data.len() {
            prop_assert!(avcc_record(&data[..cut]).is_none(), "prefix {cut} parsed");
        }
    }

    #[test]
    fn avcc_record_never_panics(bytes in prop::collection::vec(any::<u8>(), 0..128)) {
        let _ = avcc_record(&bytes);
    }

    /// Every AVC sequence header Restream builds from Annex-B parameter sets
    /// reads back, through the same parser RTMP ingest uses, as exactly the
    /// SPS and PPS it was built from, in order.
    #[test]
    fn avcc_sequence_header_round_trips_the_parameter_sets(
        sps in prop::collection::vec(h264_nalu(0x67, 3..48), 1..4),
        pps in prop::collection::vec(h264_nalu(0x68, 1..24), 0..4),
        slice in h264_nalu(0x65, 1..32),
    ) {
        let mut input = sps.clone();
        input.extend(pps.iter().cloned());
        input.push(slice);

        let header = build_avcc_sequence_header(&annexb(&input)).expect("SPS present");

        prop_assert_eq!(&header[..5], &[0x17, 0, 0, 0, 0]);
        let (nalu_len_size, sets) = parse_avcc_config(&header[5..]);
        prop_assert_eq!(nalu_len_size, 4);
        let mut expected = sps.clone();
        expected.extend(pps.iter().cloned());
        prop_assert_eq!(sets, annexb(&expected));
    }

    /// Annex-B → AVCC → Annex-B keeps every non-parameter-set NALU, in order
    /// and byte for byte (SPS, PPS and AUD are dropped by design).
    #[test]
    fn annexb_avcc_round_trip_keeps_every_coded_nalu(
        nalus in prop::collection::vec(
            prop::sample::select(vec![0x65u8, 0x41, 0x06, 0x67, 0x68, 0x09])
                .prop_flat_map(|header| h264_nalu(header, 1..64)),
            1..8,
        ),
    ) {
        let avcc = annexb_to_avcc(&annexb(&nalus));
        let back = avcc_to_annexb(&avcc, 4);

        let expected: Vec<&[u8]> = nalus
            .iter()
            .map(Vec::as_slice)
            .filter(|nalu| !matches!(nalu[0] & 0x1F, 7..=9))
            .collect();
        prop_assert_eq!(split_annexb_nalus(&back), expected);
    }

    /// The Enhanced RTMP hvcC record carries the first VPS, SPS and PPS of
    /// the input, byte for byte, and the chroma/bit-depth fields of the SPS.
    #[test]
    fn hevc_sequence_header_carries_the_parameter_sets(
        chroma in 0u64..=3,
        bit_depth_minus8 in 0u64..=7,
        vps_body in nalu_body(1..32),
        pps_body in nalu_body(1..32),
        sei_body in nalu_body(1..16),
    ) {
        let vps = [vec![0x40, 0x01], vps_body].concat();
        let sps = minimal_hevc_sps_nalu(chroma, bit_depth_minus8);
        let pps = [vec![0x44, 0x01], pps_body].concat();
        let sei = [vec![0x4E, 0x01], sei_body].concat();
        // Order and noise on purpose: the record order is fixed VPS/SPS/PPS.
        let input = annexb(&[sei, pps.clone(), sps.clone(), vps.clone()]);

        let header = build_hevc_enhanced_rtmp_sequence_header(&input).expect("hvcC");

        prop_assert_eq!(&header[..5], b"\x80hvc1");
        let record = &header[5..];
        prop_assert_eq!(record[0], 1, "configurationVersion");
        prop_assert_eq!(u64::from(record[16] & 0x03), chroma);
        prop_assert_eq!(u64::from(record[17] & 0x07), bit_depth_minus8);
        prop_assert_eq!(u64::from(record[18] & 0x07), bit_depth_minus8);
        prop_assert_eq!(record[21] & 0x03, 3, "lengthSizeMinusOne");
        prop_assert_eq!(record[22], 3, "numOfArrays");
        let (t0, n0, next) = read_hvcc_array(record, 23);
        let (t1, n1, next) = read_hvcc_array(record, next);
        let (t2, n2, end) = read_hvcc_array(record, next);
        prop_assert_eq!((t0, n0), (32, vps.as_slice()));
        prop_assert_eq!((t1, n1), (33, sps.as_slice()));
        prop_assert_eq!((t2, n2), (34, pps.as_slice()));
        prop_assert_eq!(end, record.len());
    }

    /// An Enhanced RTMP HEVC coded frame carries every non-parameter-set
    /// NALU as a 4-byte length-prefixed unit, the frame type, and the signed
    /// 24-bit composition offset (CodedFramesX when it is zero).
    #[test]
    fn hevc_coded_frame_round_trips_nalus_and_composition(
        nalus in prop::collection::vec(
            (prop::sample::select(vec![1u8, 19, 21, 32, 33, 34, 35, 39]), nalu_body(1..48))
                .prop_map(|(nal_type, body)| [vec![nal_type << 1, 0x01], body].concat()),
            1..8,
        ),
        is_keyframe in any::<bool>(),
        composition in -(1i32 << 23)..(1i32 << 23),
    ) {
        let mut out = Vec::new();
        let has_vcl =
            hevc_video_for_enhanced_rtmp_with_composition_into(&annexb(&nalus), is_keyframe, composition, &mut out);

        let kept: Vec<&[u8]> = nalus
            .iter()
            .map(Vec::as_slice)
            .filter(|nalu| !matches!((nalu[0] >> 1) & 0x3F, 32..=35))
            .collect();
        prop_assert_eq!(has_vcl, kept.iter().any(|nalu| (nalu[0] >> 1) & 0x3F <= 31));
        let frame_type = if is_keyframe { 1 } else { 2 };
        prop_assert_eq!(out[0] >> 4, 0x8 | frame_type);
        prop_assert_eq!(&out[1..5], b"hvc1");
        let mut offset = 5;
        if composition == 0 {
            prop_assert_eq!(out[0] & 0x0F, 3, "CodedFramesX");
        } else {
            prop_assert_eq!(out[0] & 0x0F, 1, "CodedFrames");
            let raw = (i32::from(out[5]) << 16) | (i32::from(out[6]) << 8) | i32::from(out[7]);
            prop_assert_eq!((raw << 8) >> 8, composition);
            offset = 8;
        }
        let mut units = Vec::new();
        while offset < out.len() {
            let len = u32::from_be_bytes(out[offset..offset + 4].try_into().unwrap()) as usize;
            units.push(&out[offset + 4..offset + 4 + len]);
            offset += 4 + len;
        }
        prop_assert_eq!(units, kept);
    }
}
